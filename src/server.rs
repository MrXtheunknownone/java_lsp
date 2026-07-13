use crate::build_tool::{self, BuildTool};
use crate::documents::DocumentStore;
use crate::handshake::{ExitAction, Handshake};
use crate::jsonrpc::{INVALID_PARAMS, Notification, OutgoingNotification, Request, Response};
use crate::project_model::ProjectModel;
use crate::symbol::extract_symbols;
use crate::workspace_index::WorkspaceIndex;
use crate::{
    classfile, completion, external_index, goto_definition, gradle, hover, javac_compile,
    javac_fallback, jdk_home, lombok, maven, syntax, workspace_scan,
};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Initialized, Notification as _, PublishDiagnostics,
};
use lsp_types::request::{Completion, GotoDefinition, HoverRequest, Initialize, Request as _};
use lsp_types::{
    CompletionParams, Diagnostic, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, GotoDefinitionParams, HoverParams,
    InitializeParams, PublishDiagnosticsParams, Uri,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tree_sitter::Tree;

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
    /// The content hash last used to trigger a Lombok recompile for a given
    /// document — skips a redundant recompile on a save/open with
    /// byte-identical content.
    lombok_source_hashes: HashMap<Uri, u64>,
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
            lombok_source_hashes: HashMap::new(),
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
                DidSaveTextDocument::METHOD => {
                    return (ExitAction::Continue, self.handle_did_save(notification));
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
        self.trigger_lombok_compile(&uri);
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

    /// The `DocumentStore` is already kept current via `didChange`, so this
    /// doesn't need the save params' (optional) text — it's the trigger
    /// point for the Lombok recompile check, deliberately not `didChange`,
    /// so a real `javac` process isn't spawned on every keystroke.
    fn handle_did_save(&mut self, notification: &Notification) -> Vec<OutgoingNotification> {
        let Some(params) = parse_params::<DidSaveTextDocumentParams>(notification, "didSave")
        else {
            return Vec::new();
        };
        self.trigger_lombok_compile(&params.text_document.uri);
        Vec::new()
    }

    /// Checks whether `uri`'s currently tracked document uses Lombok and,
    /// if so and its content actually changed since the last trigger,
    /// spawns a background `javac`+Lombok compile (see `compile_lombok_file`).
    /// Silently does nothing for a non-Lombok file, an unopened document, or
    /// a project model that hasn't resolved yet — all expected, not errors.
    fn trigger_lombok_compile(&mut self, uri: &Uri) {
        let Some(document) = self.documents.document(uri) else {
            return;
        };
        if !lombok::uses_lombok(document.tree(), document.source()) {
            return;
        }

        let hash = lombok::content_hash(document.source());
        if self.lombok_source_hashes.get(uri) == Some(&hash) {
            return;
        }
        self.lombok_source_hashes.insert(uri.clone(), hash);

        let Some(file_path) = workspace_scan::uri_to_path(uri) else {
            return;
        };
        let tree = document.tree().clone();
        let source = document.source().to_string();
        let project_model = Arc::clone(&self.project_model);
        let index = Arc::clone(&self.index);

        self.runtime.spawn_blocking(move || {
            if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let guard = lock(&project_model);
                let Some(model) = guard.as_ref() else {
                    return;
                };
                let cache_root = external_index::default_cache_root();
                compile_lombok_file(&file_path, &source, &tree, model, &cache_root, &index);
            })) {
                eprintln!("background lombok compile panicked: {panic:?}");
            }
        });
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

        let tool = build_tool::detect(&root_path);

        let project_model = Arc::clone(&self.project_model);
        let index = Arc::clone(&self.index);
        self.runtime.spawn_blocking(move || {
            if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let resolved = match tool {
                    Some(BuildTool::Gradle) => gradle::resolve_project_model(&root_path),
                    Some(BuildTool::Maven) => maven::resolve_project_model(&root_path),
                    // No build tool: still resolve a minimal, JDK-only model so
                    // JDK types (e.g. `java.util.List`) resolve for classpath-free
                    // projects too — see `javac_fallback`.
                    None => Ok(javac_fallback::resolve_project_model(&root_path)),
                };
                match resolved {
                    Ok(model) => {
                        eprintln!(
                            "resolved project model: {} module(s), tool={tool:?}",
                            model.modules.len()
                        );
                        *lock(&project_model) = Some(model.clone());
                        index_external_symbols(&model, &index);
                        compile_lombok_files(&root_path, &model, &index);
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

/// Sweeps the whole workspace once, right after `project_model` resolves,
/// for Lombok-tagged files that are never individually opened but are
/// still referenced elsewhere — runs synchronously since it's already
/// inside a background task, and requires no wait/retry machinery since
/// `project_model` already exists by this point.
fn compile_lombok_files(
    root_path: &Path,
    model: &ProjectModel,
    index: &Arc<Mutex<WorkspaceIndex>>,
) {
    let cache_root = external_index::default_cache_root();
    for file in workspace_scan::find_java_files(root_path) {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let tree = syntax::SyntaxTree::parse(&source);
        compile_lombok_file(&file, &source, tree.tree(), model, &cache_root, index);
    }
}

/// Compiles `file` with Lombok on the processor path if it uses Lombok and
/// its owning module has a resolvable Lombok jar. Silently does nothing
/// otherwise — a non-Lombok file, no owning module, no Lombok dependency,
/// or no locatable JDK are all expected outcomes, not errors.
fn compile_lombok_file(
    file: &Path,
    source: &str,
    tree: &Tree,
    model: &ProjectModel,
    cache_root: &Path,
    index: &Arc<Mutex<WorkspaceIndex>>,
) {
    if !lombok::uses_lombok(tree, source) {
        return;
    }
    let Some(module) = model.module_for_file(file) else {
        return;
    };
    let Some(lombok_jar) = javac_compile::find_lombok_jar(&module.classpath) else {
        return;
    };
    let Some(jdk) = jdk_home::locate() else {
        return;
    };

    let output_dir = javac_compile::output_dir_for_module(cache_root, &module.root);
    match javac_compile::compile(file, module, lombok_jar, &output_dir, &jdk) {
        Ok(()) => {
            external_index::index_javac_output(&output_dir, cache_root, index);
            redirect_lombok_accessors(file, source, tree, &output_dir, cache_root, index);
        }
        Err(err) => eprintln!("lombok compile failed for {file:?}: {err}"),
    }
}

/// After the generic `index_javac_output` above has indexed every compiled
/// class in `output_dir`, corrects the *one* class that was just compiled
/// from `file`: excludes any hand-written member (already correctly
/// indexed from the real source by Tier 2) and redirects any Lombok-generated
/// accessor to its backing field (see `lombok::stub_symbol_overrides`).
/// Silently does nothing if the expected compiled classfile can't be found —
/// an unexpected but non-fatal outcome; the generic indexing above still
/// stands.
fn redirect_lombok_accessors(
    file: &Path,
    source: &str,
    tree: &Tree,
    output_dir: &Path,
    cache_root: &Path,
    index: &Arc<Mutex<WorkspaceIndex>>,
) {
    let Some(file_uri) = workspace_scan::path_to_uri(file) else {
        return;
    };
    let Some(stem) = file.file_stem().and_then(|s| s.to_str()) else {
        return;
    };

    let class_relative: PathBuf = match lombok::source_package(tree, source) {
        Some(package) => Path::new(&package.replace('.', "/")).join(format!("{stem}.class")),
        None => PathBuf::from(format!("{stem}.class")),
    };
    let Ok(bytes) = std::fs::read(output_dir.join(class_relative)) else {
        return;
    };
    let Ok(class) = classfile::parse(&bytes) else {
        return;
    };

    let original_symbols = extract_symbols(&file_uri, source, tree);
    let overrides = lombok::stub_symbol_overrides(&class, &original_symbols);
    external_index::reindex_class_file_with_overrides(&class, cache_root, index, &overrides);
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
    fn did_save_while_initialized_is_accepted_without_error() {
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

        let (exit, outgoing) = server.handle_notification(&notification(
            "textDocument/didSave",
            json!({"textDocument": {"uri": "file:///Main.java"}}),
        ));

        assert_eq!(exit, ExitAction::Continue);
        assert!(outgoing.is_empty());
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
    fn goto_definition_on_a_qualified_call_resolves_only_the_receivers_own_class() {
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());
        initialize(&mut server);

        server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Person.java",
                    "languageId": "java",
                    "version": 1,
                    "text": "class Person { String getName() { return null; } }"
                }
            }),
        ));
        server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Car.java",
                    "languageId": "java",
                    "version": 1,
                    "text": "class Car { String getName() { return null; } }"
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
                    "text": "class Main { void run() { Person person = new Person(); person.getName(); } }"
                }
            }),
        ));

        wait_until(|| server.index.lock().unwrap().lookup("getName").len() >= 2);

        let position_of_get_name =
            "class Main { void run() { Person person = new Person(); person.get".len();
        let response = server.handle_request(&request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///Main.java"},
                "position": {"line": 0, "character": position_of_get_name}
            }),
        ));
        let value = serde_json::to_value(&response).unwrap();

        assert!(value["result"].is_array());
        assert_eq!(value["result"].as_array().unwrap().len(), 1);
        assert_eq!(value["result"][0]["uri"], json!("file:///Person.java"));
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
    fn a_build_tool_less_workspace_still_resolves_jdk_types() {
        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/no_build_tool_sample");
        let root_uri = format!("file://{}", root.display());

        server.handle_request(&request(
            1,
            "initialize",
            json!({"rootUri": root_uri, "capabilities": {}}),
        ));
        server.handle_notification(&notification("initialized", json!({})));

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
            std::time::Duration::from_secs(60),
        );

        let guard = server.project_model.lock().unwrap();
        let model = guard.as_ref().unwrap();
        assert_eq!(model.modules.len(), 1);
        assert!(model.modules[0].classpath.is_empty());
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
    fn lombok_generated_getters_resolve_from_the_workspace_wide_sweep() {
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

        // `Widget.java` (checked into the fixture) is `@Getter @Setter` over a
        // `name` field, never individually opened — this exercises the
        // workspace-wide sweep in `compile_lombok_files`, not the per-edit path.
        let widget_path = root.join("app/src/main/java/com/example/app/Widget.java");
        wait_until_timeout(
            || {
                server
                    .index
                    .lock()
                    .unwrap()
                    .lookup("getName")
                    .iter()
                    .any(|s| {
                        crate::workspace_scan::uri_to_path(&s.uri).as_deref()
                            == Some(widget_path.as_path())
                    })
            },
            std::time::Duration::from_secs(90),
        );

        // The generated getter redirects to the real backing field, not the
        // synthetic stub.
        let symbols = server.index.lock().unwrap().lookup("getName").to_vec();
        let method = symbols
            .iter()
            .find(|s| {
                crate::workspace_scan::uri_to_path(&s.uri).as_deref() == Some(widget_path.as_path())
            })
            .expect("getName should redirect to Widget.java");
        assert_eq!(method.kind, crate::symbol::SymbolKind::Method);
        let source = std::fs::read_to_string(&widget_path).unwrap();
        assert_eq!(
            &source[crate::text_position::position_to_byte_offset(
                &source,
                method.selection_range.start
            )
                ..crate::text_position::position_to_byte_offset(
                    &source,
                    method.selection_range.end
                )],
            "name"
        );

        // A hand-written method in the same Lombok-touched file must not get
        // a second, duplicate entry from the synthetic stub.
        wait_until_timeout(
            || !server.index.lock().unwrap().lookup("describe").is_empty(),
            std::time::Duration::from_secs(30),
        );
        let describe_symbols = server.index.lock().unwrap().lookup("describe").to_vec();
        assert_eq!(
            describe_symbols.len(),
            1,
            "describe() must resolve to exactly the real source, not also a stub duplicate: {describe_symbols:?}"
        );
        assert_eq!(
            crate::workspace_scan::uri_to_path(&describe_symbols[0].uri).as_deref(),
            Some(widget_path.as_path())
        );
    }

    /// Deletes a fixture file added at test time regardless of how the test
    /// exits, so a panic mid-test doesn't leave litter in a checked-in
    /// fixture directory.
    struct CleanupFile(std::path::PathBuf);
    impl Drop for CleanupFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn did_save_on_a_newly_added_lombok_file_makes_its_getter_resolvable() {
        let _guard = crate::test_support::GRADLE_SAMPLE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gradle_sample");
        let new_file = root.join("app/src/main/java/com/example/app/Gadget.java");
        let source = "package com.example.app;\n\nimport lombok.Getter;\n\n@Getter\npublic class Gadget {\n    private String title;\n}\n";
        std::fs::write(&new_file, source).unwrap();
        let _cleanup = CleanupFile(new_file.clone());
        let file_uri = format!("file://{}", new_file.display());

        let runtime = test_runtime();
        let mut server = Server::new(runtime.handle().clone());
        let root_uri = format!("file://{}", root.display());
        server.handle_request(&request(
            1,
            "initialize",
            json!({"rootUri": root_uri, "capabilities": {}}),
        ));
        server.handle_notification(&notification("initialized", json!({})));

        server.handle_notification(&notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": file_uri,
                    "languageId": "java",
                    "version": 1,
                    "text": source
                }
            }),
        ));
        server.handle_notification(&notification(
            "textDocument/didSave",
            json!({"textDocument": {"uri": file_uri}}),
        ));

        wait_until_timeout(
            || {
                server
                    .index
                    .lock()
                    .unwrap()
                    .lookup("getTitle")
                    .iter()
                    .any(|s| s.kind == crate::symbol::SymbolKind::Method)
            },
            std::time::Duration::from_secs(90),
        );
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
