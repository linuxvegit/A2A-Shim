//! End-to-end smoke for `a2a-shim client`. Spawns the binary, drives it
//! via stdin/stdout, and asserts every line on stdout is parseable JSON
//! (i.e. no log line ever leaked onto the MCP transport).

mod common;

use axum::{
    response::sse::{Event, KeepAlive, Sse},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::net::TcpListener;

fn workspace_target() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    exe.parent()
        .and_then(|p| p.parent())
        .expect("two parents up")
        .to_path_buf()
}

fn shim_bin() -> PathBuf {
    let mut p = workspace_target().join("a2a-shim");
    if cfg!(windows) {
        p.set_extension("exe");
    }
    assert!(p.exists(), "a2a-shim binary missing at {}", p.display());
    p
}

async fn spawn_happy_a2a() -> std::net::SocketAddr {
    let app = Router::new().route(
        "/",
        post(|Json(_): Json<Value>| async {
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
                            "parts": [{ "text": "42" }]
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
                for e in events {
                    yield Ok::<_, Infallible>(Event::default().data(serde_json::to_string(&e).unwrap()));
                }
            };
            Sse::new(stream).keep_alive(KeepAlive::default())
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

#[tokio::test]
async fn client_run_smoke_stdout_is_pure_jsonrpc() {
    let a2a_addr = spawn_happy_a2a().await;

    let mut child = Command::new(shim_bin())
        .args(["client"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Drain stderr to null so the child cannot deadlock on a full
        // stderr pipe (tracing chatters per request); we test stdout
        // discipline here, not log content.
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn shim client");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    // Drive the loop: initialize, tools/list, tools/call (a2a_send).
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    let list = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    let call = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "a2a_send",
            "arguments": {
                "endpoint": format!("http://{a2a_addr}"),
                "conversation_id": "alice/smoke",
                "message": "what is the answer?"
            }
        }
    });
    for req in [init, list, call] {
        let mut s = serde_json::to_string(&req).unwrap();
        s.push('\n');
        stdin.write_all(s.as_bytes()).expect("write request");
    }
    stdin.flush().expect("flush");
    drop(stdin);

    // Collect stdout in a blocking thread, race against a deadline. If
    // the child somehow stays alive past the deadline (it shouldn't —
    // serve_loop returns on stdin EOF — but be defensive), kill it so
    // the test does not hang the CI worker.
    let collector = tokio::task::spawn_blocking(move || {
        let mut lines = Vec::new();
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) => lines.push(l),
                Err(_) => break,
            }
        }
        lines
    });
    let lines = match tokio::time::timeout(Duration::from_secs(15), collector).await {
        Ok(Ok(lines)) => lines,
        _ => {
            common::kill_tree(&mut child);
            panic!("child stdout never EOF'd within 15s deadline");
        }
    };
    common::kill_tree(&mut child);

    // EVERY non-empty stdout line must parse as JSON. That is the
    // 'stdout is MCP transport, nothing else' invariant.
    let mut responses = std::collections::HashMap::<i64, Value>::new();
    let mut progress_count = 0usize;
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(trimmed)
            .unwrap_or_else(|e| panic!("non-JSON line on stdout: {line:?} ({e})"));
        if let Some(id) = v.get("id").and_then(Value::as_i64) {
            responses.insert(id, v);
        } else if v.get("method").and_then(Value::as_str) == Some("notifications/progress") {
            progress_count += 1;
        }
    }

    // Three responses expected (ids 1, 2, 3) — heartbeat is 30s so we
    // do not expect progress frames in this fast-completing test.
    assert!(responses.contains_key(&1), "missing initialize response");
    assert!(responses.contains_key(&2), "missing tools/list response");
    let call_resp = responses.get(&3).expect("missing tools/call response");

    // The tool result must be the happy-path render: isError=false,
    // text contains "42", _meta.a2aTask.status.state == "completed".
    let result = &call_resp["result"];
    assert_eq!(result["isError"], false, "got: {call_resp}");
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("42"),
        "expected '42' in tool text, got '{text}'"
    );
    assert_eq!(
        result["_meta"]["a2aTask"]["status"]["state"], "completed",
        "got: {call_resp}"
    );

    // Sanity: heartbeat is 30s default, so no progress notifications
    // should have fired in this test (which completes in under 1s).
    assert_eq!(
        progress_count, 0,
        "unexpected progress notifications: {progress_count}"
    );
}
