//! Outbound A2A streaming tests. Spins up a tiny axum server in-process
//! (more predictable than wiremock's SSE) and points `outbound::stream`
//! at it. Covers the happy path, HTTP 500 mapping, and stream-idle
//! timeout.

use a2a_shim_client::outbound::{stream, OutboundDeadlines, OutboundError};
use a2a_shim_core::wire::sse::SseEvent;
use axum::{
    response::sse::{Event, KeepAlive, Sse},
    routing::post,
    Json, Router,
};
use futures::StreamExt;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
struct ServerCfg {
    /// If Some, the server delays this long BEFORE emitting any event.
    initial_delay: Option<Duration>,
    /// If true, respond with HTTP 500 immediately.
    fail_500: bool,
}

async fn spawn_fake(cfg: ServerCfg) -> SocketAddr {
    let cfg = Arc::new(Mutex::new(cfg));
    let app = Router::new().route(
        "/",
        post({
            let cfg = Arc::clone(&cfg);
            move |Json(_body): Json<Value>| {
                let cfg = Arc::clone(&cfg);
                async move {
                    let cfg = cfg.lock().await.clone();
                    if cfg.fail_500 {
                        return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                    // Build a small canonical stream: status(working) ->
                    // artifact-update("4") -> status(completed final=true).
                    let initial_delay = cfg.initial_delay;
                    let events = vec![
                        json!({
                            "statusUpdate": {
                                "taskId": "t-fake",
                                "status": { "state": "working" },
                                "final": false
                            }
                        }),
                        json!({
                            "artifactUpdate": {
                                "taskId": "t-fake",
                                "artifact": {
                                    "artifactId": "a-answer",
                                    "parts": [{ "text": "4" }]
                                },
                                "append": false
                            }
                        }),
                        json!({
                            "statusUpdate": {
                                "taskId": "t-fake",
                                "status": { "state": "completed" },
                                "final": true
                            }
                        }),
                    ];
                    let stream = async_stream::stream! {
                        if let Some(d) = initial_delay {
                            tokio::time::sleep(d).await;
                        }
                        for e in events {
                            let s = serde_json::to_string(&e).unwrap();
                            yield Ok::<_, Infallible>(Event::default().data(s));
                        }
                    };
                    Sse::new(stream)
                        .keep_alive(KeepAlive::default())
                        .into_response()
                }
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

fn dead_default() -> OutboundDeadlines {
    OutboundDeadlines {
        connect: Duration::from_secs(5),
        stream_idle: Duration::from_secs(10),
        hard_ceiling: Duration::from_secs(60),
    }
}

use axum::response::IntoResponse;

#[tokio::test]
async fn happy_path_yields_three_events_then_ends() {
    let addr = spawn_fake(ServerCfg::default()).await;
    let endpoint = format!("http://{addr}");
    let mut s = stream(&endpoint, "alice/x", "hi", None, None, None, None, dead_default())
        .await
        .expect("open stream");
    let mut got = Vec::new();
    while let Some(item) = s.next().await {
        got.push(item.expect("event ok"));
    }
    assert_eq!(got.len(), 3, "got: {got:?}");
    assert!(matches!(got[0], SseEvent::StatusUpdate { .. }));
    assert!(matches!(got[1], SseEvent::ArtifactUpdate { .. }));
    assert!(matches!(
        &got[2],
        SseEvent::StatusUpdate { inner } if inner.final_
    ));
}

#[tokio::test]
async fn http_500_maps_to_remote_failed() {
    let addr = spawn_fake(ServerCfg {
        fail_500: true,
        ..Default::default()
    })
    .await;
    let endpoint = format!("http://{addr}");
    let res = stream(&endpoint, "alice/x", "hi", None, None, None, None, dead_default()).await;
    match res {
        Err(OutboundError::RemoteFailed { status, .. }) => assert_eq!(status, 500),
        Err(e) => panic!("expected RemoteFailed(500), got Err({e:?})"),
        Ok(_) => panic!("expected RemoteFailed(500), got Ok(stream)"),
    }
}

#[tokio::test]
async fn idle_timeout_yields_remote_timeout() {
    // Server delays 500ms before first event; we set stream_idle to 100ms.
    let addr = spawn_fake(ServerCfg {
        initial_delay: Some(Duration::from_millis(500)),
        ..Default::default()
    })
    .await;
    let endpoint = format!("http://{addr}");
    let deadlines = OutboundDeadlines {
        connect: Duration::from_secs(5),
        stream_idle: Duration::from_millis(100),
        hard_ceiling: Duration::from_secs(60),
    };
    let mut s = stream(&endpoint, "alice/x", "hi", None, None, None, None, deadlines)
        .await
        .expect("open stream ok");
    // The very first item should be a RemoteTimeout error and then the
    // stream ends.
    let first = s.next().await.expect("got an item");
    match first {
        Err(OutboundError::RemoteTimeout { .. }) => {} // ok
        Err(e) => panic!("expected RemoteTimeout, got Err({e:?})"),
        Ok(_) => panic!("expected RemoteTimeout, got Ok(stream-event)"),
    }
}

#[tokio::test]
async fn missing_endpoint_yields_network_error() {
    let endpoint = "http://127.0.0.1:1"; // nobody home
    let deadlines = OutboundDeadlines {
        connect: Duration::from_millis(500),
        stream_idle: Duration::from_secs(10),
        hard_ceiling: Duration::from_secs(60),
    };
    let res = stream(endpoint, "alice/x", "hi", None, None, None, None, deadlines).await;
    let msg = match &res {
        Err(OutboundError::NetworkError(_)) => return,
        Err(e) => format!("Err({e:?})"),
        Ok(_) => "Ok(stream)".into(),
    };
    panic!("expected NetworkError, got {msg}");
}
