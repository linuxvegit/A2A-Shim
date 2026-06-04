//! v1.1 Tasks 28+29 client e2e: ensure caller_id + conversation_mode
//! args flow through call_handler -> outbound -> wiremock-style fake
//! and the request body carries the expected fields.

use a2a_shim_client::call_handler::{call_a2a_send, A2aSendArgs, CallContext};
use a2a_shim_client::cancellation::CancellationRegistry;
use a2a_shim_client::outbound::OutboundDeadlines;
use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
    routing::post,
    Json, Router,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

#[derive(Default, Clone)]
struct ReceivedBody(Arc<Mutex<Option<Value>>>);

async fn spawn_capturing_endpoint() -> (SocketAddr, ReceivedBody) {
    let captured = ReceivedBody::default();
    let captured_for_handler = captured.clone();
    let app = Router::new().route(
        "/",
        post(move |State(c): State<ReceivedBody>, Json(body): Json<Value>| async move {
            *c.0.lock() = Some(body);
            // Reply with a minimal happy SSE stream so the call completes.
            let events = vec![
                json!({
                    "statusUpdate": {
                        "taskId": "t-fake",
                        "status": { "state": "working" },
                        "final": false
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
                for e in events {
                    yield Ok::<_, Infallible>(Event::default().data(serde_json::to_string(&e).unwrap()));
                }
            };
            Sse::new(stream).keep_alive(KeepAlive::default())
        }),
    ).with_state(captured_for_handler);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, captured)
}

fn cx(writer_tx: tokio::sync::mpsc::UnboundedSender<Value>) -> CallContext {
    CallContext {
        request_id: json!(1),
        progress_token: None,
        writer_tx,
        registry: Arc::new(CancellationRegistry::new()),
        default_deadlines: OutboundDeadlines {
            connect: Duration::from_secs(2),
            stream_idle: Duration::from_secs(5),
            hard_ceiling: Duration::from_secs(30),
        },
        heartbeat_interval: Duration::from_secs(30),
    }
}

#[tokio::test]
async fn caller_id_arg_forwards_into_outbound_message_metadata() {
    let (addr, captured) = spawn_capturing_endpoint().await;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let args = serde_json::to_value(A2aSendArgs {
        endpoint: format!("http://{addr}"),
        conversation_id: "alice/x".into(),
        message: "hi".into(),
        task_id: None,
        timeout_secs: None,
        metadata: None,
        caller_id: Some("alice".into()),
        conversation_mode: None,
    })
    .unwrap();
    let _ = call_a2a_send(args, cx(tx)).await;

    let body = captured.0.lock().clone().expect("server received body");
    let md = &body["params"]["message"]["metadata"];
    assert_eq!(md["x-a2a-shim/caller_id"], "alice", "got: {body}");
}

#[tokio::test]
async fn conversation_mode_arg_forwards_into_outbound_params() {
    let (addr, captured) = spawn_capturing_endpoint().await;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let args = serde_json::to_value(A2aSendArgs {
        endpoint: format!("http://{addr}"),
        conversation_id: "alice/x".into(),
        message: "hi".into(),
        task_id: None,
        timeout_secs: None,
        metadata: None,
        caller_id: None,
        conversation_mode: Some("continue".into()),
    })
    .unwrap();
    let _ = call_a2a_send(args, cx(tx)).await;

    let body = captured.0.lock().clone().expect("server received body");
    assert_eq!(body["params"]["_shim_conversation_mode"], "continue");
}
