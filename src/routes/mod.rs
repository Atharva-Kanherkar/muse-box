use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};

use crate::{error::AppError, spotify::SpotifyClient};

#[derive(Clone)]
struct AppState {
    spotify: SpotifyClient,
    device_api_token: String,
}

pub fn router(spotify: SpotifyClient, device_api_token: String) -> Router {
    let state = AppState {
        spotify,
        device_api_token: device_api_token.clone(),
    };
    let protected =
        Router::new()
            .route("/health", get(health))
            .route_layer(middleware::from_fn_with_state(
                device_api_token,
                require_bearer,
            ));

    Router::new()
        .route("/auth/spotify", get(start_spotify_auth))
        .route("/auth/spotify/callback", get(spotify_callback))
        .merge(protected)
        .with_state(state)
}

async fn start_spotify_auth(State(state): State<AppState>) -> Result<Response, AppError> {
    let location = state.spotify.authorization_url().await?;
    Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
}

async fn spotify_callback(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Html<String>, AppError> {
    let code = query
        .get("code")
        .ok_or_else(|| AppError::BadRequest("missing authorization code".to_string()))?;
    let oauth_state = query
        .get("state")
        .ok_or_else(|| AppError::BadRequest("missing OAuth state".to_string()))?;

    state
        .spotify
        .exchange_authorization_code(code, oauth_state)
        .await?;

    let device_token = escape_html(&state.device_api_token);
    Ok(Html(format!(
        "<!doctype html><html><body><h1>Spotify connected</h1><p>Device API token: <code>{device_token}</code></p></body></html>"
    )))
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn require_bearer(
    State(expected_token): State<String>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_credential)
        .is_some_and(|token| constant_time_eq(token.as_bytes(), expected_token.as_bytes()));

    if !authorized {
        return Err(AppError::Unauthorized);
    }

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

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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

    use crate::spotify::SpotifyConfig;

    use super::*;

    fn test_spotify(path: PathBuf) -> SpotifyClient {
        SpotifyClient::new(SpotifyConfig {
            client_id: "client-id".to_string(),
            client_secret: "client-secret".to_string(),
            redirect_uri: "http://localhost/auth/spotify/callback".to_string(),
            token_store_path: path,
        })
    }

    #[tokio::test]
    async fn bearer_middleware_rejects_missing_and_wrong_tokens() {
        let app = router(
            test_spotify(PathBuf::from("unused")),
            "right-token".to_string(),
        );

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
        let app = router(
            test_spotify(PathBuf::from("unused")),
            "right-token".to_string(),
        );
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
    async fn callback_rejects_bad_state_without_token_request() {
        let calls = Arc::new(AtomicUsize::new(0));
        let token_url = spawn_token_server(calls.clone()).await;
        let spotify = SpotifyClient::with_test_endpoints(
            SpotifyConfig {
                client_id: "client-id".to_string(),
                client_secret: "client-secret".to_string(),
                redirect_uri: "http://localhost/auth/spotify/callback".to_string(),
                token_store_path: PathBuf::from("unused"),
            },
            "https://accounts.spotify.com/authorize".to_string(),
            token_url,
        );
        let app = router(spotify, "device-token".to_string());

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
    async fn callback_success_returns_provisioning_page() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let calls = Arc::new(AtomicUsize::new(0));
        let token_url = spawn_token_server(calls.clone()).await;
        let spotify = SpotifyClient::with_test_endpoints(
            SpotifyConfig {
                client_id: "client-id".to_string(),
                client_secret: "client-secret".to_string(),
                redirect_uri: "http://localhost/auth/spotify/callback".to_string(),
                token_store_path: directory.path().join("spotify.json"),
            },
            "https://accounts.spotify.com/authorize".to_string(),
            token_url,
        );
        let app = router(spotify, "device-token".to_string());
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
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("response body");
        assert!(String::from_utf8_lossy(&body).contains("device-token"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    async fn spawn_token_server(calls: Arc<AtomicUsize>) -> String {
        let app = Router::new().route(
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
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock token server");
        let address = listener.local_addr().expect("mock server address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock token server");
        });
        format!("http://{address}/token")
    }
}
