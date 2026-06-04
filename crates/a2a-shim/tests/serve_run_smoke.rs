//! End-to-end smoke for `a2a-shim serve`: spawn the real binary pointed
//! at mock_acp_agent, POST one message/send, assert the response carries
//! a completed Task with answer "4".

mod common;

use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

fn mock_bin() -> PathBuf {
    let mut p = workspace_target().join("mock_acp_agent");
    if cfg!(windows) {
        p.set_extension("exe");
    }
    assert!(p.exists(), "mock_acp_agent missing at {}", p.display());
    p
}

#[tokio::test]
async fn serve_run_smoke_message_send_through_full_pipeline() {
    // Write a tiny sample config pointing at mock_acp_agent --script happy.
    let tmp = tempdir_path();
    let cfg_path = tmp.join("sample-config.toml");
    let mock = mock_bin();
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
        tmp.display().to_string().replace('\\', "/")
    );
    std::fs::write(&cfg_path, cfg_body).expect("write cfg");

    // Spawn the shim, capture stderr so we can sniff the bind line.
    let mut child = Command::new(shim_bin())
        .args(["serve", "--config", cfg_path.to_str().unwrap()])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn shim");

    let stderr = child.stderr.take().expect("piped stderr");
    let mut reader = BufReader::new(stderr);

    // Scan stderr until we see "listening on 127.0.0.1:<port>" or time out.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut bound: Option<String> = None;
    let mut all_stderr = String::new();
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break, // child exited
            Ok(_) => {
                all_stderr.push_str(&line);
                if let Some(addr) = parse_bind_line(&line) {
                    bound = Some(addr);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let addr = bound.unwrap_or_else(|| {
        common::kill_tree(&mut child);
        panic!("serve never logged bind address; stderr was:\n{all_stderr}")
    });

    // POST a happy-path message/send.
    let url = format!("http://{addr}/");
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [{ "type": "text", "text": "go" }],
                "metadata": { "x-a2a-shim/conversation": "e2e/smoke" }
            }
        }
    });
    let resp: Value = tokio::time::timeout(
        Duration::from_secs(10),
        reqwest::Client::new().post(&url).json(&body).send(),
    )
    .await
    .expect("rpc timeout")
    .expect("reqwest ok")
    .json()
    .await
    .expect("parse json");

    common::kill_tree(&mut child);

    let task = &resp["result"];
    assert_eq!(task["status"]["state"], "completed", "got: {resp}");
    let text = task["artifacts"][0]["parts"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert_eq!(text, "4");
}

/// Pull "127.0.0.1:NNNNN" out of a tracing line like
/// "INFO a2a_shim_serve::run: serve listening on 127.0.0.1:54321".
fn parse_bind_line(line: &str) -> Option<String> {
    let needle = "listening on ";
    let idx = line.find(needle)?;
    let tail = &line[idx + needle.len()..];
    let end = tail
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ':'))
        .unwrap_or(tail.len());
    let addr = tail[..end].to_string();
    if addr.contains(':') && addr.contains('.') {
        Some(addr)
    } else {
        None
    }
}

fn tempdir_path() -> PathBuf {
    // Cheap unique-per-test directory; cargo test creates a fresh CWD per
    // run anyway so collisions are unlikely.
    let base = std::env::temp_dir().join(format!(
        "a2a-shim-serve-smoke-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).expect("mkdir");
    base
}
