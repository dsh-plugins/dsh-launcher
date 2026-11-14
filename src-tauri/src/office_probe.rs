//! Bounded, document-free self-check for the launcher-owned Office executable.
//! This is not a general-purpose command runner or a DSH connection test.
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const MAX_OUTPUT: u64 = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

pub async fn check(command: &Path, args: &[String], cwd: &Path) -> Result<(), String> {
    let mut child = tokio::process::Command::new(command)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Cannot start Office runtime: {error}"))?;
    let attempt = tokio::time::timeout(TIMEOUT, async {
        let mut stdin = child.stdin.take().ok_or("Office stdin is unavailable")?;
        let requests = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"dsh-launcher-self-check","version":env!("CARGO_PKG_VERSION")}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"office_info","arguments":{}}}),
        ];
        let input = requests.iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
        stdin.write_all(input.as_bytes()).await.map_err(|e| e.to_string())?;
        stdin.shutdown().await.map_err(|e| e.to_string())?;
        drop(stdin);
        let stdout = child.stdout.take().ok_or("Office stdout is unavailable")?;
        let stderr = child.stderr.take().ok_or("Office stderr is unavailable")?;
        let (out, err, status) = tokio::join!(read_bounded(stdout), read_bounded(stderr), child.wait());
        let out = out?;
        let err = err?;
        if !status.map_err(|e| e.to_string())?.success() {
            return Err(format!("Office runtime exited unsuccessfully: {}", String::from_utf8_lossy(&err).chars().take(512).collect::<String>()));
        }
        validate_output(&out)
    }).await;
    match attempt {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            let _ = child.kill().await;
            Err(error)
        }
        Err(_) => {
            // This only launches our own server, which has no child processes.
            // Wait for termination before returning an unsuccessful self-check.
            let _ = child.kill().await;
            Err("Office runtime self-check timed out after 15 seconds".into())
        }
    }
}

async fn read_bounded(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    reader
        .take(MAX_OUTPUT + 1)
        .read_to_end(&mut output)
        .await
        .map_err(|e| e.to_string())?;
    if output.len() as u64 > MAX_OUTPUT {
        return Err("Office runtime self-check output exceeded 64 KiB".into());
    }
    Ok(output)
}

fn validate_output(output: &[u8]) -> Result<(), String> {
    let text =
        std::str::from_utf8(output).map_err(|_| "Office runtime returned non-UTF-8 output")?;
    let replies: Vec<Value> = text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .map_err(|_| "Office runtime returned invalid protocol output")?;
    if replies.len() != 3
        || replies.iter().enumerate().any(|(index, reply)| {
            reply["jsonrpc"] != "2.0"
                || reply["id"] != json!(index + 1)
                || reply.get("error").is_some()
        })
    {
        return Err("Office runtime self-check received unexpected replies".into());
    }
    if replies[0]["result"]["serverInfo"]["name"] != "dsh-office" {
        return Err("Office runtime identity did not match".into());
    }
    let tools = replies[1]["result"]["tools"]
        .as_array()
        .ok_or("Office runtime did not list tools")?;
    for name in [
        "office_info",
        "office_preview",
        "docx_create",
        "docx_read",
        "xlsx_create",
        "xlsx_read",
    ] {
        if !tools.iter().any(|tool| tool["name"] == name) {
            return Err(format!("Office runtime is missing the {name} tool"));
        }
    }
    if replies[2]["result"]["isError"] != false {
        return Err("Office runtime capability check failed".into());
    }
    let content = replies[2]["result"]["content"][0]["text"]
        .as_str()
        .ok_or("Office capability response is missing")?;
    let info: Value =
        serde_json::from_str(content).map_err(|_| "Office capability response is invalid")?;
    if info["runtime"] != "rust" {
        return Err("Office runtime capability did not match".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replies() -> Vec<Value> {
        vec![
            json!({"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"dsh-office"}}}),
            json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"office_info"},{"name":"office_preview"},{"name":"docx_create"},{"name":"docx_read"},{"name":"xlsx_create"},{"name":"xlsx_read"}]}}),
            json!({"jsonrpc":"2.0","id":3,"result":{"isError":false,"content":[{"type":"text","text":"{\"runtime\":\"rust\"}"}]}}),
        ]
    }

    fn encode(replies: &[Value]) -> Vec<u8> {
        replies
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes()
    }

    #[test]
    fn validates_identity_tools_and_capability_without_documents() {
        assert!(validate_output(&encode(&replies())).is_ok());
        let mut missing = replies();
        missing[1]["result"]["tools"] = json!([]);
        assert!(validate_output(&encode(&missing)).is_err());
        let mut wrong = replies();
        wrong[0]["result"]["serverInfo"]["name"] = json!("other");
        assert!(validate_output(&encode(&wrong)).is_err());
        let mut error = replies();
        error[2]["result"]["isError"] = json!(true);
        assert!(validate_output(&encode(&error)).is_err());
        let mut duplicate = replies();
        duplicate[2]["id"] = json!(2);
        assert!(validate_output(&encode(&duplicate)).is_err());
        assert!(validate_output(b"GUI log\n{}").is_err());
    }

    #[tokio::test]
    async fn caps_output_before_protocol_parsing() {
        assert!(read_bounded(&vec![b'x'; MAX_OUTPUT as usize + 1][..])
            .await
            .is_err());
        assert!(read_bounded(&b"ok"[..]).await.is_ok());
    }
}
