use crate::documents::DocumentStore;
use crate::symbol::{self, SymbolKind};
use crate::workspace_index::WorkspaceIndex;
use lsp_types::{Hover, HoverContents, HoverParams, MarkedString};

pub fn hover(
    index: &WorkspaceIndex,
    documents: &DocumentStore,
    params: &HoverParams,
) -> Option<Hover> {
    let position_params = &params.text_document_position_params;
    let document = documents.document(&position_params.text_document.uri)?;
    let (name, range) = document.identifier_at(position_params.position)?;
    let symbol = symbol::best_match(index.lookup(&name))?;

    Some(Hover {
        contents: HoverContents::Scalar(MarkedString::String(format!(
            "{} {}",
            kind_label(symbol.kind),
            symbol.name
        ))),
        range: Some(range),
    })
}

fn kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Class => "class",
        SymbolKind::Interface => "interface",
        SymbolKind::Enum => "enum",
        SymbolKind::Method => "method",
        SymbolKind::Constructor => "constructor",
        SymbolKind::Field => "field",
        SymbolKind::Import => "import",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol::SymbolInfo;
    use lsp_types::{Position, Range, TextDocumentIdentifier, TextDocumentPositionParams, Uri};

    fn uri(value: &str) -> Uri {
        value.parse().unwrap()
    }

    fn params(uri: Uri, position: Position) -> HoverParams {
        HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: Default::default(),
        }
    }

    #[test]
    fn hover_returns_kind_and_name_for_a_resolved_symbol() {
        let main = uri("file:///Main.java");
        let greeter = uri("file:///Greeter.java");

        let mut documents = DocumentStore::new();
        documents.open(main.clone(), "class Main { Greeter g; }");

        let mut index = WorkspaceIndex::new();
        let range = Range::new(Position::new(0, 6), Position::new(0, 13));
        index.update_file(
            greeter.clone(),
            1,
            1,
            vec![SymbolInfo {
                name: "Greeter".to_string(),
                kind: SymbolKind::Class,
                uri: greeter,
                range,
                selection_range: range,
            }],
        );

        let position = Position::new(0, "class Main { Gree".len() as u32);
        let result = hover(&index, &documents, &params(main, position)).unwrap();

        match result.contents {
            HoverContents::Scalar(MarkedString::String(text)) => {
                assert_eq!(text, "class Greeter");
            }
            other => panic!("expected a scalar string, got {other:?}"),
        }
    }

    #[test]
    fn hover_prefers_a_declaration_over_an_import_regardless_of_index_insertion_order() {
        let main = uri("file:///Main.java");
        let greeter = uri("file:///Greeter.java");
        let class_range = Range::new(Position::new(0, 6), Position::new(0, 13));
        let class_symbol = SymbolInfo {
            name: "Greeter".to_string(),
            kind: SymbolKind::Class,
            uri: greeter,
            range: class_range,
            selection_range: class_range,
        };
        let import_range = Range::new(Position::new(0, 0), Position::new(0, 7));
        let import_symbol = SymbolInfo {
            name: "Greeter".to_string(),
            kind: SymbolKind::Import,
            uri: main.clone(),
            range: import_range,
            selection_range: import_range,
        };

        let mut documents = DocumentStore::new();
        documents.open(main.clone(), "class Main { Greeter g; }");
        let position = Position::new(0, "class Main { Gree".len() as u32);

        // Import indexed before the class it refers to.
        let mut import_first = WorkspaceIndex::new();
        import_first.update_file(main.clone(), 1, 1, vec![import_symbol.clone()]);
        import_first.update_file(import_symbol.uri.clone(), 1, 1, vec![class_symbol.clone()]);
        let result = hover(&import_first, &documents, &params(main.clone(), position)).unwrap();
        assert_eq!(
            result.contents,
            HoverContents::Scalar(MarkedString::String("class Greeter".to_string()))
        );

        // Class indexed before the import that refers to it.
        let mut class_first = WorkspaceIndex::new();
        class_first.update_file(class_symbol.uri.clone(), 1, 1, vec![class_symbol]);
        class_first.update_file(main.clone(), 1, 1, vec![import_symbol]);
        let result = hover(&class_first, &documents, &params(main, position)).unwrap();
        assert_eq!(
            result.contents,
            HoverContents::Scalar(MarkedString::String("class Greeter".to_string()))
        );
    }

    #[test]
    fn hover_returns_none_when_not_on_an_identifier() {
        let main = uri("file:///Main.java");
        let mut documents = DocumentStore::new();
        documents.open(main.clone(), "class Main {}");
        let index = WorkspaceIndex::new();

        let result = hover(&index, &documents, &params(main, Position::new(0, 0)));

        assert!(result.is_none());
    }

    #[test]
    fn hover_returns_none_when_no_symbol_matches() {
        let main = uri("file:///Main.java");
        let mut documents = DocumentStore::new();
        documents.open(main.clone(), "class Main { Unknown u; }");
        let index = WorkspaceIndex::new();

        let position = Position::new(0, "class Main { Unkn".len() as u32);
        let result = hover(&index, &documents, &params(main, position));

        assert!(result.is_none());
    }
}
