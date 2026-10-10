//! JSON-RPC 2.0 over newline-delimited stdio, as the Model Context Protocol's stdio transport specifies:
//! one message per line, UTF-8, no embedded newlines. Requests get one response line each; notifications
//! get none.

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

use crate::backend::Backend;
use crate::tools;

/// Largest request line accepted (bigger lines get an "Invalid Request" error and are skipped).
pub const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// Newest protocol revision this server implements.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Revisions a client may ask for (newest first).
pub const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC error codes.
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
}

/// A JSON-RPC error.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

/// The server: protocol state plus the backend tool calls act on.
pub struct Server<B: Backend> {
    backend: B,
    protocol: Option<String>,
    client: Option<Value>,
}

impl<B: Backend> std::fmt::Debug for Server<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("backend", &self.backend.describe())
            .field("protocol", &self.protocol)
            .finish()
    }
}

fn error_response(id: Value, e: &RpcError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": e.code, "message": e.message}})
}

impl<B: Backend> Server<B> {
    pub fn new(backend: B) -> Self {
        Self { backend, protocol: None, client: None }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Protocol revision agreed in `initialize` (None before it).
    pub fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }

    /// `clientInfo` from `initialize`.
    pub fn client_info(&self) -> Option<&Value> {
        self.client.as_ref()
    }

    /// Handles one input line; returns the response to write, if any (notifications and client responses
    /// get none).
    pub fn handle_line(&mut self, line: &str) -> Option<Value> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(error_response(
                    Value::Null,
                    &RpcError::new(code::PARSE_ERROR, format!("parse error: {e}")),
                ));
            }
        };
        match msg {
            Value::Array(batch) => {
                if batch.is_empty() {
                    return Some(error_response(
                        Value::Null,
                        &RpcError::new(code::INVALID_REQUEST, "empty batch"),
                    ));
                }
                let out: Vec<Value> = batch.into_iter().filter_map(|m| self.handle_message(m)).collect();
                (!out.is_empty()).then_some(Value::Array(out))
            }
            other => self.handle_message(other),
        }
    }

    /// Handles one parsed message.
    pub fn handle_message(&mut self, msg: Value) -> Option<Value> {
        let Value::Object(obj) = msg else {
            return Some(error_response(
                Value::Null,
                &RpcError::new(code::INVALID_REQUEST, "a message must be a JSON object"),
            ));
        };
        let id = obj.get("id").cloned();
        let method = obj.get("method").and_then(Value::as_str);
        let Some(method) = method else {
            // A response to a request we never sent (servers here send none), or junk.
            if obj.contains_key("result") || obj.contains_key("error") {
                return None;
            }
            return Some(error_response(
                id.filter(valid_id).unwrap_or(Value::Null),
                &RpcError::new(code::INVALID_REQUEST, "`method` (string) is required"),
            ));
        };
        let Some(id) = id else {
            self.handle_notification(method);
            return None;
        };
        if !valid_id(&id) {
            return Some(error_response(
                Value::Null,
                &RpcError::new(code::INVALID_REQUEST, "`id` must be a string or a number"),
            ));
        }
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error_response(
                id,
                &RpcError::new(code::INVALID_REQUEST, "`jsonrpc` must be \"2.0\""),
            ));
        }
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        if !(params.is_null() || params.is_object()) {
            return Some(error_response(
                id,
                &RpcError::new(code::INVALID_PARAMS, "`params` must be an object"),
            ));
        }
        Some(match self.handle_request(method, &params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => error_response(id, &e),
        })
    }

    fn handle_notification(&mut self, _method: &str) {
        // notifications/initialized, notifications/cancelled (requests run to completion before the next
        // line is read, so there is never anything to cancel), notifications/roots/list_changed: nothing to do.
    }

    fn handle_request(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(self.initialize(params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools::definitions()})),
            "tools/call" => self.call_tool(params),
            "resources/list" => Ok(json!({"resources": []})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            other => Err(RpcError::new(code::METHOD_NOT_FOUND, format!("method not found: {other}"))),
        }
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
        let version = SUPPORTED_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSION);
        self.protocol = Some(version.to_owned());
        self.client = params.get("clientInfo").cloned();
        json!({
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "mailcraft", "title": "MailCraft", "version": env!("CARGO_PKG_VERSION")},
            "instructions": format!(
                "MailCraft mail client ({}). Everything MailCraft can do is a command: list_commands shows them, \
                 run_command runs one by id. list_folders / list_messages / read_message read mail; select picks \
                 the folder and message commands act on; search filters the list. Mail is synthetic in the \
                 sample mailbox; in a real profile, commands change real data (undo with app.undo).",
                self.backend.describe()
            ),
        })
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "`name` (string) is required"))?;
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
        let outcome = tools::call(&mut self.backend, name, &args)
            .ok_or_else(|| RpcError::new(code::INVALID_PARAMS, format!("unknown tool: {name}")))?;
        Ok(match outcome {
            Ok(value) => {
                // Text content only: no tool declares an outputSchema, and clients show the text to the model.
                let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
                json!({"content": [{"type": "text", "text": text}], "isError": false})
            }
            Err(e) => json!({"content": [{"type": "text", "text": e.0}], "isError": true}),
        })
    }
}

fn valid_id(id: &Value) -> bool {
    id.is_string() || id.is_number()
}

/// What [`read_line_capped`] read.
#[derive(Debug, PartialEq, Eq)]
pub enum LineRead {
    Eof,
    /// A complete line (or the last, unterminated one) is in the buffer, without its `\n`.
    Line,
    /// The line was longer than the cap; it was consumed and discarded.
    TooLong,
}

/// Reads one `\n`-terminated line into `out` (cleared first) without ever holding more than `cap` bytes.
pub fn read_line_capped(reader: &mut impl BufRead, out: &mut Vec<u8>, cap: usize) -> io::Result<LineRead> {
    out.clear();
    let mut too_long = false;
    let mut saw_any = false;
    loop {
        let buf = match reader.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            return Ok(if !saw_any {
                LineRead::Eof
            } else if too_long {
                LineRead::TooLong
            } else {
                LineRead::Line
            });
        }
        saw_any = true;
        let (chunk, used, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(i) => (buf.get(..i).unwrap_or(&[]), i.saturating_add(1), true),
            None => (buf, buf.len(), false),
        };
        if !too_long {
            if out.len().saturating_add(chunk.len()) > cap {
                too_long = true;
                out.clear();
            } else {
                out.extend_from_slice(chunk);
            }
        }
        reader.consume(used);
        if done {
            return Ok(if too_long { LineRead::TooLong } else { LineRead::Line });
        }
    }
}

/// Serves requests from `reader` until end of input, writing one response line per request to `writer`.
pub fn serve<B: Backend>(
    server: &mut Server<B>,
    mut reader: impl BufRead,
    mut writer: impl Write,
) -> io::Result<()> {
    let mut buf = Vec::new();
    loop {
        let response = match read_line_capped(&mut reader, &mut buf, MAX_REQUEST_BYTES)? {
            LineRead::Eof => return Ok(()),
            LineRead::TooLong => Some(error_response(
                Value::Null,
                &RpcError::new(
                    code::INVALID_REQUEST,
                    format!("request larger than {MAX_REQUEST_BYTES} bytes"),
                ),
            )),
            LineRead::Line => match std::str::from_utf8(&buf) {
                Ok(line) => server.handle_line(line),
                Err(_) => Some(error_response(
                    Value::Null,
                    &RpcError::new(code::PARSE_ERROR, "parse error: the line is not UTF-8"),
                )),
            },
        };
        if let Some(resp) = response {
            let mut line = serde_json::to_vec(&resp).map_err(io::Error::other)?;
            line.push(b'\n');
            writer.write_all(&line)?;
            writer.flush()?;
        }
    }
}

/// Serves on this process's stdin/stdout (the MCP stdio transport). Diagnostics go to stderr.
pub fn serve_stdio<B: Backend>(server: &mut Server<B>) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve(server, stdin.lock(), stdout.lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn capped_reader_skips_long_lines() {
        let mut r = Cursor::new(b"abc\nlonglonglong\nxy".to_vec());
        let mut buf = Vec::new();
        assert_eq!(read_line_capped(&mut r, &mut buf, 5).unwrap(), LineRead::Line);
        assert_eq!(buf, b"abc");
        assert_eq!(read_line_capped(&mut r, &mut buf, 5).unwrap(), LineRead::TooLong);
        assert!(buf.is_empty());
        assert_eq!(read_line_capped(&mut r, &mut buf, 5).unwrap(), LineRead::Line);
        assert_eq!(buf, b"xy");
        assert_eq!(read_line_capped(&mut r, &mut buf, 5).unwrap(), LineRead::Eof);
    }

    #[test]
    fn capped_reader_across_small_buffers() {
        let data = b"0123456789\nab\n".to_vec();
        let mut r = io::BufReader::with_capacity(3, Cursor::new(data));
        let mut buf = Vec::new();
        assert_eq!(read_line_capped(&mut r, &mut buf, 10).unwrap(), LineRead::Line);
        assert_eq!(buf, b"0123456789");
        assert_eq!(read_line_capped(&mut r, &mut buf, 10).unwrap(), LineRead::Line);
        assert_eq!(buf, b"ab");
        assert_eq!(read_line_capped(&mut r, &mut buf, 10).unwrap(), LineRead::Eof);
    }
}
