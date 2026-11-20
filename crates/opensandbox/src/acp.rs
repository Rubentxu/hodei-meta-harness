//! ACP wire codec: newline-delimited JSON-RPC 2.0 over the execd pipe.
//! Pure byte assembly and message encoding — the transport feeds bytes in,
//! complete messages come out.

use serde_json::{Value, json};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AcpWireError {
    #[error("invalid UTF-8 in ACP message: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
    #[error("invalid JSON in ACP message: {0}")]
    InvalidJson(#[source] serde_json::Error),
    #[error("message exceeds maximum size of {limit} bytes")]
    Oversized { limit: usize },
}

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Incremental NDJSON assembler: ACP transports split messages across pipe
/// chunks, so bytes are pushed and complete messages are drained.
#[derive(Debug, Default)]
pub struct AcpAssembler {
    buffer: Vec<u8>,
}

impl AcpAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw stdout bytes; returns every complete JSON value received so
    /// far (in order). Bytes after the last newline stay buffered.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, AcpWireError> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > MAX_MESSAGE_BYTES {
            let limit = MAX_MESSAGE_BYTES;
            self.buffer.clear();
            return Err(AcpWireError::Oversized { limit });
        }
        let mut messages = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let trimmed = &line[..line.len() - 1];
            let trimmed = trim_carriage_return(trimmed);
            if trimmed.is_empty() {
                continue;
            }
            let text = String::from_utf8(trimmed.to_vec())?;
            let value = serde_json::from_str(&text).map_err(AcpWireError::InvalidJson)?;
            messages.push(value);
        }
        Ok(messages)
    }
}

fn trim_carriage_return(bytes: &[u8]) -> &[u8] {
    match bytes.split_last() {
        Some((b'\r', rest)) => rest,
        _ => bytes,
    }
}

/// Encode a JSON-RPC request with numeric or string id.
pub fn encode_request(id: u64, method: &str, params: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string()
        + "\n"
}

/// Encode a JSON-RPC response for an incoming request id.
pub fn encode_response(id: Value, result: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
    .to_string()
        + "\n"
}

/// Encode a JSON-RPC error response for an incoming request id. It is the
/// answer an unsupported interaction gets, so the peer stops waiting.
pub fn encode_error(id: Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    })
    .to_string()
        + "\n"
}

/// The JSON-RPC code of a method this client does not implement.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// True when the message is a request carrying one of the expected methods.
pub fn is_method(message: &Value, method: &str) -> bool {
    message.get("method").and_then(|m| m.as_str()) == Some(method)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assembler_joins_fragments_across_chunks() {
        let mut asm = AcpAssembler::new();
        assert_eq!(
            asm.push(br#"{"jsonrpc":"2.0""#).unwrap(),
            Vec::<Value>::new()
        );
        assert_eq!(
            asm.push(br#","id":1,"result":{"ok":true}}"#).unwrap(),
            Vec::<Value>::new()
        );
        let drained = asm.push(b"\n").unwrap();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0]["id"], json!(1));
    }

    #[test]
    fn assembler_drains_multiple_messages_and_keeps_partial_tail() {
        let mut asm = AcpAssembler::new();
        let first = json!({"a": 1}).to_string();
        let second = json!({"b": 2}).to_string();
        let mut bytes = first.clone().into_bytes();
        bytes.push(b'\n');
        bytes.extend(second.as_bytes());
        // only the first line is complete; second lacks its newline
        let drained = asm.push(&bytes).unwrap();
        assert_eq!(drained, vec![json!({"a": 1})]);
        let drained = asm.push(b"\n").unwrap();
        assert_eq!(drained, vec![json!({"b": 2})]);
    }

    #[test]
    fn assembler_handles_crlf_and_blank_lines() {
        let mut asm = AcpAssembler::new();
        let mut bytes = b"\r\n".to_vec();
        bytes.extend(json!({"c": 3}).to_string().into_bytes());
        bytes.extend(b"\r\n");
        let drained = asm.push(&bytes).unwrap();
        assert_eq!(drained, vec![json!({"c": 3})]);
    }

    #[test]
    fn invalid_json_is_an_error_not_a_panic() {
        let mut asm = AcpAssembler::new();
        assert!(asm.push(b"not-json\n").is_err());
    }

    #[test]
    fn oversized_messages_fail_fast_and_reset() {
        let mut asm = AcpAssembler::new();
        let chunk = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        assert!(matches!(
            asm.push(&chunk),
            Err(AcpWireError::Oversized { .. })
        ));
        // state resets: small messages work again
        assert_eq!(asm.push(b"\n").unwrap(), Vec::<Value>::new());
    }

    #[test]
    fn request_encoding_matches_jsonrpc_envelope() {
        let text = encode_request(7, "initialize", json!({"protocolVersion": 1}));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 7);
        assert_eq!(value["method"], "initialize");
        assert_eq!(value["params"]["protocolVersion"], 1);
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn an_error_response_carries_the_reason_and_the_method_code() {
        let text = encode_error(json!(9), METHOD_NOT_FOUND, "unsupported interaction");
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["id"], 9);
        assert_eq!(value["error"]["code"], -32601);
        assert_eq!(value["error"]["message"], "unsupported interaction");
        assert!(value.get("result").is_none());
    }

    #[test]
    fn response_and_method_detection() {
        let text = encode_response(json!(3), json!({"granted": true}));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["id"], 3);
        let request = json!({"method": "session/prompt", "id": 4});
        assert!(is_method(&request, "session/prompt"));
        assert!(!is_method(&request, "initialize"));
    }
}
