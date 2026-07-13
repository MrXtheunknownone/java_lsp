use crate::text_position::byte_offset_to_position;
use lsp_types::{Range, Uri};
use tree_sitter::{Node, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Class,
    Interface,
    Enum,
    Method,
    Constructor,
    Field,
    Import,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: SymbolKind,
    pub uri: Uri,
    pub range: Range,
    pub selection_range: Range,
    /// The simple name of the nearest enclosing class/interface/enum/record
    /// declaring this symbol — `None` for a top-level type or an import,
    /// `Some` for a method/constructor/field. Lets a member-access lookup
    /// (`person.getName()`) narrow same-named candidates to the one actually
    /// declared on the receiver's type, instead of every same-named symbol
    /// in the workspace.
    pub owner: Option<String>,
}

/// A sort key that deterministically orders same-named symbols so that a real
/// declaration is preferred over an `Import` reference, with the remaining ties
/// broken by URI and position — independent of index insertion order, which
/// itself depends on background scan/reindex completion order and filesystem
/// directory-read order.
pub fn relevance_key(symbol: &SymbolInfo) -> (bool, &str, u32, u32) {
    (
        symbol.kind == SymbolKind::Import,
        symbol.uri.as_str(),
        symbol.selection_range.start.line,
        symbol.selection_range.start.character,
    )
}

/// The most relevant symbol among same-named candidates, per [`relevance_key`].
pub fn best_match<'a>(symbols: impl IntoIterator<Item = &'a SymbolInfo>) -> Option<&'a SymbolInfo> {
    symbols
        .into_iter()
        .min_by_key(|symbol| relevance_key(symbol))
}

/// Narrows `candidates` to those declared on `receiver_type`, when given and
/// when at least one candidate actually matches it — otherwise every
/// candidate is returned unfiltered (e.g. an inherited member not owned by
/// the receiver's own class, or a receiver type that couldn't be resolved
/// at all). Shared by goto-definition and hover so a qualified reference
/// (`person.getName()`) narrows to the one symbol actually declared on the
/// receiver's type instead of every same-named symbol in the workspace.
pub fn narrow_to_receiver_type<'a>(
    candidates: &'a [SymbolInfo],
    receiver_type: Option<&str>,
) -> Vec<&'a SymbolInfo> {
    if let Some(receiver_type) = receiver_type {
        let owned: Vec<&SymbolInfo> = candidates
            .iter()
            .filter(|symbol| symbol.owner.as_deref() == Some(receiver_type))
            .collect();
        if !owned.is_empty() {
            return owned;
        }
    }
    candidates.iter().collect()
}

pub fn extract_symbols(uri: &Uri, source: &str, tree: &Tree) -> Vec<SymbolInfo> {
    let mut symbols = Vec::new();
    walk(tree.root_node(), uri, source, &mut symbols, None);
    symbols
}

/// A type declaration's own symbol takes `owner` (its *enclosing* type, if
/// any) — recursion into its body then continues with `owner` set to the
/// type's own name, so its members are attributed to it rather than to
/// whatever type it's nested inside.
fn walk(node: Node, uri: &Uri, source: &str, symbols: &mut Vec<SymbolInfo>, owner: Option<&str>) {
    let child_owner = match node.kind() {
        "class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration" => {
            // A nameless (mid-edit) type declaration's members belong to
            // that type, not the outer one — falling back to `owner` here
            // would mis-attribute them to the wrong class.
            let kind = type_declaration_kind(node.kind());
            push_named(node, kind, uri, source, symbols, owner)
        }
        "method_declaration" => {
            push_named(node, SymbolKind::Method, uri, source, symbols, owner);
            owner.map(String::from)
        }
        "constructor_declaration" | "compact_constructor_declaration" => {
            push_named(node, SymbolKind::Constructor, uri, source, symbols, owner);
            owner.map(String::from)
        }
        "field_declaration" => {
            push_field_declarators(node, uri, source, symbols, owner);
            owner.map(String::from)
        }
        "import_declaration" => {
            push_import(node, uri, source, symbols);
            owner.map(String::from)
        }
        _ => owner.map(String::from),
    };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, uri, source, symbols, child_owner.as_deref());
    }
}

fn type_declaration_kind(node_kind: &str) -> SymbolKind {
    match node_kind {
        "interface_declaration" => SymbolKind::Interface,
        "enum_declaration" => SymbolKind::Enum,
        _ => SymbolKind::Class,
    }
}

/// Pushes `node`'s symbol (if it has a `name` field) and returns its name,
/// so a type declaration's caller can use it as the `owner` for recursion
/// into the type's body.
fn push_named(
    node: Node,
    kind: SymbolKind,
    uri: &Uri,
    source: &str,
    symbols: &mut Vec<SymbolInfo>,
    owner: Option<&str>,
) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    let name = source[name_node.start_byte()..name_node.end_byte()].to_string();
    symbols.push(symbol(name_node, node, kind, uri, source, owner));
    Some(name)
}

fn push_field_declarators(
    node: Node,
    uri: &Uri,
    source: &str,
    symbols: &mut Vec<SymbolInfo>,
    owner: Option<&str>,
) {
    let mut cursor = node.walk();
    for declarator in node.children_by_field_name("declarator", &mut cursor) {
        if let Some(name_node) = declarator.child_by_field_name("name") {
            symbols.push(symbol(
                name_node,
                node,
                SymbolKind::Field,
                uri,
                source,
                owner,
            ));
        }
    }
}

fn push_import(node: Node, uri: &Uri, source: &str, symbols: &mut Vec<SymbolInfo>) {
    let mut cursor = node.walk();
    let mut has_wildcard = false;
    let mut path_node = None;
    for child in node.children(&mut cursor) {
        match child.kind() {
            "asterisk" => has_wildcard = true,
            "scoped_identifier" | "identifier" => path_node = Some(child),
            _ => {}
        }
    }

    if has_wildcard {
        return;
    }

    let Some(path_node) = path_node else {
        return;
    };
    let name_node = path_node.child_by_field_name("name").unwrap_or(path_node);
    symbols.push(symbol(
        name_node,
        node,
        SymbolKind::Import,
        uri,
        source,
        None,
    ));
}

fn symbol(
    name_node: Node,
    whole_node: Node,
    kind: SymbolKind,
    uri: &Uri,
    source: &str,
    owner: Option<&str>,
) -> SymbolInfo {
    SymbolInfo {
        name: source[name_node.start_byte()..name_node.end_byte()].to_string(),
        kind,
        uri: uri.clone(),
        range: node_range(whole_node, source),
        selection_range: node_range(name_node, source),
        owner: owner.map(String::from),
    }
}

fn node_range(node: Node, source: &str) -> Range {
    Range::new(
        byte_offset_to_position(source, node.start_byte()),
        byte_offset_to_position(source, node.end_byte()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn uri() -> Uri {
        "file:///Main.java".parse().unwrap()
    }

    fn names_with_kind(symbols: &[SymbolInfo], kind: SymbolKind) -> Vec<&str> {
        symbols
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.name.as_str())
            .collect()
    }

    #[test]
    fn extract_symbols_finds_a_class_declaration() {
        let source = "class Main {}";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        assert_eq!(names_with_kind(&symbols, SymbolKind::Class), vec!["Main"]);
    }

    #[test]
    fn extract_symbols_finds_an_interface_declaration() {
        let source = "interface Greetable {}";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        assert_eq!(
            names_with_kind(&symbols, SymbolKind::Interface),
            vec!["Greetable"]
        );
    }

    #[test]
    fn extract_symbols_finds_an_enum_declaration() {
        let source = "enum Color { RED, GREEN }";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        assert_eq!(names_with_kind(&symbols, SymbolKind::Enum), vec!["Color"]);
    }

    #[test]
    fn extract_symbols_treats_a_record_declaration_as_a_class() {
        let source = "record Point(int x, int y) {}";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        assert_eq!(names_with_kind(&symbols, SymbolKind::Class), vec!["Point"]);
    }

    #[test]
    fn extract_symbols_finds_a_method_declaration() {
        let source = "class Main { void run() {} }";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        assert_eq!(names_with_kind(&symbols, SymbolKind::Method), vec!["run"]);
    }

    #[test]
    fn extract_symbols_finds_a_constructor_declaration() {
        let source = "class Main { Main() {} }";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        assert_eq!(
            names_with_kind(&symbols, SymbolKind::Constructor),
            vec!["Main"]
        );
    }

    #[test]
    fn extract_symbols_finds_every_declarator_in_a_multi_declarator_field() {
        let source = "class Main { int a, b; }";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        assert_eq!(names_with_kind(&symbols, SymbolKind::Field), vec!["a", "b"]);
    }

    #[test]
    fn extract_symbols_finds_a_regular_import() {
        let source = "import java.util.List;\nclass Main {}";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        assert_eq!(names_with_kind(&symbols, SymbolKind::Import), vec!["List"]);
    }

    #[test]
    fn extract_symbols_skips_a_wildcard_import() {
        let source = "import java.util.*;\nclass Main {}";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        assert!(names_with_kind(&symbols, SymbolKind::Import).is_empty());
    }

    #[test]
    fn extract_symbols_sets_no_owner_for_a_top_level_class() {
        let source = "class Main {}";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        let class_symbol = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Class)
            .unwrap();
        assert_eq!(class_symbol.owner, None);
    }

    #[test]
    fn extract_symbols_sets_no_owner_for_an_import() {
        let source = "import java.util.List;\nclass Main {}";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        let import_symbol = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Import)
            .unwrap();
        assert_eq!(import_symbol.owner, None);
    }

    #[test]
    fn extract_symbols_sets_a_methods_owner_to_its_enclosing_class() {
        let source = "class Person { String getName() { return null; } }";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        let method = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Method)
            .unwrap();
        assert_eq!(method.owner.as_deref(), Some("Person"));
    }

    #[test]
    fn extract_symbols_sets_a_fields_owner_to_its_enclosing_class() {
        let source = "class Person { String name; }";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        let field = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Field)
            .unwrap();
        assert_eq!(field.owner.as_deref(), Some("Person"));
    }

    #[test]
    fn extract_symbols_sets_a_constructors_owner_to_its_enclosing_class() {
        let source = "class Person { Person() {} }";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        let constructor = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Constructor)
            .unwrap();
        assert_eq!(constructor.owner.as_deref(), Some("Person"));
    }

    #[test]
    fn extract_symbols_distinguishes_owners_for_the_same_method_name_in_different_classes() {
        let source = "class Person { String getName() { return null; } } class Car { String getName() { return null; } }";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        let mut owners: Vec<Option<&str>> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method && s.name == "getName")
            .map(|s| s.owner.as_deref())
            .collect();
        owners.sort();
        assert_eq!(owners, vec![Some("Car"), Some("Person")]);
    }

    #[test]
    fn extract_symbols_attributes_a_nested_classs_members_to_the_nested_class_not_the_outer_one() {
        let source = "class Outer { class Inner { void run() {} } }";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);

        let inner_class = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Class && s.name == "Inner")
            .unwrap();
        assert_eq!(inner_class.owner.as_deref(), Some("Outer"));

        let run_method = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Method && s.name == "run")
            .unwrap();
        assert_eq!(run_method.owner.as_deref(), Some("Inner"));
    }

    #[test]
    fn selection_range_is_narrower_than_the_full_declaration_range() {
        let source = "class Main {}";
        let tree = parse(source);

        let symbols = extract_symbols(&uri(), source, &tree);
        let class_symbol = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Class)
            .unwrap();

        assert_eq!(class_symbol.selection_range.start.character, 6);
        assert_eq!(class_symbol.selection_range.end.character, 10);
        assert_eq!(class_symbol.range.start.character, 0);
    }

    fn make_symbol(name: &str, kind: SymbolKind, uri_str: &str) -> SymbolInfo {
        let range = lsp_types::Range::new(
            lsp_types::Position::new(0, 0),
            lsp_types::Position::new(0, 1),
        );
        SymbolInfo {
            name: name.to_string(),
            kind,
            uri: uri_str.parse().unwrap(),
            range,
            selection_range: range,
            owner: None,
        }
    }

    #[test]
    fn best_match_prefers_a_declaration_over_an_import_regardless_of_order() {
        let import = make_symbol("Greeter", SymbolKind::Import, "file:///Main.java");
        let class = make_symbol("Greeter", SymbolKind::Class, "file:///Greeter.java");

        assert_eq!(best_match(&[import.clone(), class.clone()]), Some(&class));
        assert_eq!(
            best_match(&[class, import]),
            Some(&make_symbol(
                "Greeter",
                SymbolKind::Class,
                "file:///Greeter.java"
            ))
        );
    }

    #[test]
    fn best_match_is_deterministic_between_two_real_declarations_regardless_of_order() {
        let a = make_symbol("Builder", SymbolKind::Class, "file:///a/Builder.java");
        let b = make_symbol("Builder", SymbolKind::Class, "file:///b/Builder.java");

        let first_order = best_match(&[a.clone(), b.clone()]).cloned();
        let second_order = best_match(&[b, a]).cloned();

        assert_eq!(first_order, second_order);
    }

    fn make_symbol_with_owner(name: &str, uri_str: &str, owner: &str) -> SymbolInfo {
        let mut symbol = make_symbol(name, SymbolKind::Method, uri_str);
        symbol.owner = Some(owner.to_string());
        symbol
    }

    #[test]
    fn narrow_to_receiver_type_keeps_only_the_candidate_owned_by_the_receiver() {
        let person_method = make_symbol_with_owner("getName", "file:///Person.java", "Person");
        let car_method = make_symbol_with_owner("getName", "file:///Car.java", "Car");
        let candidates = vec![person_method.clone(), car_method];

        let narrowed = narrow_to_receiver_type(&candidates, Some("Person"));

        assert_eq!(narrowed, vec![&person_method]);
    }

    #[test]
    fn narrow_to_receiver_type_falls_back_to_every_candidate_when_none_match() {
        let person_method = make_symbol_with_owner("getName", "file:///Person.java", "Person");
        let car_method = make_symbol_with_owner("getName", "file:///Car.java", "Car");
        let candidates = vec![person_method, car_method];

        let narrowed = narrow_to_receiver_type(&candidates, Some("Dog"));

        assert_eq!(narrowed.len(), 2);
    }

    #[test]
    fn narrow_to_receiver_type_returns_every_candidate_when_no_receiver_type_is_given() {
        let person_method = make_symbol_with_owner("getName", "file:///Person.java", "Person");
        let car_method = make_symbol_with_owner("getName", "file:///Car.java", "Car");
        let candidates = vec![person_method, car_method];

        let narrowed = narrow_to_receiver_type(&candidates, None);

        assert_eq!(narrowed.len(), 2);
    }
}
