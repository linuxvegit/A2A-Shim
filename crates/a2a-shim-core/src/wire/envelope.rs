//! Generic JSON-RPC 2.0 envelope types (spec § 4.3).
//!
//! `JsonRpcRequest<P>` and `JsonRpcResponse<R>` are parameterised over the
//! method-specific params/result shape so a single envelope works for every
//! A2A method.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest<P> {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    pub params: P,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse<R> {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(flatten)]
    pub result_or_error: ResultOrError<R>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResultOrError<R> {
    Result { result: R },
    Error { error: JsonRpcError },
}

impl<R> ResultOrError<R> {
    pub fn from_result(r: R) -> Self {
        Self::Result { result: r }
    }
    pub fn from_error(e: JsonRpcError) -> Self {
        Self::Error { error: e }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}
