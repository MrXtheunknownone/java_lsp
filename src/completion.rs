use crate::documents::DocumentStore;
use crate::symbol::SymbolKind;
use crate::workspace_index::WorkspaceIndex;
use lsp_types::{CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse};

pub fn completion(
    index: &WorkspaceIndex,
    documents: &DocumentStore,
    params: &CompletionParams,
) -> Option<CompletionResponse> {
    let position_params = &params.text_document_position;
    let document = documents.document(&position_params.text_document.uri)?;
    let prefix = document.identifier_prefix_before(position_params.position);

    if prefix.is_empty() {
        return None;
    }

    let items: Vec<CompletionItem> = index
        .completions(&prefix)
        .into_iter()
        .map(|symbol| CompletionItem {
            label: symbol.name.clone(),
            kind: Some(completion_item_kind(symbol.kind)),
            ..Default::default()
        })
        .collect();

    Some(CompletionResponse::Array(items))
}

fn completion_item_kind(kind: SymbolKind) -> CompletionItemKind {
    match kind {
        SymbolKind::Class => CompletionItemKind::CLASS,
        SymbolKind::Interface => CompletionItemKind::INTERFACE,
        SymbolKind::Enum => CompletionItemKind::ENUM,
        SymbolKind::Method => CompletionItemKind::METHOD,
        SymbolKind::Constructor => CompletionItemKind::CONSTRUCTOR,
        SymbolKind::Field => CompletionItemKind::FIELD,
        SymbolKind::Import => CompletionItemKind::MODULE,
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

    fn params(uri: Uri, position: Position) -> CompletionParams {
        CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        }
    }

    fn symbol(name: &str, kind: SymbolKind, uri: Uri) -> SymbolInfo {
        let range = Range::new(Position::new(0, 0), Position::new(0, 1));
        SymbolInfo {
            name: name.to_string(),
            kind,
            uri,
            range,
            selection_range: range,
        }
    }

    #[test]
    fn completion_returns_matching_workspace_symbols_by_prefix() {
        let main = uri("file:///Main.java");
        let greeter = uri("file:///Greeter.java");

        let mut documents = DocumentStore::new();
        documents.open(main.clone(), "class Main { Gree }");

        let mut index = WorkspaceIndex::new();
        index.update_file(
            greeter.clone(),
            1,
            1,
            vec![symbol("Greeter", SymbolKind::Class, greeter)],
        );

        let position = Position::new(0, "class Main { Gree".len() as u32);
        let response = completion(&index, &documents, &params(main, position)).unwrap();

        match response {
            CompletionResponse::Array(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].label, "Greeter");
                assert_eq!(items[0].kind, Some(CompletionItemKind::CLASS));
            }
            other => panic!("expected an Array response, got {other:?}"),
        }
    }

    #[test]
    fn completion_returns_none_when_there_is_no_prefix_being_typed() {
        let main = uri("file:///Main.java");
        let mut documents = DocumentStore::new();
        documents.open(main.clone(), "class Main {}");
        let index = WorkspaceIndex::new();

        let response = completion(&index, &documents, &params(main, Position::new(0, 6)));

        assert!(response.is_none());
    }
}
