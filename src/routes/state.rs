use std::{collections::HashMap, convert::Infallible, pin::Pin, sync::Arc};

use axum::{
    extract::{Extension, Query},
    response::sse::{Event, KeepAlive, Sse},
};
use futures::{Stream, StreamExt, stream};

use crate::{account::AccountRuntime, error::AppError, render::RenderDoc, state::RenderParams};

type StateStream = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

pub(crate) async fn get_state(
    Extension(runtime): Extension<Arc<AccountRuntime>>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Sse<StateStream>, AppError> {
    let hub = runtime.hub.clone();
    let params = RenderParams::from_query(&query)?;
    let keep_alive = hub.keep_alive();
    let receiver = hub.subscribe();
    let document = hub.current_document(params).await?;
    let initial_event = state_event(&document)?;
    let initial = stream::once(async move { Ok(initial_event) });
    let updates = stream::unfold(
        (receiver, hub, params),
        |(mut receiver, hub, params)| async move {
            loop {
                match receiver.recv().await {
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        match hub.current_document(params).await {
                            Ok(document) => match state_event(&document) {
                                Ok(event) => {
                                    return Some((Ok(event), (receiver, hub, params)));
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "failed to serialize SSE state event");
                                }
                            },
                            Err(error) => {
                                tracing::warn!(%error, "failed to render SSE state event");
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    let events: StateStream = Box::pin(initial.chain(updates));
    // A stream that sends nothing between meaningful changes gets reaped by any
    // proxy with an idle timeout, and neither end learns why: the client just
    // reconnects, forever. An SSE comment keeps the socket warm without being an
    // event — clients skip it, so the "zero events in steady state" rule holds.
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(keep_alive).text("")))
}

fn state_event(document: &RenderDoc) -> Result<Event, AppError> {
    Event::default()
        .event("state")
        .id(uuid::Uuid::new_v4().to_string())
        .json_data(document)
        .map_err(|error| AppError::Internal(error.into()))
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, path::PathBuf, time::Duration};

    use ::image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use axum::{
        Router,
        body::{Body, BodyDataStream, to_bytes},
        http::{Request, StatusCode, header},
        response::IntoResponse,
        routing::get,
    };
    use chrono::{Duration as ChronoDuration, Utc};
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::{
        routes,
        spotify::{PlaybackObservation, SpotifyClient, SpotifyConfig},
        state::StateHub,
    };

    use super::*;

    #[tokio::test]
    async fn the_stream_is_kept_warm_without_emitting_state_events() {
        // Without this the connection is silent between changes, so any proxy
        // with an idle timeout drops it and the client reconnects forever.
        let hub = Arc::new(StateHub::new().with_keep_alive(Duration::from_millis(60)));
        let app = test_router(hub.clone()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/state?w=16&h=16")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let mut stream = response.into_body().into_data_stream();

        // The initial document, then nothing but comments.
        let initial = next_event(&mut stream).await.expect("initial event");
        assert!(initial.contains("data:"));

        let mut raw = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    raw.push_str(&String::from_utf8_lossy(&chunk));
                    if raw.contains("\n\n") {
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(
            raw.starts_with(':'),
            "expected an SSE comment to hold the socket open, got {raw:?}"
        );
        assert!(
            !raw.contains("data:"),
            "a keep-alive must not carry a state document: {raw:?}"
        );
    }

    #[tokio::test]
    async fn sse_is_immediate_silent_for_steady_progress_then_emits_changes() {
        let art_url = spawn_art_server().await;
        let hub = Arc::new(StateHub::new());
        let app = test_router(hub.clone()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/state?w=16&h=16&dither=atkinson")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        let initial = tokio::time::timeout(Duration::from_millis(100), next_event(&mut stream))
            .await
            .expect("immediate event")
            .expect("initial event");
        assert_eq!(event_json(&initial)["state"], "idle");

        let start = Utc::now();
        hub.publish_if_meaningful(observation(start, "track-a", 10_000, Some(art_url)))
            .await
            .expect("publish track");
        let changed = next_event(&mut stream).await.expect("track event");
        let changed = event_json(&changed);
        assert_eq!(changed["track_id"], "track-a");
        assert_eq!(changed["art"]["w"], 16);
        assert_eq!(changed["art"]["h"], 16);
        assert!(changed["art_url"].as_str().is_some());

        let steady = observation(
            start + ChronoDuration::seconds(2),
            "track-a",
            12_000,
            changed["art_url"].as_str().map(str::to_string),
        );
        assert!(
            !hub.publish_if_meaningful(steady)
                .await
                .expect("steady poll")
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), stream.next())
                .await
                .is_err()
        );

        let seek = observation(
            start + ChronoDuration::seconds(2),
            "track-a",
            14_001,
            changed["art_url"].as_str().map(str::to_string),
        );
        assert!(hub.publish_if_meaningful(seek).await.expect("seek poll"));
        let seek_event = next_event(&mut stream).await.expect("seek event");
        assert_eq!(event_json(&seek_event)["progress_ms"], 14_001);

        drop(stream);
        assert_eq!(hub.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn state_route_rejects_auth_and_parameter_matrix() {
        let app = test_router(Arc::new(StateHub::new())).await;
        for authorization in [None, Some("Bearer wrong")] {
            let mut request = Request::builder().uri("/state");
            if let Some(value) = authorization {
                request = request.header(header::AUTHORIZATION, value);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).expect("request"))
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        for uri in [
            "/state?w=8",
            "/state?h=2000",
            "/state?dither=floyd",
            "/state?w=not-a-number",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(header::AUTHORIZATION, "Bearer test-token")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = to_bytes(response.into_body(), 1024)
                .await
                .expect("error body");
            assert!(serde_json::from_slice::<Value>(&body).unwrap()["error"].is_string());
        }
    }

    async fn test_router(hub: Arc<StateHub>) -> Router {
        routes::test_router_with(
            SpotifyClient::new(SpotifyConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                redirect_uri: "http://localhost/callback".to_string(),
                token_store_path: PathBuf::from("unused"),
            }),
            "test-token".to_string(),
            hub,
            Arc::new(routes::voice::FailingVoiceModel),
        )
        .await
    }

    fn observation(
        observed_at: chrono::DateTime<Utc>,
        track_id: &str,
        progress_ms: u64,
        art_url: Option<String>,
    ) -> PlaybackObservation {
        PlaybackObservation {
            observed_at,
            track_id: Some(track_id.to_string()),
            track: Some("Track".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            art_url,
            is_playing: true,
            progress_ms,
            duration_ms: 300_000,
        }
    }

    async fn next_event(stream: &mut BodyDataStream) -> Option<String> {
        let mut event = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.ok()?;
            event.push_str(&String::from_utf8_lossy(&chunk));
            if event.contains("\n\n") {
                // Keep-alive comments are not events; skip them so silence
                // assertions still measure real state traffic.
                if event.lines().any(|line| line.starts_with("data:")) {
                    return Some(event);
                }
                event.clear();
            }
        }
        None
    }

    fn event_json(event: &str) -> Value {
        let data = event
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("SSE data line");
        serde_json::from_str(data).expect("SSE JSON")
    }

    async fn spawn_art_server() -> String {
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_fn(32, 32, |x, y| {
            Rgb([(x * 7) as u8, (y * 7) as u8, ((x + y) * 3) as u8])
        }))
        .write_to(&mut encoded, ImageFormat::Png)
        .expect("encode PNG");
        let bytes = Arc::new(encoded.into_inner());
        let app = Router::new().route(
            "/art.png",
            get(move || {
                let bytes = bytes.clone();
                async move { Body::from(bytes.as_ref().clone()).into_response() }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind art server");
        let address = listener.local_addr().expect("art server address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("art server");
        });
        format!("http://{address}/art.png")
    }
}
