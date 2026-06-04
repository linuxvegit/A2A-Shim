use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_a2a-shim")
}

#[test]
fn help_lists_both_subcommands() {
    let out = Command::new(bin()).arg("--help").output().expect("run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("serve"),  "help missing serve: {stdout}");
    assert!(stdout.contains("client"), "help missing client: {stdout}");
}

#[test]
fn serve_help_shows_listen_flag() {
    let out = Command::new(bin())
        .args(["serve", "--help"])
        .output()
        .expect("run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--listen"), "serve --help missing --listen: {stdout}");
}

#[test]
fn client_help_shows_heartbeat_flag() {
    let out = Command::new(bin())
        .args(["client", "--help"])
        .output()
        .expect("run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--heartbeat-secs"),
        "client --help missing --heartbeat-secs: {stdout}"
    );
}
