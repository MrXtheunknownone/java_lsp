mod common;

use common::{receive, send};
use serde_json::json;
use std::io::BufReader;
use std::process::{Command, Stdio};

#[test]
fn opening_a_file_with_a_syntax_error_publishes_a_diagnostic() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_java-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn server");

    let mut stdin = child.stdin.take().expect("server stdin is piped");
    let stdout = child.stdout.take().expect("server stdout is piped");
    let mut reader = BufReader::new(stdout);

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"processId": null, "rootUri": null, "capabilities": {}}
        }),
    );
    receive(&mut reader);

    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///Main.java",
                    "languageId": "java",
                    "version": 1,
                    "text": "class Main { void run() { int x = 1 } }"
                }
            }
        }),
    );

    let publish = receive(&mut reader);
    assert_eq!(publish["method"], json!("textDocument/publishDiagnostics"));
    assert_eq!(publish["params"]["uri"], json!("file:///Main.java"));
    let diagnostics = publish["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert!(!diagnostics.is_empty());
    assert_eq!(diagnostics[0]["severity"], json!(1));

    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
    );
    receive(&mut reader);

    send(&mut stdin, &json!({"jsonrpc": "2.0", "method": "exit"}));
    drop(stdin);

    let status = child.wait().expect("failed to wait for server");
    assert!(status.success());
}
