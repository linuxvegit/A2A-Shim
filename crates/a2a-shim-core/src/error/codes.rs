//! JSON-RPC + A2A-Shim error codes (spec § 4.6).
//!
//! Negative ranges follow JSON-RPC 2.0 convention: -32700..-32600 are
//! standard JSON-RPC errors; -32099..-32000 is the "server defined"
//! range where the spec parks our domain-specific codes.

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

pub const TASK_NOT_FOUND: i32 = -32001;
pub const TASK_NOT_CANCELABLE: i32 = -32002;

pub const CONVERSATION_BUSY: i32 = -32010;
pub const CONVERSATION_LIMIT_REACHED: i32 = -32011;

// v1.1 additions
pub const CONVERSATION_EXISTS: i32 = -32012;
pub const CONVERSATION_LOST: i32 = -32013;
pub const PUSH_NOTIFICATIONS_NOT_SUPPORTED: i32 = -32030;
pub const INVALID_PUSH_NOTIFICATION_CONFIG: i32 = -32031;
