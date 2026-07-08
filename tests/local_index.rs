mod common;

use common::{receive, send};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn testbed_root() -> String {
    format!("{}/testbed", env!("CARGO_MANIFEST_DIR"))
}

/// Finds the 0-indexed (line, character) of the byte `offset_into_needle` bytes
/// into the first occurrence of `needle` in `text`. Assumes ASCII content, where
/// byte offsets and UTF-16 character offsets coincide.
fn position_within(text: &str, needle: &str, offset_into_needle: usize) -> (u32, u32) {
    let byte_offset = text.find(needle).expect("needle not found in text") + offset_into_needle;
    let line_start = text[..byte_offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = text[..line_start].matches('\n').count() as u32;
    let character = (byte_offset - line_start) as u32;
    (line, character)
}

fn poll_definition(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    uri: &str,
    line: u32,
    character: u32,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut id = 100;
    loop {
        send(
            stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "textDocument/definition",
                "params": {
                    "textDocument": {"uri": uri},
                    "position": {"line": line, "character": character}
                }
            }),
        );
        let response = receive(reader);
        let resolved_in_greeter_file = response["result"].as_array().is_some_and(|locations| {
            locations
                .iter()
                .any(|location| location["uri"].as_str().unwrap().ends_with("Greeter.java"))
        });
        if resolved_in_greeter_file {
            return response;
        }
        assert!(
            Instant::now() < deadline,
            "textDocument/definition did not resolve within the timeout"
        );
        std::thread::sleep(Duration::from_millis(50));
        id += 1;
    }
}

#[test]
fn navigating_and_completing_workspace_symbols_works_against_the_testbed() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_java-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn server");

    let mut stdin = child.stdin.take().expect("server stdin is piped");
    let stdout = child.stdout.take().expect("server stdout is piped");
    let mut reader = BufReader::new(stdout);

    let root_uri = format!("file://{}", testbed_root());
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"processId": null, "rootUri": root_uri, "capabilities": {}}
        }),
    );
    receive(&mut reader);

    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
    );

    let main_path = format!(
        "{}/src/main/java/dev/javalsp/testbed/Main.java",
        testbed_root()
    );
    let main_uri = format!("file://{main_path}");
    let main_text = std::fs::read_to_string(&main_path).expect("testbed Main.java exists");

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": main_uri,
                    "languageId": "java",
                    "version": 1,
                    "text": main_text
                }
            }
        }),
    );
    let publish = receive(&mut reader);
    assert_eq!(publish["method"], json!("textDocument/publishDiagnostics"));
    assert!(
        publish["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Greeter.java is never opened — this exercises the background workspace scan
    // reading it from disk, not the didOpen-triggered reindex path.
    let (import_line, import_character) = position_within(&main_text, "Greeter", 3);
    let definition = poll_definition(
        &mut stdin,
        &mut reader,
        &main_uri,
        import_line,
        import_character,
    );
    // Name-based resolution (no type-checking at this tier) legitimately returns
    // every workspace symbol named "Greeter" — the class, its same-named
    // constructor, and the import statement itself — not just one; poll_definition
    // already waits until a Greeter.java location is among them.
    assert!(!definition["result"].as_array().unwrap().is_empty());

    let (hover_line, hover_character) = position_within(&main_text, "greeter.greet()", 9);
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 200,
            "method": "textDocument/hover",
            "params": {
                "textDocument": {"uri": main_uri},
                "position": {"line": hover_line, "character": hover_character}
            }
        }),
    );
    let hover = receive(&mut reader);
    assert!(!hover["result"].is_null());

    let (completion_line, completion_character) = position_within(&main_text, "new Greeter", 6);
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 201,
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": main_uri},
                "position": {"line": completion_line, "character": completion_character}
            }
        }),
    );
    let completion = receive(&mut reader);
    let items = completion["result"].as_array().expect("result is array");
    assert!(items.iter().any(|item| item["label"] == json!("Greeter")));

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
