use a2a_shim_core::logging::{try_init_idempotent, LogDestination, LogFormat, LoggingOptions};

#[test]
fn options_default_to_stderr_compact_info() {
    let o = LoggingOptions::default();
    assert!(matches!(o.destination, LogDestination::Stderr));
    assert!(matches!(o.format, LogFormat::Compact));
    assert_eq!(o.level, "info");
}

#[test]
fn init_idempotent_no_panic() {
    // Second call must NOT panic and must NOT return an error.
    try_init_idempotent(LoggingOptions::default()).expect("first init");
    try_init_idempotent(LoggingOptions::default()).expect("second init must be ok");
}
