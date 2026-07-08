mod common;

use common::{receive, send};
use serde_json::{Value, json};
use std::io::BufReader;
use std::process::{Command, Stdio};

#[test]
fn full_handshake_sequence_completes_and_exits_cleanly() {
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
    let initialize_response = receive(&mut reader);
    assert!(initialize_response["result"]["capabilities"].is_object());

    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
    );

    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
    );
    let shutdown_response = receive(&mut reader);
    assert_eq!(shutdown_response["result"], Value::Null);

    send(&mut stdin, &json!({"jsonrpc": "2.0", "method": "exit"}));
    drop(stdin);

    let status = child.wait().expect("failed to wait for server");
    assert!(status.success());
}
