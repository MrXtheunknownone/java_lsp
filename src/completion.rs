use crate::documents::DocumentStore;
use crate::member_reference;
use crate::symbol::{SymbolInfo, SymbolKind};
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
    let receiver = document.qualified_completion_receiver(position_params.position);

    // A bare/global completion (no `.`-qualified receiver) needs at least
    // one typed character to search on — an empty prefix would otherwise
    // mean "every symbol in the index". A qualified receiver has no such
    // restriction: `person.` with nothing typed yet is exactly the moment
    // the `.` trigger character (see handshake.rs) fires, and it must still
    // offer every member of the receiver's type.
    if receiver.is_none() && prefix.is_empty() {
        return None;
    }

    // Unlike goto-definition/hover, a `.`-qualified receiver whose type
    // can't be resolved (a chained call, an unknown variable) narrows to
    // nothing rather than falling back to every global match — offering
    // unrelated symbols from other classes would be a worse completion
    // experience than offering none.
    let selected: Vec<&SymbolInfo> = match receiver {
        Some((receiver_name, receiver_position)) => member_reference::resolve_declared_type_at(
            document.tree(),
            document.source(),
            receiver_position,
            &receiver_name,
        )
        .map(|receiver_type| index.completions_by_owner(&prefix, &receiver_type))
        .unwrap_or_default(),
        None => index.completions(&prefix),
    };

    let items: Vec<CompletionItem> = selected
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
            owner: None,
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

    fn owned_field_symbol(name: &str, uri: Uri, owner: &str) -> SymbolInfo {
        let range = Range::new(Position::new(0, 0), Position::new(0, 1));
        SymbolInfo {
            name: name.to_string(),
            kind: SymbolKind::Field,
            uri,
            range,
            selection_range: range,
            owner: Some(owner.to_string()),
        }
    }

    #[test]
    fn completion_after_a_qualified_receiver_only_offers_that_types_members() {
        let main = uri("file:///Main.java");
        let person_uri = uri("file:///Person.java");
        let car_uri = uri("file:///Car.java");

        let mut documents = DocumentStore::new();
        documents.open(
            main.clone(),
            "class Main { void run() { Person person = new Person(); person.na } }",
        );

        let mut index = WorkspaceIndex::new();
        index.update_file(
            person_uri.clone(),
            1,
            1,
            vec![owned_field_symbol("name", person_uri.clone(), "Person")],
        );
        index.update_file(
            car_uri.clone(),
            1,
            1,
            vec![owned_field_symbol("name", car_uri, "Car")],
        );

        let position = Position::new(
            0,
            "class Main { void run() { Person person = new Person(); person.na".len() as u32,
        );
        let response = completion(&index, &documents, &params(main, position)).unwrap();

        match response {
            CompletionResponse::Array(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].label, "name");
            }
            other => panic!("expected an Array response, got {other:?}"),
        }
    }

    #[test]
    fn completion_returns_no_items_when_the_resolved_type_has_no_matching_member() {
        let main = uri("file:///Main.java");
        let person_uri = uri("file:///Person.java");
        let car_uri = uri("file:///Car.java");

        let mut documents = DocumentStore::new();
        documents.open(
            main.clone(),
            "class Main { void run() { Person person = new Person(); person.na } }",
        );

        let mut index = WorkspaceIndex::new();
        index.update_file(person_uri, 1, 1, vec![]);
        index.update_file(
            car_uri.clone(),
            1,
            1,
            vec![owned_field_symbol("name", car_uri, "Car")],
        );

        let position = Position::new(
            0,
            "class Main { void run() { Person person = new Person(); person.na".len() as u32,
        );
        let response = completion(&index, &documents, &params(main, position)).unwrap();

        match response {
            CompletionResponse::Array(items) => assert!(items.is_empty()),
            other => panic!("expected an Array response, got {other:?}"),
        }
    }

    #[test]
    fn completion_returns_no_items_when_a_qualified_receivers_type_is_unresolved() {
        let main = uri("file:///Main.java");
        let car_uri = uri("file:///Car.java");

        let mut documents = DocumentStore::new();
        // `unknownVar` is never declared in scope, so its type can't be
        // resolved — a real dot-qualified receiver, just an unresolvable one.
        documents.open(main.clone(), "class Main { void run() { unknownVar.na } }");

        let mut index = WorkspaceIndex::new();
        index.update_file(
            car_uri.clone(),
            1,
            1,
            vec![owned_field_symbol("name", car_uri, "Car")],
        );

        let position = Position::new(0, "class Main { void run() { unknownVar.na".len() as u32);
        let response = completion(&index, &documents, &params(main, position)).unwrap();

        match response {
            // Offering Car's unrelated "name" would be worse than offering
            // nothing, and would reintroduce the cross-class noise this fix
            // exists to eliminate.
            CompletionResponse::Array(items) => assert!(items.is_empty()),
            other => panic!("expected an Array response, got {other:?}"),
        }
    }

    #[test]
    fn completion_offers_every_member_right_after_the_dot_trigger_with_no_prefix_yet() {
        let main = uri("file:///Main.java");
        let person_uri = uri("file:///Person.java");

        let mut documents = DocumentStore::new();
        documents.open(
            main.clone(),
            "class Main { void run() { Person person = new Person(); person. } }",
        );

        let mut index = WorkspaceIndex::new();
        index.update_file(
            person_uri.clone(),
            1,
            1,
            vec![owned_field_symbol("name", person_uri, "Person")],
        );

        let position = Position::new(
            0,
            "class Main { void run() { Person person = new Person(); person.".len() as u32,
        );
        let response = completion(&index, &documents, &params(main, position)).unwrap();

        match response {
            CompletionResponse::Array(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].label, "name");
            }
            other => panic!("expected an Array response, got {other:?}"),
        }
    }
}
