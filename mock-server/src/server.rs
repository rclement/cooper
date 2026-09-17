use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::{StreamExt, stream};

use crate::anthropic_wire::build_anthropic_sse_events;
use crate::fixture::Fixture;
use crate::wire::build_sse_payloads;

struct AppState {
    fixture: Fixture,
    next_index: AtomicUsize,
}

pub fn app(fixture: Fixture) -> Router {
    let state = Arc::new(AppState {
        fixture,
        next_index: AtomicUsize::new(0),
    });

    Router::new()
        .route("/health", get(health))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

/// Cycles through `fixture.responses` in order, wrapping back to the start
/// once exhausted — so re-running a prompt manually (e.g. from the web UI)
/// replays the scripted sequence instead of erroring on the second call.
async fn chat_completions(State(state): State<Arc<AppState>>) -> Response {
    if state.fixture.responses.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "cooper-mock-server: fixture has no responses configured".to_string(),
        )
            .into_response();
    }

    let index = state.next_index.fetch_add(1, Ordering::SeqCst);
    let response = &state.fixture.responses[index % state.fixture.responses.len()];

    let id = format!("chatcmpl-mock-{index}");
    let payloads = build_sse_payloads(response, &id);

    let events = stream::iter(
        payloads
            .into_iter()
            .map(|p| Ok::<_, Infallible>(Event::default().data(p))),
    )
    .chain(stream::once(async { Ok(Event::default().data("[DONE]")) }));

    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Cycles through `fixture.responses` the same way `chat_completions` does,
/// but speaks the Anthropic Messages API's SSE shape: every event carries an
/// explicit `event:` name, and the stream ends with `message_stop` rather
/// than a `[DONE]` sentinel.
async fn messages(State(state): State<Arc<AppState>>) -> Response {
    if state.fixture.responses.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "cooper-mock-server: fixture has no responses configured".to_string(),
        )
            .into_response();
    }

    let index = state.next_index.fetch_add(1, Ordering::SeqCst);
    let response = &state.fixture.responses[index % state.fixture.responses.len()];

    let id = format!("msg-mock-{index}");
    let events = build_anthropic_sse_events(response, &id);

    let events = stream::iter(
        events
            .into_iter()
            .map(|(name, data)| Ok::<_, Infallible>(Event::default().event(name).data(data))),
    );

    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::FixtureResponse;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn request() -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap()
    }

    fn one_response_fixture() -> Fixture {
        Fixture {
            responses: vec![FixtureResponse {
                reasoning: None,
                text: Some("PONG".to_string()),
                tool_calls: vec![],
                finish_reason: None,
                usage: None,
            }],
        }
    }

    #[tokio::test]
    async fn repeated_requests_cycle_instead_of_erroring() {
        let router = app(one_response_fixture());

        for _ in 0..3 {
            let response = router.clone().oneshot(request()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn empty_fixture_returns_server_error() {
        let router = app(Fixture { responses: vec![] });

        let response = router.oneshot(request()).await.unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn messages_endpoint_streams_anthropic_style_events_with_no_done_sentinel() {
        let router = app(one_response_fixture());

        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert!(body.contains("event: message_start"));
        assert!(body.contains("event: message_stop"));
        assert!(!body.contains("[DONE]"));
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let router = app(one_response_fixture());

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
