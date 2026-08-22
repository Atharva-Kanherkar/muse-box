mod state;
pub mod voice;

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    services::{ServeDir, ServeFile},
};

use crate::{
    account::{AccountId, AccountRegistry, OwnerMarker},
    error::AppError,
    session::{self, SessionStore},
    spotify::SpotifyClient,
};

/// Everything shared across every account, needed to run the OAuth entry
/// points and resolve which account a request belongs to. Per-account state
/// (`SpotifyClient`, `StateHub`, `TasteIndex`) lives behind `registry`
/// instead, resolved per request — see [`AccountRuntime`].
#[derive(Clone)]
pub(crate) struct AppState {
    /// Runs the OAuth dance only: `authorization_url` and
    /// `exchange_code_for_tokens`. Shared across every visitor signing in
    /// concurrently, and deliberately never the thing that persists a token —
    /// see that method's doc comment.
    oauth: SpotifyClient,
    registry: Arc<AccountRegistry>,
    owner: Arc<OwnerMarker>,
    sessions: Arc<SessionStore>,
    /// False for local http development, where a Secure cookie would be dropped.
    secure_cookies: bool,
}

#[derive(Clone)]
struct Credentials {
    device_api_token: String,
    sessions: Arc<SessionStore>,
    registry: Arc<AccountRegistry>,
    owner: Arc<OwnerMarker>,
}

/// Everything the router needs. A struct rather than a parameter list: eight
/// positional arguments, several of them `Arc`s and one a bare `bool`, is a
/// swap waiting to happen.
pub struct RouterConfig {
    pub oauth: SpotifyClient,
    pub device_api_token: String,
    pub registry: Arc<AccountRegistry>,
    pub owner: Arc<OwnerMarker>,
    pub voice_model: Arc<dyn voice::VoiceModel>,
    pub sessions: Arc<SessionStore>,
    /// False for local http development, where a Secure cookie is dropped.
    pub secure_cookies: bool,
    /// Directory of the built web client, served from this same origin.
    pub client_root: std::path::PathBuf,
}

pub fn router(config: RouterConfig) -> Router {
    let RouterConfig {
        oauth,
        device_api_token,
        registry,
        owner,
        voice_model,
        sessions,
        secure_cookies,
        client_root,
    } = config;
    let credentials = Credentials {
        device_api_token: device_api_token.clone(),
        sessions: sessions.clone(),
        registry: registry.clone(),
        owner: owner.clone(),
    };
    let state = AppState {
        oauth,
        registry,
        owner,
        sessions,
        secure_cookies,
    };
    // The voice routes' only shared, router-level state is the model: every
    // other piece they need (Spotify, the state hub, taste) comes from the
    // account the auth middleware resolved, via the `Extension` it inserts.
    let voice_route = Router::new()
        .route("/voice", post(voice::post_voice))
        .route("/control", post(voice::post_control))
        .route("/command", post(voice::post_command))
        .with_state(voice_model);
    let protected = Router::new()
        .route("/health", get(health))
        .route("/state", get(state::get_state))
        .merge(voice_route)
        .route_layer(middleware::from_fn_with_state(
            credentials,
            require_credentials,
        ));

    // Serving the client from this same origin is what makes a plain
    // SameSite=Lax cookie work: a separate frontend deployment would be
    // cross-site, and browsers are actively dropping those cookies.
    let client =
        ServeDir::new(&client_root).fallback(ServeFile::new(client_root.join("index.html")));

    Router::new()
        .route("/auth/spotify", get(start_spotify_auth))
        .route("/auth/spotify/callback", get(spotify_callback))
        // Unauthenticated liveness probe. `/health` stays bearer-protected as
        // its own issue locked it, but a platform health check cannot send a
        // token, and pointing one at `/health` fails every deploy with 401.
        .route("/healthz", get(healthz))
        .merge(protected)
        .with_state(state)
        .fallback_service(client)
}

/// Browser preflight support. Without this every cross-origin `fetch` from a
/// web frontend fails before it is sent, because the `Authorization` header
/// makes each request preflighted.
pub fn cors_layer(allowed_origins: &[String]) -> CorsLayer {
    let origins = if allowed_origins.is_empty() {
        AllowOrigin::any()
    } else {
        AllowOrigin::list(
            allowed_origins
                .iter()
                .filter_map(|origin| HeaderValue::from_str(origin).ok()),
        )
    };
    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

/// Router for tests: a single pre-warmed "test-owner" account already holding
/// `spotify`/`state_hub`, an empty taste index, a throwaway session store, and
/// insecure cookies — so test sites only name what they actually vary. The
/// same `spotify` also backs the OAuth entry points, which no caller of this
/// helper exercises.
#[cfg(test)]
pub(crate) async fn test_router_with(
    spotify: SpotifyClient,
    device_api_token: impl Into<String>,
    state_hub: Arc<crate::state::StateHub>,
    voice_model: Arc<dyn voice::VoiceModel>,
) -> Router {
    let owner_id = AccountId::new("test-owner");
    let registry = Arc::new(AccountRegistry::new(
        crate::account::RegistryConfig::for_test(),
    ));
    registry
        .insert_for_test(crate::account::AccountRuntime::for_test(
            owner_id.clone(),
            spotify.clone(),
            state_hub,
            Arc::new(crate::taste::TasteIndex::new(
                "test-key",
                std::path::PathBuf::from("unused"),
            )),
        ))
        .await;
    router(RouterConfig {
        oauth: spotify,
        device_api_token: device_api_token.into(),
        registry,
        owner: Arc::new(OwnerMarker::for_test(owner_id)),
        voice_model,
        sessions: Arc::new(SessionStore::new(std::path::PathBuf::from("unused"))),
        secure_cookies: false,
        client_root: std::path::PathBuf::from("web/dist"),
    })
}

async fn start_spotify_auth(State(state): State<AppState>) -> Result<Response, AppError> {
    let location = state.oauth.authorization_url().await?;
    Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
}

async fn spotify_callback(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let code = query
        .get("code")
        .ok_or_else(|| AppError::BadRequest("missing authorization code".to_string()))?;
    let oauth_state = query
        .get("state")
        .ok_or_else(|| AppError::BadRequest("missing OAuth state".to_string()))?;

    let tokens = state
        .oauth
        .exchange_code_for_tokens(code, oauth_state)
        .await?;
    let account_id = AccountId::new(
        state
            .oauth
            .user_for_access_token(&tokens.access_token)
            .await?,
    );
    let runtime = state.registry.get_or_create(&account_id).await;
    runtime.spotify.adopt_tokens(tokens).await?;
    // Whoever completes this first, on a fresh install, becomes the account
    // the hardware bearer token resolves to. A no-op for every later login.
    state.owner.claim(&account_id).await?;

    // The authorization a person already has to complete becomes the browser
    // login, so there is nothing else to set up. The device token is no longer
    // shown here: hardware reads it from configuration, and putting a secret on
    // a page invites it into screenshots and history.
    let handle = state.sessions.issue(account_id).await?;
    let cookie = session::set_cookie_value(&handle, state.secure_cookies);
    Ok((
        StatusCode::FOUND,
        [
            (header::SET_COOKIE, cookie),
            (header::LOCATION, "/".to_string()),
        ],
    )
        .into_response())
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Accepts either credential, because two very different clients call this API.
///
/// Hardware carries the device token in a header: it cannot do OAuth and has
/// nowhere to keep a cookie. A browser carries an `HttpOnly` session cookie and
/// must never hold the device token, since page JavaScript is public.
///
/// Either way, resolving *which* account the request belongs to happens here,
/// once: the bearer path always resolves to the owner account, the cookie path
/// to whichever account signed that session in. The resolved
/// `Arc<AccountRuntime>` is stashed as a request extension so every downstream
/// handler reads it without repeating this lookup.
async fn require_credentials(
    State(credentials): State<Credentials>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let headers = request.headers();

    let bearer_ok = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_credential)
        .is_some_and(|token| {
            constant_time_eq(token.as_bytes(), credentials.device_api_token.as_bytes())
        });

    let account_id = if bearer_ok {
        // Matches the device token but nobody has ever completed OAuth on
        // this install yet: there is no account to serve state for.
        credentials.owner.get().await
    } else {
        match headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(session::session_from_cookie_header)
        {
            Some(handle) => credentials.sessions.account_for(handle).await,
            None => None,
        }
    };

    let Some(account_id) = account_id else {
        return Err(AppError::Unauthorized);
    };

    let runtime = credentials.registry.get_or_create(&account_id).await;
    request.extensions_mut().insert(runtime);

    Ok(next.run(request).await.into_response())
}

/// Splits the credential out of an `Authorization` header. RFC 7235 makes the
/// scheme case-insensitive, so an ESP32 that sends `bearer` must still work.
fn bearer_credential(value: &str) -> Option<&str> {
    let (scheme, credential) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| credential.trim_start())
}

/// Compares in time independent of how many leading bytes match, so a caller
/// cannot probe the token byte by byte.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        routing::post,
    };
    use tower::ServiceExt;

    use crate::{
        account::{AccountRuntime, RegistryConfig},
        spotify::SpotifyConfig,
        state::StateHub,
    };

    use super::*;

    fn test_spotify(path: PathBuf) -> SpotifyClient {
        SpotifyClient::new(SpotifyConfig {
            client_id: "client-id".to_string(),
            client_secret: "client-secret".to_string(),
            redirect_uri: "http://localhost/auth/spotify/callback".to_string(),
            token_store_path: path,
        })
    }

    async fn test_router(spotify: SpotifyClient, token: &str) -> Router {
        test_router_with(
            spotify,
            token,
            Arc::new(StateHub::new()),
            Arc::new(voice::FailingVoiceModel),
        )
        .await
    }

    #[tokio::test]
    async fn either_a_session_cookie_or_the_device_token_gets_in() {
        // Two clients, two credentials: hardware carries the token in a header,
        // a browser carries an HttpOnly cookie and never holds the token. The
        // cookie names a different account than the bearer's owner, so this
        // also proves the two paths resolve independently.
        let sessions = Arc::new(SessionStore::new(
            tempfile::tempdir().unwrap().path().join("sessions.json"),
        ));
        let owner_id = AccountId::new("owner-account");
        let browser_id = AccountId::new("browser-account");
        let handle = sessions.issue(browser_id).await.expect("session");
        let registry = Arc::new(AccountRegistry::new(RegistryConfig::for_test()));
        registry
            .insert_for_test(AccountRuntime::for_test(
                owner_id.clone(),
                test_spotify(PathBuf::from("unused")),
                Arc::new(StateHub::new()),
                Arc::new(crate::taste::TasteIndex::new("k", PathBuf::from("unused"))),
            ))
            .await;
        let app = router(RouterConfig {
            oauth: test_spotify(PathBuf::from("unused")),
            device_api_token: "device-token".to_string(),
            registry,
            owner: Arc::new(OwnerMarker::for_test(owner_id)),
            voice_model: Arc::new(voice::FailingVoiceModel),
            sessions,
            secure_cookies: false,
            client_root: PathBuf::from("web/dist"),
        });

        /// Case name, optional credential header, expected status.
        type Case = (&'static str, Option<(&'static str, String)>, StatusCode);
        let cases: [Case; 5] = [
            ("no credential", None, StatusCode::UNAUTHORIZED),
            (
                "hardware bearer",
                Some(("authorization", "Bearer device-token".to_string())),
                StatusCode::OK,
            ),
            (
                "browser cookie",
                Some(("cookie", format!("muse_session={handle}"))),
                StatusCode::OK,
            ),
            (
                "cookie among others",
                Some(("cookie", format!("ab=1; muse_session={handle}; cd=2"))),
                StatusCode::OK,
            ),
            (
                "forged cookie",
                Some(("cookie", "muse_session=not-a-real-handle".to_string())),
                StatusCode::UNAUTHORIZED,
            ),
        ];

        for (name, credential, expected) in cases {
            let mut request = Request::builder().uri("/health");
            if let Some((header_name, value)) = credential {
                request = request.header(header_name, value);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).expect("request"))
                .await
                .expect("response");
            assert_eq!(response.status(), expected, "case: {name}");
        }
    }

    #[tokio::test]
    async fn cors_preflight_allows_authorized_cross_origin_calls() {
        // A browser sends OPTIONS before any request carrying Authorization.
        // Without the layer this 405s and the real request is never sent.
        let app = test_router(test_spotify(PathBuf::from("unused")), "right-token")
            .await
            .layer(cors_layer(&[]));
        let response = app
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/state")
                    .header(header::ORIGIN, "http://localhost:5173")
                    .header("access-control-request-method", "GET")
                    .header("access-control-request-headers", "authorization")
                    .body(Body::empty())
                    .expect("preflight"),
            )
            .await
            .expect("response");

        assert!(response.status().is_success(), "{:?}", response.status());
        let headers = response.headers();
        assert_eq!(
            headers
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some("*")
        );
        let allowed = headers
            .get("access-control-allow-headers")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(allowed.contains("authorization"), "{allowed}");
    }

    #[tokio::test]
    async fn cors_restricts_to_configured_origins_when_set() {
        let app = test_router(test_spotify(PathBuf::from("unused")), "right-token")
            .await
            .layer(cors_layer(&["https://box.example".to_string()]));
        let response = app
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/state")
                    .header(header::ORIGIN, "https://evil.example")
                    .header("access-control-request-method", "GET")
                    .body(Body::empty())
                    .expect("preflight"),
            )
            .await
            .expect("response");
        // An origin outside the list gets no allow-origin header, so the
        // browser blocks the response.
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none(),
            "unlisted origin must not be allowed"
        );
    }

    #[tokio::test]
    async fn healthz_needs_no_token_but_health_still_does() {
        let app = test_router(test_spotify(PathBuf::from("unused")), "right-token").await;

        let open = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(open.status(), StatusCode::OK);

        // The bearer-protected probe is unchanged, as its own contract locked it.
        let closed = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(closed.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bearer_middleware_rejects_missing_and_wrong_tokens() {
        let app = test_router(test_spotify(PathBuf::from("unused")), "right-token").await;

        for authorization in [None, Some("Bearer wrong-token")] {
            let mut request = Request::builder().uri("/health");
            if let Some(value) = authorization {
                request = request.header(header::AUTHORIZATION, value);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).expect("request"))
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body = to_bytes(response.into_body(), 1024)
                .await
                .expect("error body");
            assert_eq!(body.as_ref(), br#"{"error":"unauthorized"}"#);
        }

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header(header::AUTHORIZATION, "Bearer right-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bearer_scheme_is_case_insensitive_and_credential_must_match_exactly() {
        assert_eq!(bearer_credential("bearer tok"), Some("tok"));
        assert_eq!(bearer_credential("BEARER tok"), Some("tok"));
        assert_eq!(bearer_credential("Bearer  tok"), Some("tok"));
        assert_eq!(bearer_credential("Basic tok"), None);
        assert_eq!(bearer_credential("Bearer"), None);

        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"sane"));
        assert!(!constant_time_eq(b"short", b"shorter"));

        // A lowercase scheme from a hand-rolled device client must be accepted.
        let app = test_router(test_spotify(PathBuf::from("unused")), "right-token").await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header(header::AUTHORIZATION, "bearer right-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bearer_resolves_to_the_owner_account_even_before_anyone_logs_in() {
        // A fresh install: nobody has ever completed OAuth, so there is no
        // owner yet. The device token still matches, but there is no account
        // to serve, so this must 401 rather than panicking or inventing one.
        let registry = Arc::new(AccountRegistry::new(RegistryConfig::for_test()));
        let owner = Arc::new(OwnerMarker::for_test_unclaimed());
        let app = router(RouterConfig {
            oauth: test_spotify(PathBuf::from("unused")),
            device_api_token: "device-token".to_string(),
            registry,
            owner,
            voice_model: Arc::new(voice::FailingVoiceModel),
            sessions: Arc::new(SessionStore::new(PathBuf::from("unused"))),
            secure_cookies: false,
            client_root: PathBuf::from("web/dist"),
        });
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header(header::AUTHORIZATION, "Bearer device-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn callback_rejects_bad_state_without_token_request() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (token_url, api_base_url) = spawn_token_server(calls.clone()).await;
        let spotify = SpotifyClient::with_all_test_endpoints(
            SpotifyConfig {
                client_id: "client-id".to_string(),
                client_secret: "client-secret".to_string(),
                redirect_uri: "http://localhost/auth/spotify/callback".to_string(),
                token_store_path: PathBuf::from("unused"),
            },
            "https://accounts.spotify.com/authorize".to_string(),
            token_url,
            api_base_url,
        );
        let app = test_router(spotify, "device-token").await;

        for uri in [
            "/auth/spotify/callback?code=code",
            "/auth/spotify/callback?code=code&state=unknown",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = to_bytes(response.into_body(), 1024)
                .await
                .expect("error body");
            let json: Value = serde_json::from_slice(&body).expect("JSON error body");
            assert!(json.get("error").and_then(Value::as_str).is_some());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn callback_signs_the_browser_in_without_revealing_the_token() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let calls = Arc::new(AtomicUsize::new(0));
        let (token_url, api_base_url) = spawn_token_server(calls.clone()).await;
        let spotify = SpotifyClient::with_all_test_endpoints(
            SpotifyConfig {
                client_id: "client-id".to_string(),
                client_secret: "client-secret".to_string(),
                redirect_uri: "http://localhost/auth/spotify/callback".to_string(),
                token_store_path: directory.path().join("spotify.json"),
            },
            "https://accounts.spotify.com/authorize".to_string(),
            token_url,
            api_base_url,
        );
        let app = test_router(spotify, "device-token").await;
        let start = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/auth/spotify")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(start.status(), StatusCode::FOUND);
        let location = start
            .headers()
            .get(header::LOCATION)
            .expect("location")
            .to_str()
            .expect("location text");
        let state = reqwest::Url::parse(location)
            .expect("authorization URL")
            .query_pairs()
            .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
            .expect("state");

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/auth/spotify/callback?code=code&state={state}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        // Finishing Spotify authorization signs the browser in and sends it
        // home. It must not print the device token: a secret on a page ends up
        // in screenshots and history, and hardware reads it from configuration.
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/")
        );
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .expect("a session cookie");
        assert!(cookie.starts_with("muse_session="), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(!cookie.contains("device-token"), "{cookie}");

        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("response body");
        assert!(!String::from_utf8_lossy(&body).contains("device-token"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A mock Spotify: `/token` for the code exchange, `/me` for the identity
    /// lookup the callback makes right after. Returns `(token_url, api_base_url)`.
    async fn spawn_token_server(calls: Arc<AtomicUsize>) -> (String, String) {
        let app = Router::new()
            .route(
                "/token",
                post(move || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Json(json!({
                            "access_token": "access-token",
                            "refresh_token": "refresh-token",
                            "expires_in": 3600
                        }))
                    }
                }),
            )
            .route("/me", get(|| async { Json(json!({ "id": "test-owner" })) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock token server");
        let address = listener.local_addr().expect("mock server address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock token server");
        });
        (
            format!("http://{address}/token"),
            format!("http://{address}"),
        )
    }
}
