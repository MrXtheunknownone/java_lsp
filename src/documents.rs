use crate::syntax::SyntaxTree;
use lsp_types::{Diagnostic, TextDocumentContentChangeEvent, Uri};
use std::collections::HashMap;

#[derive(Default)]
pub struct DocumentStore {
    documents: HashMap<Uri, SyntaxTree>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&mut self, uri: Uri, text: &str) -> Vec<Diagnostic> {
        let tree = SyntaxTree::parse(text);
        let diagnostics = tree.diagnostics();
        self.documents.insert(uri, tree);
        diagnostics
    }

    pub fn change(
        &mut self,
        uri: &Uri,
        changes: &[TextDocumentContentChangeEvent],
    ) -> Vec<Diagnostic> {
        let Some(tree) = self.documents.get_mut(uri) else {
            return Vec::new();
        };

        for change in changes {
            tree.edit(change);
        }
        tree.diagnostics()
    }

    pub fn close(&mut self, uri: &Uri) {
        self.documents.remove(uri);
    }

    pub fn document(&self, uri: &Uri) -> Option<&SyntaxTree> {
        self.documents.get(uri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range};

    fn uri(value: &str) -> Uri {
        value.parse().expect("test URI is well-formed")
    }

    #[test]
    fn open_with_valid_source_returns_no_diagnostics() {
        let mut store = DocumentStore::new();

        let diagnostics = store.open(uri("file:///Main.java"), "class Main {}");

        assert_eq!(diagnostics, Vec::new());
    }

    #[test]
    fn open_with_broken_source_returns_diagnostics() {
        let mut store = DocumentStore::new();

        let diagnostics = store.open(uri("file:///Main.java"), "class Main {");

        assert!(!diagnostics.is_empty());
    }

    #[test]
    fn change_with_full_text_replacement_updates_diagnostics() {
        let mut store = DocumentStore::new();
        let doc = uri("file:///Main.java");
        store.open(doc.clone(), "class Main {");

        let diagnostics = store.change(
            &doc,
            &[TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "class Main {}".to_string(),
            }],
        );

        assert_eq!(diagnostics, Vec::new());
    }

    #[test]
    fn change_applies_multiple_content_changes_in_order() {
        let mut store = DocumentStore::new();
        let doc = uri("file:///Main.java");
        store.open(doc.clone(), "class Main { void run() { int x = 1 } }");

        let insert_at = Position::new(0, "class Main { void run() { int x = 1".len() as u32);
        let diagnostics = store.change(
            &doc,
            &[
                TextDocumentContentChangeEvent {
                    range: Some(Range::new(insert_at, insert_at)),
                    range_length: None,
                    text: ";".to_string(),
                },
                TextDocumentContentChangeEvent {
                    range: Some(Range::new(insert_at, insert_at)),
                    range_length: None,
                    text: " ".to_string(),
                },
            ],
        );

        assert_eq!(diagnostics, Vec::new());
    }

    #[test]
    fn change_on_unknown_document_returns_no_diagnostics() {
        let mut store = DocumentStore::new();

        let diagnostics = store.change(
            &uri("file:///NeverOpened.java"),
            &[TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "class Main {}".to_string(),
            }],
        );

        assert_eq!(diagnostics, Vec::new());
    }

    #[test]
    fn document_returns_the_current_syntax_tree_for_an_open_uri() {
        let mut store = DocumentStore::new();
        let doc = uri("file:///Main.java");
        store.open(doc.clone(), "class Main {}");

        let tree = store.document(&doc).unwrap();

        assert_eq!(tree.source(), "class Main {}");
    }

    #[test]
    fn document_returns_none_for_an_unopened_uri() {
        let store = DocumentStore::new();

        assert!(store.document(&uri("file:///NeverOpened.java")).is_none());
    }

    #[test]
    fn close_removes_the_document_so_later_changes_are_ignored() {
        let mut store = DocumentStore::new();
        let doc = uri("file:///Main.java");
        store.open(doc.clone(), "class Main {");

        store.close(&doc);
        let diagnostics = store.change(
            &doc,
            &[TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "class Main {}".to_string(),
            }],
        );

        assert_eq!(diagnostics, Vec::new());
    }
}
