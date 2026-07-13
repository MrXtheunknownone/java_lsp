use crate::documents::DocumentStore;
use crate::member_reference::member_reference_at;
use crate::symbol;
use crate::workspace_index::WorkspaceIndex;
use lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, Location};

pub fn goto_definition(
    index: &WorkspaceIndex,
    documents: &DocumentStore,
    params: &GotoDefinitionParams,
) -> Option<GotoDefinitionResponse> {
    let position_params = &params.text_document_position_params;
    let document = documents.document(&position_params.text_document.uri)?;
    let (name, _) = document.identifier_at(position_params.position)?;

    let candidates = index.lookup(&name);
    let receiver_type =
        member_reference_at(document.tree(), document.source(), position_params.position)
            .map(|reference| reference.receiver_type);
    let selected = symbol::narrow_to_receiver_type(candidates, receiver_type.as_deref());

    let locations: Vec<Location> = selected
        .iter()
        .map(|symbol| Location {
            uri: symbol.uri.clone(),
            range: symbol.selection_range,
        })
        .collect();

    if locations.is_empty() {
        None
    } else {
        Some(GotoDefinitionResponse::Array(locations))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol::{SymbolInfo, SymbolKind};
    use lsp_types::{Position, Range, TextDocumentIdentifier, TextDocumentPositionParams, Uri};

    fn uri(value: &str) -> Uri {
        value.parse().unwrap()
    }

    fn params(uri: Uri, position: Position) -> GotoDefinitionParams {
        GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    #[test]
    fn resolves_a_reference_to_its_workspace_declaration() {
        let main = uri("file:///Main.java");
        let greeter = uri("file:///Greeter.java");

        let mut documents = DocumentStore::new();
        documents.open(main.clone(), "class Main { Greeter g; }");

        let mut index = WorkspaceIndex::new();
        let declaration_range = Range::new(Position::new(0, 6), Position::new(0, 13));
        index.update_file(
            greeter.clone(),
            1,
            1,
            vec![SymbolInfo {
                name: "Greeter".to_string(),
                kind: SymbolKind::Class,
                uri: greeter.clone(),
                range: declaration_range,
                selection_range: declaration_range,
                owner: None,
            }],
        );

        let position = Position::new(0, "class Main { Gree".len() as u32);
        let response = goto_definition(&index, &documents, &params(main, position)).unwrap();

        match response {
            GotoDefinitionResponse::Array(locations) => {
                assert_eq!(locations.len(), 1);
                assert_eq!(locations[0].uri, greeter);
                assert_eq!(locations[0].range, declaration_range);
            }
            other => panic!("expected an Array response, got {other:?}"),
        }
    }

    #[test]
    fn returns_none_when_the_cursor_is_not_on_an_identifier() {
        let main = uri("file:///Main.java");
        let mut documents = DocumentStore::new();
        documents.open(main.clone(), "class Main {}");
        let index = WorkspaceIndex::new();

        let response = goto_definition(&index, &documents, &params(main, Position::new(0, 0)));

        assert!(response.is_none());
    }

    #[test]
    fn returns_none_when_no_declaration_matches_the_name() {
        let main = uri("file:///Main.java");
        let mut documents = DocumentStore::new();
        documents.open(main.clone(), "class Main { Unknown u; }");
        let index = WorkspaceIndex::new();

        let position = Position::new(0, "class Main { Unkn".len() as u32);
        let response = goto_definition(&index, &documents, &params(main, position));

        assert!(response.is_none());
    }

    fn method_symbol(uri: Uri, range: Range, owner: &str) -> SymbolInfo {
        SymbolInfo {
            name: "getName".to_string(),
            kind: SymbolKind::Method,
            uri,
            range,
            selection_range: range,
            owner: Some(owner.to_string()),
        }
    }

    #[test]
    fn a_qualified_method_call_resolves_to_only_the_receivers_own_declaring_class() {
        let main = uri("file:///Main.java");
        let person_uri = uri("file:///Person.java");
        let car_uri = uri("file:///Car.java");

        let mut documents = DocumentStore::new();
        documents.open(
            main.clone(),
            "class Main { void run() { Person person = new Person(); person.getName(); } }",
        );

        let mut index = WorkspaceIndex::new();
        let person_range = Range::new(Position::new(0, 0), Position::new(0, 7));
        let car_range = Range::new(Position::new(0, 0), Position::new(0, 7));
        index.update_file(
            person_uri.clone(),
            1,
            1,
            vec![method_symbol(person_uri.clone(), person_range, "Person")],
        );
        index.update_file(
            car_uri.clone(),
            1,
            1,
            vec![method_symbol(car_uri, car_range, "Car")],
        );

        let position = Position::new(
            0,
            "class Main { void run() { Person person = new Person(); person.get".len() as u32,
        );
        let response = goto_definition(&index, &documents, &params(main, position)).unwrap();

        match response {
            GotoDefinitionResponse::Array(locations) => {
                assert_eq!(locations.len(), 1);
                assert_eq!(locations[0].uri, person_uri);
                assert_eq!(locations[0].range, person_range);
            }
            other => panic!("expected an Array response, got {other:?}"),
        }
    }

    #[test]
    fn falls_back_to_every_same_named_candidate_when_the_receiver_type_is_unresolvable() {
        let main = uri("file:///Main.java");
        let person_uri = uri("file:///Person.java");
        let car_uri = uri("file:///Car.java");

        let mut documents = DocumentStore::new();
        documents.open(
            main.clone(),
            "class Main { void run() { getPerson().getName(); } }",
        );

        let mut index = WorkspaceIndex::new();
        let person_range = Range::new(Position::new(0, 0), Position::new(0, 7));
        let car_range = Range::new(Position::new(0, 0), Position::new(0, 7));
        index.update_file(
            person_uri.clone(),
            1,
            1,
            vec![method_symbol(person_uri, person_range, "Person")],
        );
        index.update_file(
            car_uri.clone(),
            1,
            1,
            vec![method_symbol(car_uri, car_range, "Car")],
        );

        let position = Position::new(0, "class Main { void run() { getPerson().get".len() as u32);
        let response = goto_definition(&index, &documents, &params(main, position)).unwrap();

        match response {
            GotoDefinitionResponse::Array(locations) => assert_eq!(locations.len(), 2),
            other => panic!("expected an Array response, got {other:?}"),
        }
    }
}
