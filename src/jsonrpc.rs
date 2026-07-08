use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Request {
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Notification {
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Incoming {
    Request(Request),
    Notification(Notification),
}

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const INVALID_PARAMS: i64 = -32602;

pub fn parse_incoming(body: &[u8]) -> Result<Incoming, Response> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| Response::error(Value::Null, PARSE_ERROR, "parse error"))?;
    serde_json::from_value(value.clone()).map_err(|_| {
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        Response::error(id, INVALID_REQUEST, "invalid request")
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Response {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ResponseError>,
}

impl Response {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(ResponseError {
                code,
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutgoingNotification {
    jsonrpc: &'static str,
    method: String,
    params: Value,
}

impl OutgoingNotification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            method: method.into(),
            params,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deserializing_request_shaped_json_yields_request() {
        let value = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});

        let incoming: Incoming = serde_json::from_value(value).unwrap();

        match incoming {
            Incoming::Request(request) => {
                assert_eq!(request.id, json!(1));
                assert_eq!(request.method, "initialize");
            }
            Incoming::Notification(_) => panic!("expected a Request, got a Notification"),
        }
    }

    #[test]
    fn deserializing_notification_shaped_json_yields_notification() {
        let value = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});

        let incoming: Incoming = serde_json::from_value(value).unwrap();

        match incoming {
            Incoming::Notification(notification) => {
                assert_eq!(notification.method, "initialized");
            }
            Incoming::Request(_) => panic!("expected a Notification, got a Request"),
        }
    }

    #[test]
    fn success_response_serializes_to_expected_shape() {
        let response = Response::success(json!(1), json!({"capabilities": {}}));

        let value = serde_json::to_value(response).unwrap();

        assert_eq!(
            value,
            json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": {}}})
        );
    }

    #[test]
    fn outgoing_notification_serializes_to_expected_shape() {
        let notification = OutgoingNotification::new(
            "textDocument/publishDiagnostics",
            json!({"uri": "file:///Main.java", "diagnostics": []}),
        );

        let value = serde_json::to_value(notification).unwrap();

        assert_eq!(
            value,
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": {"uri": "file:///Main.java", "diagnostics": []}
            })
        );
    }

    #[test]
    fn error_response_serializes_to_expected_shape() {
        let response = Response::error(json!(1), -32002, "server not initialized");

        let value = serde_json::to_value(response).unwrap();

        assert_eq!(
            value,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32002, "message": "server not initialized"}
            })
        );
    }

    #[test]
    fn parse_incoming_returns_parse_error_for_invalid_json() {
        let response = parse_incoming(b"not json").unwrap_err();

        let value = serde_json::to_value(response).unwrap();

        assert_eq!(value["id"], Value::Null);
        assert_eq!(value["error"]["code"], json!(PARSE_ERROR));
    }

    #[test]
    fn parse_incoming_returns_invalid_request_with_extracted_id_for_malformed_body() {
        let response = parse_incoming(br#"{"jsonrpc":"2.0","id":1}"#).unwrap_err();

        let value = serde_json::to_value(response).unwrap();

        assert_eq!(value["id"], json!(1));
        assert_eq!(value["error"]["code"], json!(INVALID_REQUEST));
    }
}
