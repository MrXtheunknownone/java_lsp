use serde_json::Value;
use std::io::{BufRead, Write};

pub fn send(writer: &mut impl Write, value: &Value) {
    let body = serde_json::to_vec(value).expect("test value always serializes");
    java_lsp::transport::write_message(writer, &body).expect("failed to write message");
}

pub fn receive(reader: &mut impl BufRead) -> Value {
    let body = java_lsp::transport::read_message(reader)
        .expect("failed to read message")
        .expect("server closed stdout before responding");
    serde_json::from_slice(&body).expect("response body is valid JSON")
}
