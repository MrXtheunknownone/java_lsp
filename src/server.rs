use crate::build_tool::{self, BuildTool};
use crate::documents::DocumentStore;
use crate::handshake::{ExitAction, Handshake};
use crate::jsonrpc::{INVALID_PARAMS, Notification, OutgoingNotification, Request, Response};
use crate::project_model::ProjectModel;
use crate::symbol::extract_symbols;
use crate::workspace_index::WorkspaceIndex;
use crate::{
    completion, external_index, goto_definition, gradle, hover, jdk_home, maven, workspace_scan,
};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Initialized,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{Completion, GotoDefinition, HoverRequest, Initialize, Request as _};
use lsp_types::{
    CompletionParams, Diagnostic, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, GotoDefinitionParams, HoverParams, InitializeParams,
    PublishDiagnosticsParams, Uri,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

pub struct Server {
    handshake: Handshake,
    documents: DocumentStore,
    index: Arc<Mutex<WorkspaceIndex>>,
    project_model: Arc<Mutex<Option<ProjectModel>>>,
    runtime: tokio::runtime::Handle,
    workspace_root: Option<Uri>,
    /// Bumped on every `didOpen`, independent of the client-supplied LSP
    /// version. See `workspace_index::WorkspaceIndex`'s `Applied` type for why.
    next_generation: u64,
    document_generations: HashMap<Uri, u64>,
}

impl Server {
    pub fn new(runtime: tokio::runtime::Handle) -> Self {
        Self {
            handshake: Handshake::new(),
            documents: DocumentStore::new(),
            index: Arc::new(Mutex::new(WorkspaceIndex::new())),
            project_model: Arc::new(Mutex::new(None)),
            runtime,
            workspace_root: None,
            next_generation: 0,
            document_generations: HashMap::new(),
        }
    }

    pub fn handle_request(&mut self, request: &Request) -> Response {
        if request.method == Initialize::METHOD {
            self.workspace_root = parse_workspace_root(request);
        }

        if self.handshake.is_initialized() {
            match request.method.as_str() {
                GotoDefinition::METHOD => return self.handle_goto_definition(request),
                HoverRequest::METHOD => return self.handle_hover(request),
                Completion::METHOD => return self.handle_completion(request),
                _ => {}
            }
        }

        self.handshake.handle_request(request)
    }

    pub fn handle_notification(
        &mut self,
        notification: &Notification,
    ) -> (ExitAction, Vec<OutgoingNotification>) {
        if self.handshake.is_initialized() {
            match notification.method.as_str() {
                DidOpenTextDocument::METHOD => {
                    return (ExitAction::Continue, self.handle_did_open(notification));
                }
                DidChangeTextDocument::METHOD => {
                    return (ExitAction::Continue, self.handle_did_change(notification));
                }
                DidCloseTextDocument::METHOD => {
                    return (ExitAction::Continue, self.handle_did_close(notification));
                }
                Initialized::METHOD => {
                    self.trigger_workspace_scan();
                    self.trigger_build_resolution();
                    return (ExitAction::Continue, Vec::new());
                }
                _ => {}
            }
        }

        (self.handshake.handle_notification(notification), Vec::new())
    }

    fn handle_goto_definition(&self, request: &Request) -> Response {
        let params = match parse_request_params::<GotoDefinitionParams>(request) {
            Ok(params) => params,
            Err(error_response) => return error_response,
        };

        let index = lock(&self.index);
        let result = goto_definition::goto_definition(&index, &self.documents, &params);
        Response::success(
            request.id.clone(),
            serde_json::to_value(result).expect("GotoDefinitionResponse always serializes"),
        )
    }

    fn handle_hover(&self, request: &Request) -> Response {
        let params = match parse_request_params::<HoverParams>(request) {
            Ok(params) => params,
            Err(error_response) => return error_response,
        };

        let index = lock(&self.index);
        let result = hover::hover(&index, &self.documents, &params);
        Response::success(
            request.id.clone(),
            serde_json::to_value(result).expect("Hover always serializes"),
        )
    }

    fn handle_completion(&self, request: &Request) -> Response {
        let params = match parse_request_params::<CompletionParams>(request) {
            Ok(params) => params,
            Err(error_response) => return error_response,
        };

        let index = lock(&self.index);
        let result = completion::completion(&index, &self.documents, &params);
        Response::success(
            request.id.clone(),
            serde_json::to_value(result).expect("CompletionResponse always serializes"),
        )
    }

    fn handle_did_open(&mut self, notification: &Notification) -> Vec<OutgoingNotification> {
        let Some(params) = parse_params::<DidOpenTextDocumentParams>(notification, "didOpen")
        else {
            return Vec::new();
        };

        let uri = params.text_document.uri;
        let version = params.text_document.version;
        self.next_generation += 1;
        let generation = self.next_generation;
        self.document_generations.insert(uri.clone(), generation);
        let diagnostics = self.documents.open(uri.clone(), &params.text_document.text);
        self.spawn_reindex(&uri, generation, version);
        vec![publish_diagnostics_notification(uri, diagnostics)]
    }

    fn handle_did_change(&mut self, notification: &Notification) -> Vec<OutgoingNotification> {
        let Some(params) = parse_params::<DidChangeTextDocumentParams>(notification, "didChange")
        else {
            return Vec::new();
        };

        let uri = params.text_document.uri;
        let version = params.text_document.version;
        let generation = *self.document_generations.get(&uri).unwrap_or(&0);
        let diagnostics = self.documents.change(&uri, &params.content_changes);
        self.spawn_reindex(&uri, generation, version);
        vec![publish_diagnostics_notification(uri, diagnostics)]
    }

    fn handle_did_close(&mut self, notification: &Notification) -> Vec<OutgoingNotification> {
        let Some(params) = parse_params::<DidCloseTextDocumentParams>(notification, "didClose")
        else {
            return Vec::new();
        };

        // No index interaction needed here: a later reopen bumps `next_generation`
        // to a fresh, higher value, so any reindex still in flight from before this
        // close (however late it completes) can never outrank a subsequent session.
        let uri = params.text_document.uri;
        self.documents.close(&uri);
        vec![publish_diagnostics_notification(uri, Vec::new())]
    }

    fn spawn_reindex(&self, uri: &Uri, generation: u64, version: i32) {
        let Some(document) = self.documents.document(uri) else {
            return;
        };

        let uri = uri.clone();
        let tree = document.tree().clone();
        let source = document.source().to_string();
        let index = Arc::clone(&self.index);

        self.runtime.spawn_blocking(move || {
            if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let symbols = extract_symbols(&uri, &source, &tree);
                let mut guard = lock(&index);
                guard.update_file(uri, generation, version, symbols);
            })) {
                eprintln!("background reindex panicked: {panic:?}");
            }
        });
    }

    fn trigger_workspace_scan(&self) {
        let Some(root_path) = self
            .workspace_root
            .as_ref()
            .and_then(workspace_scan::uri_to_path)
        else {
            return;
        };

        let index = Arc::clone(&self.index);
        self.runtime.spawn_blocking(move || {
            if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                workspace_scan::index_workspace(&root_path, &index);
            })) {
                eprintln!("background workspace scan panicked: {panic:?}");
            }
        });
    }

    fn trigger_build_resolution(&self) {
        let Some(root_path) = self
            .workspace_root
            .as_ref()
            .and_then(workspace_scan::uri_to_path)
        else {
            return;
        };

        let Some(tool) = build_tool::detect(&root_path) else {
            return;
        };

        let project_model = Arc::clone(&self.project_model);
        let index = Arc::clone(&self.index);
        self.runtime.spawn_blocking(move || {
            if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let resolved = match tool {
                    BuildTool::Gradle => gradle::resolve_project_model(&root_path),
                    BuildTool::Maven => maven::resolve_project_model(&root_path),
                };
                match resolved {
                    Ok(model) => {
                        eprintln!(
                            "resolved project model: {} module(s), tool={tool:?}",
                            model.modules.len()
                        );
                        *lock(&project_model) = Some(model.clone());
                        index_external_symbols(&model, &index);
                    }
                    Err(err) => {
                        eprintln!("failed to resolve project model with {tool:?}: {err}");
                    }
                }
            })) {
                eprintln!("background build resolution panicked: {panic:?}");
            }
        });
    }
}

/// Feeds Tier 2 with symbols from every resolved dependency jar and the JDK
/// (Tier 3b) — runs after `project_model` is already visible, so a slow
/// JDK/jar indexing pass never delays the project model itself becoming
/// available to a request that only needs that.
fn index_external_symbols(model: &ProjectModel, index: &Arc<Mutex<WorkspaceIndex>>) {
    let cache_root = external_index::default_cache_root();

    if let Some(jdk) = jdk_home::locate() {
        external_index::index_jdk(&jdk, &cache_root, index);
    }

    let mut indexed = HashSet::new();
    for module in &model.modules {
        for entry in &module.classpath {
            if indexed.insert(entry.clone()) {
                external_index::index_classpath_entry(entry, &cache_root, index);
            }
        }
    }
}

/// Locks `mutex`, recovering from poisoning rather than propagating it — a
/// panic in one background task shouldn't permanently wedge shared state for
/// every future task.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn parse_workspace_root(request: &Request) -> Option<Uri> {
    let params: InitializeParams = serde_json::from_value(request.params.clone()).ok()?;
    if let Some(folder) = params
        .workspace_folders
        .and_then(|folders| folders.into_iter().next())
    {
        return Some(folder.uri);
    }

    #[allow(deprecated)]
    params.root_uri
}

fn parse_request_params<T: serde::de::DeserializeOwned>(request: &Request) -> Result<T, Response> {
    serde_json::from_value(request.params.clone()).map_err(|err| {
        Response::error(
            request.id.clone(),
            INVALID_PARAMS,
            format!("invalid params: {err}"),
        )
    })
}

fn parse_params<T: serde::de::DeserializeOwned>(
    notification: &Notification,
    event: &str,
) -> Option<T> {
    match serde_json::from_value(notification.params.clone()) {
        Ok(params) => Some(params),
        Err(err) => {
            eprintln!("ignoring malformed {event} notification: {err}");
            None
        }
    }
}

fn publish_diagnostics_notification(
    uri: Uri,
    diagnostics: Vec<Diagnostic>,
) -> OutgoingNotification {
    let params = PublishDiagnosticsParams::new(uri, diagnostics, None);
    OutgoingNotification::new(
        PublishDiagnostics::METHOD,
        serde_json::to_value(params).expect("PublishDiagnosticsParams always serializes"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("failed to build test runtime")
    }

    fn wait_until(condition: impl FnMut() -> bool) {
        wait_until_timeout(condition, std::time::Duration::from_secs(5));
    }

    fn wait_until_timeout(mut condition: impl FnMut() -> bool, timeout: std::time::Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while !condition() {
            assert!(
                std::time::Instant::now() < deadline,
                "condition did not become true in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn request(id: i64, method: &str, params: Value) -> Request {
        Request {
            id: json!(id),
            method: method.to_string(),
            params,
        }
    }

    fn notification(method: &str, params: Value) -> Notification {
        Notification {
            method: method.to_string(),
            params,
        }
    }

    fn initialize(server: &mut Server) {
        server.handle_request(&request(1, "initialize", json!({})));
    }

    #[test]
    fn did_open_while_initialized_publishes_diagnostics_for_broken_source() {
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());
        initialize(&mut server);

        let (exit, outgoing) = server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Main.java",
                    "languageId": "java",
                    "version": 1,
                    "text": "class Main {"
                }
            }),
        ));

        assert_eq!(exit, ExitAction::Continue);
        assert_eq!(outgoing.len(), 1);
        let value = serde_json::to_value(&outgoing[0]).unwrap();
        assert_eq!(value["method"], json!("textDocument/publishDiagnostics"));
        assert_eq!(value["params"]["uri"], json!("file:///Main.java"));
        assert!(
            !value["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn did_open_before_initialize_is_ignored() {
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());

        let (exit, outgoing) = server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Main.java",
                    "languageId": "java",
                    "version": 1,
                    "text": "class Main {"
                }
            }),
        ));

        assert_eq!(exit, ExitAction::Continue);
        assert!(outgoing.is_empty());
    }

    #[test]
    fn did_change_publishes_updated_diagnostics() {
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());
        initialize(&mut server);
        server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Main.java",
                    "languageId": "java",
                    "version": 1,
                    "text": "class Main {"
                }
            }),
        ));

        let (_, outgoing) = server.handle_notification(&notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///Main.java", "version": 2},
                "contentChanges": [{"text": "class Main {}"}]
            }),
        ));

        assert_eq!(outgoing.len(), 1);
        let value = serde_json::to_value(&outgoing[0]).unwrap();
        assert!(
            value["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn did_close_publishes_empty_diagnostics_to_clear_stale_errors() {
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());
        initialize(&mut server);
        server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Main.java",
                    "languageId": "java",
                    "version": 1,
                    "text": "class Main {"
                }
            }),
        ));

        let (exit, outgoing) = server.handle_notification(&notification(
            "textDocument/didClose",
            json!({"textDocument": {"uri": "file:///Main.java"}}),
        ));

        assert_eq!(exit, ExitAction::Continue);
        assert_eq!(outgoing.len(), 1);
        let value = serde_json::to_value(&outgoing[0]).unwrap();
        assert_eq!(value["method"], json!("textDocument/publishDiagnostics"));
        assert_eq!(value["params"]["uri"], json!("file:///Main.java"));
        assert!(
            value["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reopening_a_closed_document_with_a_lower_version_number_still_reindexes() {
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());
        initialize(&mut server);

        server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Main.java",
                    "languageId": "java",
                    "version": 10,
                    "text": "class Old {}"
                }
            }),
        ));
        wait_until(|| !server.index.lock().unwrap().lookup("Old").is_empty());

        server.handle_notification(&notification(
            "textDocument/didClose",
            json!({"textDocument": {"uri": "file:///Main.java"}}),
        ));

        // A fresh editor session restarts version numbering from 1.
        server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Main.java",
                    "languageId": "java",
                    "version": 1,
                    "text": "class New {}"
                }
            }),
        ));
        wait_until(|| !server.index.lock().unwrap().lookup("New").is_empty());

        assert!(server.index.lock().unwrap().lookup("Old").is_empty());
    }

    #[test]
    fn exit_after_shutdown_still_routes_through_handshake() {
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());
        initialize(&mut server);
        server.handle_request(&request(2, "shutdown", Value::Null));

        let (exit, outgoing) = server.handle_notification(&notification("exit", Value::Null));

        assert_eq!(exit, ExitAction::Exit(0));
        assert!(outgoing.is_empty());
    }

    #[test]
    fn goto_definition_resolves_a_symbol_indexed_from_a_reindexed_open_document() {
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());
        initialize(&mut server);

        server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Greeter.java",
                    "languageId": "java",
                    "version": 1,
                    "text": "class Greeter {}"
                }
            }),
        ));
        server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Main.java",
                    "languageId": "java",
                    "version": 1,
                    "text": "class Main { Greeter g; }"
                }
            }),
        ));

        wait_until(|| !server.index.lock().unwrap().lookup("Greeter").is_empty());

        let response = server.handle_request(&request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///Main.java"},
                "position": {"line": 0, "character": "class Main { Gree".len()}
            }),
        ));
        let value = serde_json::to_value(&response).unwrap();

        assert!(value["result"].is_array());
        assert_eq!(value["result"][0]["uri"], json!("file:///Greeter.java"));
    }

    #[test]
    fn initialized_triggers_build_resolution_for_a_real_gradle_fixture() {
        let _guard = crate::test_support::GRADLE_SAMPLE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());

        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gradle_sample");
        let root_uri = format!("file://{}", root.display());

        server.handle_request(&request(
            1,
            "initialize",
            json!({"rootUri": root_uri, "capabilities": {}}),
        ));
        server.handle_notification(&notification("initialized", json!({})));

        // A cold Gradle daemon can genuinely take longer than the 5s default.
        wait_until_timeout(
            || server.project_model.lock().unwrap().is_some(),
            std::time::Duration::from_secs(60),
        );

        let guard = server.project_model.lock().unwrap();
        let model = guard.as_ref().unwrap();
        assert_eq!(model.modules.len(), 2);
    }

    #[test]
    fn external_symbols_resolve_for_jdk_and_third_party_types_from_a_real_gradle_fixture() {
        let _guard = crate::test_support::GRADLE_SAMPLE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());

        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gradle_sample");
        let root_uri = format!("file://{}", root.display());

        server.handle_request(&request(
            1,
            "initialize",
            json!({"rootUri": root_uri, "capabilities": {}}),
        ));
        server.handle_notification(&notification("initialized", json!({})));

        // Third-party (gson, from the resolved classpath) and JDK (java.base,
        // via jimage extraction) symbol resolution both run in the background
        // after the project model resolves — a cold Gradle daemon plus a cold
        // JDK extraction can genuinely take longer than the 5s default. Wait
        // for a real `Class`/`Interface` declaration specifically, not just
        // any symbol named "Gson"/"List" — `Greeter.java`'s own `import
        // com.google.gson.Gson;` is indexed by the (much faster) workspace
        // scan and would otherwise satisfy a same-name-only wait condition
        // well before external resolution actually finishes.
        let timeout = std::time::Duration::from_secs(90);
        wait_until_timeout(
            || {
                server
                    .index
                    .lock()
                    .unwrap()
                    .lookup("Gson")
                    .iter()
                    .any(|s| s.kind == crate::symbol::SymbolKind::Class)
            },
            timeout,
        );
        wait_until_timeout(
            || {
                server
                    .index
                    .lock()
                    .unwrap()
                    .lookup("List")
                    .iter()
                    .any(|s| s.kind == crate::symbol::SymbolKind::Interface)
            },
            timeout,
        );

        let gson_symbols = server.index.lock().unwrap().lookup("Gson").to_vec();
        let gson_class = gson_symbols
            .iter()
            .find(|s| s.kind == crate::symbol::SymbolKind::Class)
            .expect("Gson's Class symbol should be indexed");
        let gson_path = crate::workspace_scan::uri_to_path(&gson_class.uri)
            .expect("Gson's indexed uri should be a real file:// uri");
        let gson_source = std::fs::read_to_string(&gson_path)
            .expect("Gson's stub file should exist on disk and be readable");
        assert!(gson_source.contains("class Gson"));

        let list_symbols = server.index.lock().unwrap().lookup("List").to_vec();
        let list_interface = list_symbols
            .iter()
            .find(|s| s.kind == crate::symbol::SymbolKind::Interface)
            .expect("List's Interface symbol should be indexed");
        let list_path = crate::workspace_scan::uri_to_path(&list_interface.uri)
            .expect("List's indexed uri should be a real file:// uri");
        let list_source = std::fs::read_to_string(&list_path)
            .expect("List's stub file should exist on disk and be readable");
        assert!(list_source.contains("interface List"));
    }

    #[test]
    fn goto_definition_returns_null_when_nothing_resolves() {
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());
        initialize(&mut server);
        server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Main.java",
                    "languageId": "java",
                    "version": 1,
                    "text": "class Main {}"
                }
            }),
        ));

        let response = server.handle_request(&request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///Main.java"},
                "position": {"line": 0, "character": 0}
            }),
        ));
        let value = serde_json::to_value(&response).unwrap();

        assert_eq!(value["result"], Value::Null);
    }
}
