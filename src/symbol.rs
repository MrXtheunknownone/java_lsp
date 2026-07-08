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
pub fn best_match(symbols: &[SymbolInfo]) -> Option<&SymbolInfo> {
    symbols.iter().min_by_key(|symbol| relevance_key(symbol))
}

pub fn extract_symbols(uri: &Uri, source: &str, tree: &Tree) -> Vec<SymbolInfo> {
    let mut symbols = Vec::new();
    walk(tree.root_node(), uri, source, &mut symbols);
    symbols
}

fn walk(node: Node, uri: &Uri, source: &str, symbols: &mut Vec<SymbolInfo>) {
    match node.kind() {
        "class_declaration" => push_named(node, SymbolKind::Class, uri, source, symbols),
        "interface_declaration" => push_named(node, SymbolKind::Interface, uri, source, symbols),
        "enum_declaration" => push_named(node, SymbolKind::Enum, uri, source, symbols),
        "record_declaration" => push_named(node, SymbolKind::Class, uri, source, symbols),
        "method_declaration" => push_named(node, SymbolKind::Method, uri, source, symbols),
        "constructor_declaration" | "compact_constructor_declaration" => {
            push_named(node, SymbolKind::Constructor, uri, source, symbols)
        }
        "field_declaration" => push_field_declarators(node, uri, source, symbols),
        "import_declaration" => push_import(node, uri, source, symbols),
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, uri, source, symbols);
    }
}

fn push_named(
    node: Node,
    kind: SymbolKind,
    uri: &Uri,
    source: &str,
    symbols: &mut Vec<SymbolInfo>,
) {
    if let Some(name_node) = node.child_by_field_name("name") {
        symbols.push(symbol(name_node, node, kind, uri, source));
    }
}

fn push_field_declarators(node: Node, uri: &Uri, source: &str, symbols: &mut Vec<SymbolInfo>) {
    let mut cursor = node.walk();
    for declarator in node.children_by_field_name("declarator", &mut cursor) {
        if let Some(name_node) = declarator.child_by_field_name("name") {
            symbols.push(symbol(name_node, node, SymbolKind::Field, uri, source));
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
    symbols.push(symbol(name_node, node, SymbolKind::Import, uri, source));
}

fn symbol(
    name_node: Node,
    whole_node: Node,
    kind: SymbolKind,
    uri: &Uri,
    source: &str,
) -> SymbolInfo {
    SymbolInfo {
        name: source[name_node.start_byte()..name_node.end_byte()].to_string(),
        kind,
        uri: uri.clone(),
        range: node_range(whole_node, source),
        selection_range: node_range(name_node, source),
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
}
