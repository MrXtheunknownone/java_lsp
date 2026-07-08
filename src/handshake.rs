use crate::jsonrpc::{INVALID_REQUEST, Notification, Request, Response};
use lsp_types::error_codes::SERVER_NOT_INITIALIZED;
use lsp_types::notification::{Exit, Notification as _};
use lsp_types::request::{Initialize, Request as _, Shutdown};
use lsp_types::{
    CompletionOptions, HoverProviderCapability, InitializeResult, OneOf, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
};
use serde_json::Value;

const METHOD_NOT_FOUND: i64 = -32601;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Uninitialized,
    Initialized,
    ShuttingDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitAction {
    Continue,
    Exit(i32),
}

pub struct Handshake {
    state: State,
}

impl Handshake {
    pub fn new() -> Self {
        Self {
            state: State::Uninitialized,
        }
    }

    pub fn handle_request(&mut self, request: &Request) -> Response {
        match (self.state, request.method.as_str()) {
            (State::Uninitialized, Initialize::METHOD) => {
                self.state = State::Initialized;
                let result = InitializeResult {
                    capabilities: ServerCapabilities {
                        text_document_sync: Some(TextDocumentSyncCapability::Options(
                            TextDocumentSyncOptions {
                                open_close: Some(true),
                                change: Some(TextDocumentSyncKind::INCREMENTAL),
                                will_save: None,
                                will_save_wait_until: None,
                                save: None,
                            },
                        )),
                        definition_provider: Some(OneOf::Left(true)),
                        hover_provider: Some(HoverProviderCapability::Simple(true)),
                        completion_provider: Some(CompletionOptions::default()),
                        ..ServerCapabilities::default()
                    },
                    server_info: None,
                };
                let result_value =
                    serde_json::to_value(result).expect("InitializeResult always serializes");
                Response::success(request.id.clone(), result_value)
            }
            (State::Uninitialized, _) => Response::error(
                request.id.clone(),
                SERVER_NOT_INITIALIZED,
                "server not initialized",
            ),
            (State::Initialized, Shutdown::METHOD) => {
                self.state = State::ShuttingDown;
                Response::success(request.id.clone(), Value::Null)
            }
            (State::Initialized, Initialize::METHOD) => Response::error(
                request.id.clone(),
                INVALID_REQUEST,
                "server already initialized",
            ),
            (State::ShuttingDown, _) => Response::error(
                request.id.clone(),
                INVALID_REQUEST,
                "server is shutting down",
            ),
            _ => Response::error(
                request.id.clone(),
                METHOD_NOT_FOUND,
                format!("method not found: {}", request.method),
            ),
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.state == State::Initialized
    }

    pub fn handle_notification(&mut self, notification: &Notification) -> ExitAction {
        match notification.method.as_str() {
            Exit::METHOD if self.state == State::ShuttingDown => ExitAction::Exit(0),
            Exit::METHOD => ExitAction::Exit(1),
            _ => ExitAction::Continue,
        }
    }
}

impl Default for Handshake {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::{Notification, Request};
    use serde_json::{Value, json};

    fn request(id: i64, method: &str) -> Request {
        Request {
            id: json!(id),
            method: method.to_string(),
            params: Value::Null,
        }
    }

    fn notification(method: &str) -> Notification {
        Notification {
            method: method.to_string(),
            params: Value::Null,
        }
    }

    #[test]
    fn initialize_while_uninitialized_returns_initialize_result_and_transitions() {
        let mut handshake = Handshake::new();

        let response = handshake.handle_request(&request(1, "initialize"));
        let value = serde_json::to_value(&response).unwrap();

        assert!(value.get("result").unwrap().get("capabilities").is_some());
        assert!(value.get("error").is_none());
    }

    #[test]
    fn initialize_advertises_incremental_text_document_sync() {
        let mut handshake = Handshake::new();

        let response = handshake.handle_request(&request(1, "initialize"));
        let value = serde_json::to_value(&response).unwrap();

        let sync = &value["result"]["capabilities"]["textDocumentSync"];
        assert_eq!(sync["openClose"], json!(true));
        assert_eq!(sync["change"], json!(2));
    }

    #[test]
    fn initialize_advertises_definition_hover_and_completion_support() {
        let mut handshake = Handshake::new();

        let response = handshake.handle_request(&request(1, "initialize"));
        let value = serde_json::to_value(&response).unwrap();

        let capabilities = &value["result"]["capabilities"];
        assert_eq!(capabilities["definitionProvider"], json!(true));
        assert_eq!(capabilities["hoverProvider"], json!(true));
        assert!(capabilities["completionProvider"].is_object());
    }

    #[test]
    fn is_initialized_reflects_state() {
        let mut handshake = Handshake::new();
        assert!(!handshake.is_initialized());

        handshake.handle_request(&request(1, "initialize"));
        assert!(handshake.is_initialized());

        handshake.handle_request(&request(2, "shutdown"));
        assert!(!handshake.is_initialized());
    }

    #[test]
    fn request_other_than_initialize_while_uninitialized_returns_server_not_initialized_error() {
        let mut handshake = Handshake::new();

        let response = handshake.handle_request(&request(1, "shutdown"));
        let value = serde_json::to_value(&response).unwrap();

        assert_eq!(value["error"]["code"], json!(-32002));
    }

    #[test]
    fn unknown_method_while_initialized_returns_method_not_found_error() {
        let mut handshake = Handshake::new();
        handshake.handle_request(&request(1, "initialize"));

        let response = handshake.handle_request(&request(2, "textDocument/hover"));
        let value = serde_json::to_value(&response).unwrap();

        assert_eq!(value["error"]["code"], json!(-32601));
    }

    #[test]
    fn shutdown_while_initialized_returns_null_result_and_transitions() {
        let mut handshake = Handshake::new();
        handshake.handle_request(&request(1, "initialize"));

        let response = handshake.handle_request(&request(2, "shutdown"));
        let value = serde_json::to_value(&response).unwrap();

        assert_eq!(value["result"], Value::Null);
    }

    #[test]
    fn duplicate_initialize_while_initialized_returns_invalid_request_error() {
        let mut handshake = Handshake::new();
        handshake.handle_request(&request(1, "initialize"));

        let response = handshake.handle_request(&request(2, "initialize"));
        let value = serde_json::to_value(&response).unwrap();

        assert_eq!(value["error"]["code"], json!(-32600));
    }

    #[test]
    fn request_while_shutting_down_returns_invalid_request_error() {
        let mut handshake = Handshake::new();
        handshake.handle_request(&request(1, "initialize"));
        handshake.handle_request(&request(2, "shutdown"));

        let response = handshake.handle_request(&request(3, "shutdown"));
        let value = serde_json::to_value(&response).unwrap();

        assert_eq!(value["error"]["code"], json!(-32600));
    }

    #[test]
    fn exit_after_shutdown_exits_with_code_zero() {
        let mut handshake = Handshake::new();
        handshake.handle_request(&request(1, "initialize"));
        handshake.handle_request(&request(2, "shutdown"));

        let action = handshake.handle_notification(&notification("exit"));

        assert!(matches!(action, ExitAction::Exit(0)));
    }

    #[test]
    fn exit_without_shutdown_exits_with_code_one() {
        let mut handshake = Handshake::new();
        handshake.handle_request(&request(1, "initialize"));

        let action = handshake.handle_notification(&notification("exit"));

        assert!(matches!(action, ExitAction::Exit(1)));
    }

    #[test]
    fn initialized_notification_continues() {
        let mut handshake = Handshake::new();
        handshake.handle_request(&request(1, "initialize"));

        let action = handshake.handle_notification(&notification("initialized"));

        assert!(matches!(action, ExitAction::Continue));
    }
}
