//! Optional end-to-end test against the real `@agentclientprotocol/
//! claude-agent-acp` npm binary. Validates that the same Phase 0 reality
//! checks still hold after Phase 1-3 implementation.
//!
//! GATED in two ways so it stays out of normal `cargo test --workspace`:
//!   1. `#[ignore]` so default test runs skip it.
//!   2. Early-return if `ANTHROPIC_API_KEY` is unset, so the rare operator
//!      who passes `--ignored` without a key gets a no-op pass instead
//!      of an opaque agent-side auth error.
//!
//! Run with:
//!   `cargo test -p a2a-shim --test e2e_claude_agent_acp -- --ignored`

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
        .expect("two parents up")
        .to_path_buf()
}

fn bin(name: &str) -> PathBuf {
    let mut p = workspace_target().join(name);
    if cfg!(windows) {
        p.set_extension("exe");
    }
    p
}

/// Resolve a `node <abs path to claude-agent-acp/dist/index.js>` command
/// line. On Windows the npm-installed `claude-agent-acp.cmd` shim defeats
/// `Command::new` and shell-words splitters, so we always launch via
/// node + the absolute JS entry — same trick the verify/ probe uses.
fn resolve_agent_cmdline() -> Option<String> {
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let out = std::process::Command::new(npm)
        .args(["root", "-g"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = std::str::from_utf8(&out.stdout).ok()?.trim();
    let p = PathBuf::from(root).join("@agentclientprotocol/claude-agent-acp/dist/index.js");
    if !p.exists() {
        return None;
    }
    Some(format!(
        "node \"{}\"",
        p.display().to_string().replace('\\', "/")
    ))
}

fn write_serve_config(agent_cmdline: &str) -> (PathBuf, PathBuf) {
    let tmp = std::env::temp_dir().join(format!(
        "a2a-shim-e2e-claude-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).expect("mkdir");
    let cfg_path = tmp.join("sample-config.toml");

    // Split the cmdline at the first space: first token = command, rest = args.
    // (Same shell-words behavior the SDK's AcpAgent::from_str uses.)
    let mut parts = agent_cmdline.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");
    let mut args_toml = Vec::new();
    if !rest.is_empty() {
        // The rest may have one argument like "C:/path/dist/index.js"
        // possibly quoted; strip surrounding quotes if present.
        let arg = rest.trim().trim_matches('"').to_string();
        args_toml.push(format!("\"{arg}\""));
    }
    let cfg_body = format!(
        r#"
[server]
listen = "127.0.0.1:0"

[agent]
command = "{cmd}"
args = [{args}]
cwd = "{cwd}"
"#,
        cmd = cmd.trim_matches('"'),
        args = args_toml.join(", "),
        cwd = tmp.display().to_string().replace('\\', "/"),
    );
    std::fs::write(&cfg_path, cfg_body).expect("write cfg");
    (tmp, cfg_path)
}

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
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
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
        panic!("serve never logged bind line. stderr:\n{all}")
    });
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
#[ignore = "requires ANTHROPIC_API_KEY + claude-agent-acp installed; opt-in via --ignored"]
async fn one_turn_against_real_claude_agent_acp() {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!("[skip] ANTHROPIC_API_KEY not set; no-op pass");
        return;
    }
    let Some(agent_cmdline) = resolve_agent_cmdline() else {
        eprintln!("[skip] claude-agent-acp not installed under npm root -g");
        return;
    };
    eprintln!("[run] agent cmdline: {agent_cmdline}");

    let shim = bin("a2a-shim");
    assert!(shim.exists(), "a2a-shim binary missing");

    let (_tmp, cfg) = write_serve_config(&agent_cmdline);
    let (mut serve_child, serve_addr) = spawn_serve(&cfg);

    let mut client_child = Command::new(&shim)
        .args(["client"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn client");
    let mut stdin = client_child.stdin.take().unwrap();
    let stdout = client_child.stdout.take().unwrap();

    let reqs = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "a2a_send",
                "arguments": {
                    "endpoint": format!("http://{serve_addr}"),
                    "conversation_id": "e2e/real-claude",
                    "message": "Reply with exactly the single digit 7 and nothing else."
                }
            }
        }),
    ];
    for r in reqs {
        let mut s = serde_json::to_string(&r).unwrap();
        s.push('\n');
        stdin.write_all(s.as_bytes()).expect("write");
    }
    stdin.flush().expect("flush");
    drop(stdin);

    let collector = tokio::task::spawn_blocking(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();
        for line in reader.lines() {
            match line {
                Ok(l) => lines.push(l),
                Err(_) => break,
            }
        }
        lines
    });
    let lines = match tokio::time::timeout(Duration::from_secs(120), collector).await {
        Ok(Ok(lines)) => lines,
        _ => {
            common::kill_tree(&mut client_child);
            common::kill_tree(&mut serve_child);
            panic!("client stdout never EOF'd within 120s (real LLM call can be slow)");
        }
    };
    common::kill_tree(&mut client_child);
    common::kill_tree(&mut serve_child);

    let mut by_id = std::collections::HashMap::<i64, Value>::new();
    for line in &lines {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(t)
            .unwrap_or_else(|e| panic!("non-JSON on client stdout: {line:?} ({e})"));
        if let Some(id) = v.get("id").and_then(Value::as_i64) {
            by_id.insert(id, v);
        }
    }
    let call = by_id
        .get(&2)
        .unwrap_or_else(|| panic!("missing tools/call response; got ids: {:?}", by_id.keys()));
    let result = &call["result"];
    assert_eq!(
        result["isError"], false,
        "tools/call returned error: {call}"
    );
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        !text.is_empty(),
        "expected non-empty text from real Claude, got nothing: {call}"
    );
    assert_eq!(
        result["_meta"]["a2aTask"]["status"]["state"], "completed",
        "expected completed task, got {call}"
    );
    eprintln!("[ok] Claude answered: {text:?}");
}
