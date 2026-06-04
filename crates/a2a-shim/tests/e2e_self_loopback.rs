//! End-to-end self-loopback test.
//!
//! Spawns the real `a2a-shim serve` binary (wrapping `mock_acp_agent`),
//! then spawns the real `a2a-shim client` binary, drives the client via
//! stdin and reads its stdout, and asserts the answer made the full
//! round trip: stdin → MCP → outbound HTTP/SSE → serve JSON-RPC → bridge
//! → ACP → mock → ACP update → bridge → SSE → outbound → render → stdout.
//!
//! This is the canonical proof that all three Phase boundaries
//! (1/2/3) compose into one working system.

mod common;

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn workspace_target() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    exe.parent()
        .and_then(|p| p.parent())
        .expect("two parents up from test bin")
        .to_path_buf()
}

fn bin(name: &str) -> PathBuf {
    let mut p = workspace_target().join(name);
    if cfg!(windows) {
        p.set_extension("exe");
    }
    assert!(p.exists(), "binary missing at {}", p.display());
    p
}

/// Write a temp directory + config TOML pointing the serve shim at
/// `mock_acp_agent --script happy` bound to a random loopback port.
fn write_serve_config() -> (PathBuf, PathBuf) {
    let tmp = std::env::temp_dir().join(format!(
        "a2a-shim-e2e-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).expect("mkdir tmp");
    let cfg_path = tmp.join("sample-config.toml");
    let mock = bin("mock_acp_agent");
    let cfg_body = format!(
        r#"
[server]
listen = "127.0.0.1:0"

[agent]
command = "{}"
args = ["--script", "happy"]
cwd = "{}"
"#,
        mock.display().to_string().replace('\\', "/"),
        tmp.display().to_string().replace('\\', "/"),
    );
    std::fs::write(&cfg_path, cfg_body).expect("write cfg");
    (tmp, cfg_path)
}

/// Spawn `a2a-shim serve --config <cfg>`, scan stderr for the bind line,
/// return (child, bound_address_string).
fn spawn_serve(cfg_path: &std::path::Path) -> (std::process::Child, String) {
    let mut serve = Command::new(bin("a2a-shim"))
        .args(["serve", "--config", cfg_path.to_str().unwrap()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");

    let stderr = serve.stderr.take().expect("piped stderr");
    let mut reader = BufReader::new(stderr);
    let mut bound: Option<String> = None;
    let mut all = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                all.push_str(&line);
                if let Some(addr) = parse_bind_line(&line) {
                    bound = Some(addr);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let addr = bound.unwrap_or_else(|| {
        let _ = serve.kill();
        panic!("serve never logged bind line. stderr was:\n{all}")
    });
    // Drain remaining stderr in a thread so the serve child cannot
    // deadlock on a full stderr pipe across the test lifetime.
    std::thread::spawn(move || {
        let mut sink = String::new();
        loop {
            sink.clear();
            if reader.read_line(&mut sink).map(|n| n == 0).unwrap_or(true) {
                break;
            }
        }
    });
    (serve, addr)
}

fn parse_bind_line(line: &str) -> Option<String> {
    let idx = line.find("listening on ")?;
    let tail = &line[idx + "listening on ".len()..];
    let end = tail
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ':'))
        .unwrap_or(tail.len());
    let addr = tail[..end].to_string();
    (addr.contains(':') && addr.contains('.')).then_some(addr)
}

#[tokio::test]
async fn full_loopback_host_to_client_to_serve_to_mock_to_back() {
    // 1) bring up serve + mock
    let (_tmp, cfg) = write_serve_config();
    let (mut serve_child, serve_addr) = spawn_serve(&cfg);

    // 2) bring up client
    let mut client_child = Command::new(bin("a2a-shim"))
        .args(["client"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()) // tracing chatters; null sink avoids pipe-fill deadlock
        .spawn()
        .expect("spawn client");
    let mut stdin = client_child.stdin.take().expect("stdin");
    let stdout = client_child.stdout.take().expect("stdout");

    // 3) drive MCP: initialize, tools/list, tools/call
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} });
    let call = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "a2a_send",
            "arguments": {
                "endpoint": format!("http://{serve_addr}"),
                "conversation_id": "e2e/loopback",
                "message": "what is 2+2?"
            }
        }
    });
    for req in [init, list, call] {
        let mut s = serde_json::to_string(&req).unwrap();
        s.push('\n');
        stdin.write_all(s.as_bytes()).expect("write");
    }
    stdin.flush().expect("flush");
    drop(stdin);

    // 4) collect stdout in a blocking thread, race against a deadline.
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
    let lines = match tokio::time::timeout(Duration::from_secs(30), collector).await {
        Ok(Ok(lines)) => lines,
        _ => {
            common::kill_tree(&mut client_child);
            common::kill_tree(&mut serve_child);
            panic!("client stdout never EOF'd within 30s");
        }
    };
    common::kill_tree(&mut client_child);
    common::kill_tree(&mut serve_child);

    // 5) every stdout line must parse as JSON (the discipline invariant),
    //    and the tools/call response (id=3) must carry the happy answer.
    let mut by_id = std::collections::HashMap::<i64, Value>::new();
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(trimmed)
            .unwrap_or_else(|e| panic!("non-JSON on client stdout: {line:?} ({e})"));
        if let Some(id) = v.get("id").and_then(Value::as_i64) {
            by_id.insert(id, v);
        }
    }
    let call_resp = by_id
        .get(&3)
        .unwrap_or_else(|| panic!("missing tools/call response; got ids: {:?}", by_id.keys()));
    let result = &call_resp["result"];
    assert_eq!(
        result["isError"], false,
        "tools/call returned error: {call_resp}"
    );
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert_eq!(text, "4", "expected '4', got '{text}' from {call_resp}");
    assert_eq!(
        result["_meta"]["a2aTask"]["status"]["state"], "completed",
        "expected completed task, got {call_resp}"
    );
    // Bonus: confirm conversation context flowed all the way through.
    assert_eq!(
        result["_meta"]["a2aTask"]["contextId"], "e2e/loopback",
        "context_id did not survive the round trip"
    );
}
