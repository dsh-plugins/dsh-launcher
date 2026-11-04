//! A deliberately small, GUI-independent Office MCP server.
//!
//! This library creates new DOCX paragraph documents and XLSX typed-cell
//! workbooks and extracts bounded text/cell data. It does not render documents,
//! evaluate formulas, execute active content, or edit existing Office packages.

mod docx;
mod office;
mod package;
mod workspace;
mod xlsx;

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;
use workspace::Workspace;

pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Serve newline-delimited MCP JSON-RPC on stdin/stdout until EOF.
///
/// `root` must be an explicitly selected, existing absolute directory. Only
/// protocol messages are written to stdout. The caller owns error reporting.
pub fn run(root: &Path) -> Result<(), String> {
    serve(root, std::io::stdin().lock(), std::io::stdout().lock())
}

/// The same server with injectable I/O, suitable for embedding and tests.
pub fn serve<R: BufRead, W: Write>(root: &Path, mut input: R, mut output: W) -> Result<(), String> {
    let workspace = Workspace::new(root)?;
    let mut state = State::New;
    while let Some(line) = read_line(&mut input)? {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let request: Value = match serde_json::from_slice(&line) {
            Ok(request) => request,
            Err(_) => {
                write_response(&mut output, rpc_error(Value::Null, -32700, "Invalid JSON"))?;
                continue;
            }
        };
        if let Some(response) = dispatch(&workspace, &mut state, request) {
            write_response(&mut output, response)?;
        }
    }
    Ok(())
}

fn read_line<R: BufRead>(input: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut line = Vec::new();
    loop {
        let bytes = input.fill_buf().map_err(|e| format!("read request: {e}"))?;
        if bytes.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let length = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |n| n + 1);
        if line.len() + length > MAX_REQUEST_BYTES {
            return Err("request exceeds 1 MiB; closing transport".into());
        }
        line.extend_from_slice(&bytes[..length]);
        let done = bytes[length - 1] == b'\n';
        input.consume(length);
        if done {
            return Ok(Some(line));
        }
    }
}

fn write_response<W: Write>(output: &mut W, response: Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        bytes = serde_json::to_vec(&rpc_error(
            response.get("id").cloned().unwrap_or(Value::Null),
            -32603,
            "Result exceeds response limit; use a smaller document",
        ))
        .map_err(|e| e.to_string())?;
    }
    output
        .write_all(&bytes)
        .and_then(|_| output.write_all(b"\n"))
        .and_then(|_| output.flush())
        .map_err(|e| format!("write response: {e}"))
}

#[derive(PartialEq)]
enum State {
    New,
    Initializing,
    Ready,
}

fn dispatch(workspace: &Workspace, state: &mut State, request: Value) -> Option<Value> {
    let id = request.get("id").cloned();
    if !request.is_object()
        || request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || !request.get("method").is_some_and(Value::is_string)
        || id
            .as_ref()
            .is_some_and(|id| !(id.is_string() || id.is_number()))
        || request.get("params").is_some_and(|p| !p.is_object())
    {
        return Some(rpc_error(
            id.filter(|id| id.is_string() || id.is_number())
                .unwrap_or(Value::Null),
            -32600,
            "Invalid JSON-RPC request; batches are unsupported",
        ));
    }
    let method = request["method"].as_str().unwrap();
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let Some(id) = id else {
        if method == "notifications/initialized" && *state == State::Initializing {
            *state = State::Ready;
        }
        return None;
    };
    let result = match method {
        "initialize" => {
            if *state != State::New {
                return Some(rpc_error(id, -32600, "Already initialized"));
            }
            let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
                return Some(rpc_error(id, -32602, "protocolVersion is required"));
            };
            if !params.get("capabilities").is_some_and(Value::is_object)
                || !params.get("clientInfo").is_some_and(Value::is_object)
            {
                return Some(rpc_error(
                    id,
                    -32602,
                    "capabilities and clientInfo are required",
                ));
            }
            let version = match requested {
                "2024-11-05" | "2025-03-26" | "2025-06-18" => requested,
                _ => PROTOCOL_VERSION,
            };
            *state = State::Initializing;
            json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},
                "serverInfo":{"name":"dsh-office","version":env!("CARGO_PKG_VERSION")},
                "instructions":"Local Rust-only Office tools. Paths are relative to the selected workspace. Creates never overwrite. Text/cells only: no formula evaluation, rendering, macros, external relationships, or full-fidelity editing."})
        }
        "ping" => json!({}),
        _ if *state != State::Ready => {
            return Some(rpc_error(
                id,
                -32002,
                "Send initialize and notifications/initialized first",
            ))
        }
        "tools/list" => {
            if params.get("cursor").is_some() {
                return Some(rpc_error(
                    id,
                    -32602,
                    "Tool pagination cursors are unsupported",
                ));
            }
            json!({"tools":office::tool_definitions()})
        }
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(rpc_error(id, -32602, "Tool name is required"));
            };
            if !office::has_tool(name) {
                return Some(rpc_error(id, -32602, "Unknown tool"));
            }
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match office::call(workspace, name, arguments) {
                Ok(data) => {
                    json!({"content":[{"type":"text","text":data.to_string()}],"isError":false})
                }
                Err(error) => json!({"content":[{"type":"text","text":error}],"isError":true}),
            }
        }
        _ => return Some(rpc_error(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
