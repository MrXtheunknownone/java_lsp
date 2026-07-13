//! Resolves the receiver's declared type for a member-access expression
//! (`person.getName()`, `person.name`) using only lexical, syntax-tree-local
//! information (no cross-file type checking) — enough to narrow goto
//! definition/hover/completion candidates to the one symbol actually
//! declared on the receiver's type, instead of every same-named symbol in
//! the workspace. Deliberately conservative: anything it can't resolve with
//! a simple scope walk (a chained call, a cast, an unknown receiver) returns
//! `None`, and the caller falls back to today's name-only behavior.

use crate::text_position::position_to_byte_offset;
use lsp_types::Position;
use tree_sitter::{Node, Tree};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberReference {
    pub name: String,
    pub receiver_type: String,
}

/// If `position` is on the member-name identifier of a `method_invocation`
/// or `field_access`, resolves its receiver's declared type. Returns `None`
/// when the cursor isn't on such a member name, or the receiver's type
/// can't be determined from local scope alone.
pub fn member_reference_at(
    tree: &Tree,
    source: &str,
    position: Position,
) -> Option<MemberReference> {
    let byte_offset = position_to_byte_offset(source, position);
    let node = tree
        .root_node()
        .descendant_for_byte_range(byte_offset, byte_offset)?;

    if node.kind() != "identifier" {
        return None;
    }
    let parent = node.parent()?;

    let object = match parent.kind() {
        "method_invocation" if is_field(parent, "name", node) => {
            parent.child_by_field_name("object")
        }
        "field_access" if is_field(parent, "field", node) => {
            Some(parent.child_by_field_name("object")?)
        }
        _ => return None,
    };

    let name = node_text(node, source).to_string();
    let receiver_type = match object {
        None => enclosing_type_name(node, source)?,
        Some(object) if object.kind() == "this" => enclosing_type_name(node, source)?,
        Some(object) if object.kind() == "identifier" => {
            resolve_identifier_type(object, source, node_text(object, source))?
        }
        Some(_) => return None,
    };

    Some(MemberReference {
        name,
        receiver_type,
    })
}

/// Resolves `identifier_name`'s declared type at `position` — used for a
/// completion receiver (`person.` plus a partial member name), where the
/// receiver already parses as a normal expression (a `field_access` node's
/// `object`, even though the member itself is only partially typed) but,
/// unlike goto-definition/hover, there isn't already a specific reference
/// node in hand to pass to [`resolve_identifier_type`] directly.
pub fn resolve_declared_type_at(
    tree: &Tree,
    source: &str,
    position: Position,
    identifier_name: &str,
) -> Option<String> {
    let byte_offset = position_to_byte_offset(source, position);
    let node = tree
        .root_node()
        .descendant_for_byte_range(byte_offset, byte_offset)?;
    if identifier_name == "this" {
        return enclosing_type_name(node, source);
    }
    resolve_identifier_type(node, source, identifier_name)
}

fn is_field(node: Node, field_name: &str, candidate: Node) -> bool {
    node.child_by_field_name(field_name) == Some(candidate)
}

fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

/// The simple name of the nearest enclosing class/interface/enum/record —
/// used for `this.member()`/an implicit unqualified call's receiver.
fn enclosing_type_name(node: Node, source: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(candidate) = current {
        if is_type_declaration(candidate.kind()) {
            let name_node = candidate.child_by_field_name("name")?;
            return Some(node_text(name_node, source).to_string());
        }
        current = candidate.parent();
    }
    None
}

fn is_type_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration" | "interface_declaration" | "enum_declaration" | "record_declaration"
    )
}

fn is_method_like(kind: &str) -> bool {
    matches!(
        kind,
        "method_declaration" | "constructor_declaration" | "compact_constructor_declaration"
    )
}

/// Resolves `identifier_name`'s declared type by ascending from
/// `reference_node` through its enclosing scopes — the block(s) containing
/// it, the enclosing method/constructor's parameters, then the enclosing
/// class's own fields — checking only each level's own direct declarations,
/// never descending into an unrelated sibling block, nested lambda, or
/// local/anonymous class along the way. This is what keeps a same-named
/// variable declared in a different block (or inside a nested scope that
/// merely happens to sit inside the same method) from shadowing the
/// reference's real, innermost declaration. Doesn't model declare-before-use
/// ordering within a single block — a single unambiguous match is all
/// goto-definition/hover/completion need to narrow same-named candidates.
fn resolve_identifier_type(
    reference_node: Node,
    source: &str,
    identifier_name: &str,
) -> Option<String> {
    let mut current = reference_node.parent();
    while let Some(candidate) = current {
        if let Some(type_text) = declared_type_in_scope(candidate, source, identifier_name) {
            return Some(simple_type_name(type_text));
        }
        current = candidate.parent();
    }
    None
}

/// Checks whether `node` — one ancestor of the reference site — itself
/// scopes a declaration of `identifier_name`: a method/constructor's own
/// parameters, a type declaration's own fields, or a block's own direct
/// local variable declarations. Only ever looks at `node`'s immediate
/// children, never recurses into a nested block/type/lambda, so ascending
/// caller-to-caller through enclosing scopes only ever sees declarations
/// actually in lexical scope at the reference site.
fn declared_type_in_scope<'a>(
    node: Node,
    source: &'a str,
    identifier_name: &str,
) -> Option<&'a str> {
    if is_method_like(node.kind())
        && let Some(parameters) = node.child_by_field_name("parameters")
    {
        let mut cursor = parameters.walk();
        for parameter in parameters.children(&mut cursor) {
            if let Some(type_text) = declared_parameter_type(parameter, source, identifier_name) {
                return Some(type_text);
            }
        }
    }

    if is_type_declaration(node.kind()) {
        return find_field_type_in(node, source, identifier_name);
    }

    let mut cursor = node.walk();
    node.children(&mut cursor).find_map(|child| {
        (child.kind() == "local_variable_declaration")
            .then(|| declared_local_variable_type(child, source, identifier_name))
            .flatten()
    })
}

fn declared_parameter_type<'a>(
    node: Node,
    source: &'a str,
    identifier_name: &str,
) -> Option<&'a str> {
    if node.kind() != "formal_parameter" {
        return None;
    }
    let name_node = node.child_by_field_name("name")?;
    if node_text(name_node, source) != identifier_name {
        return None;
    }
    let type_node = node.child_by_field_name("type")?;
    Some(node_text(type_node, source))
}

fn declared_local_variable_type<'a>(
    node: Node,
    source: &'a str,
    identifier_name: &str,
) -> Option<&'a str> {
    let mut cursor = node.walk();
    let matches_name = node
        .children_by_field_name("declarator", &mut cursor)
        .any(|declarator| {
            declarator
                .child_by_field_name("name")
                .is_some_and(|name_node| node_text(name_node, source) == identifier_name)
        });
    if !matches_name {
        return None;
    }
    let type_node = node.child_by_field_name("type")?;
    Some(node_text(type_node, source))
}

fn find_field_type_in<'a>(
    type_declaration: Node,
    source: &'a str,
    identifier_name: &str,
) -> Option<&'a str> {
    let body = type_declaration.child_by_field_name("body")?;
    let mut cursor = body.walk();
    body.children(&mut cursor)
        .filter(|child| child.kind() == "field_declaration")
        .find_map(|field| {
            let mut declarator_cursor = field.walk();
            let matches_name = field
                .children_by_field_name("declarator", &mut declarator_cursor)
                .any(|declarator| {
                    declarator
                        .child_by_field_name("name")
                        .is_some_and(|name_node| node_text(name_node, source) == identifier_name)
                });
            matches_name
                .then(|| field.child_by_field_name("type"))
                .flatten()
                .map(|type_node| node_text(type_node, source))
        })
}

/// Strips array brackets, generic type arguments, and any package
/// qualifier, e.g. `java.util.List<String>[]` -> `List` — matching the bare
/// simple-name form `SymbolInfo::owner` is stored in.
fn simple_type_name(type_text: &str) -> String {
    let mut text = type_text.trim();
    while let Some(stripped) = text.strip_suffix("[]") {
        text = stripped.trim();
    }
    let without_generics = match text.find('<') {
        Some(index) => &text[..index],
        None => text,
    };
    without_generics
        .rsplit('.')
        .next()
        .unwrap_or(without_generics)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Position;
    use tree_sitter::Parser;

    fn parse(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn position_of(source: &str, needle: &str) -> Position {
        let index = source.find(needle).expect("needle not found in source");
        Position::new(0, index as u32)
    }

    #[test]
    fn resolves_a_local_variables_declared_type_for_a_method_call() {
        let source =
            "class Main { void run() { Person person = new Person(); person.getName(); } }";
        let tree = parse(source);

        let reference = member_reference_at(&tree, source, position_of(source, "getName")).unwrap();

        assert_eq!(reference.name, "getName");
        assert_eq!(reference.receiver_type, "Person");
    }

    #[test]
    fn resolves_a_parameters_declared_type_for_a_method_call() {
        let source = "class Main { void run(Person person) { person.getName(); } }";
        let tree = parse(source);

        let reference = member_reference_at(&tree, source, position_of(source, "getName")).unwrap();

        assert_eq!(reference.receiver_type, "Person");
    }

    #[test]
    fn resolves_a_fields_declared_type_for_a_method_call() {
        let source = "class Main { Person person; void run() { person.getName(); } }";
        let tree = parse(source);

        let reference = member_reference_at(&tree, source, position_of(source, "getName")).unwrap();

        assert_eq!(reference.receiver_type, "Person");
    }

    #[test]
    fn resolves_this_to_the_enclosing_class() {
        let source = "class Person { void run() { this.getName(); } }";
        let tree = parse(source);

        let reference = member_reference_at(&tree, source, position_of(source, "getName")).unwrap();

        assert_eq!(reference.receiver_type, "Person");
    }

    #[test]
    fn resolves_an_unqualified_call_to_the_enclosing_class() {
        let source = "class Person { void run() { getName(); } }";
        let tree = parse(source);

        let reference = member_reference_at(&tree, source, position_of(source, "getName")).unwrap();

        assert_eq!(reference.receiver_type, "Person");
    }

    #[test]
    fn resolves_a_field_access_receivers_type() {
        let source =
            "class Main { void run() { Person person = new Person(); String n = person.name; } }";
        let tree = parse(source);

        let reference = member_reference_at(&tree, source, position_of(source, "name;")).unwrap();

        assert_eq!(reference.name, "name");
        assert_eq!(reference.receiver_type, "Person");
    }

    #[test]
    fn strips_generic_type_arguments_from_the_resolved_type() {
        let source = "class Main { void run() { List<String> items; items.size(); } }";
        let tree = parse(source);

        let reference = member_reference_at(&tree, source, position_of(source, "size")).unwrap();

        assert_eq!(reference.receiver_type, "List");
    }

    #[test]
    fn strips_the_package_qualifier_from_a_fully_qualified_type() {
        let source = "class Main { void run() { java.util.List items; items.size(); } }";
        let tree = parse(source);

        let reference = member_reference_at(&tree, source, position_of(source, "size")).unwrap();

        assert_eq!(reference.receiver_type, "List");
    }

    #[test]
    fn returns_none_for_a_chained_call_whose_receiver_is_itself_a_call() {
        let source = "class Main { void run() { getPerson().getName(); } }";
        let tree = parse(source);

        assert_eq!(
            member_reference_at(&tree, source, position_of(source, "getName")),
            None
        );
    }

    #[test]
    fn resolves_the_variable_actually_in_scope_not_a_sibling_blocks_same_named_variable() {
        let source = "class Main { void run() { { Person p = new Person(); } { Car p = new Car(); p.getName(); } } }";
        let tree = parse(source);

        let reference = member_reference_at(&tree, source, position_of(source, "getName")).unwrap();

        assert_eq!(reference.receiver_type, "Car");
    }

    #[test]
    fn resolve_declared_type_at_resolves_this_to_the_enclosing_class() {
        let source = "class Person { void run() { this.na } }";
        let tree = parse(source);
        let position = position_of(source, "this.na");

        let resolved = resolve_declared_type_at(&tree, source, position, "this");

        assert_eq!(resolved, Some("Person".to_string()));
    }

    #[test]
    fn returns_none_when_the_receiver_variable_is_not_in_scope() {
        let source = "class Main { void run() { person.getName(); } }";
        let tree = parse(source);

        assert_eq!(
            member_reference_at(&tree, source, position_of(source, "getName")),
            None
        );
    }

    #[test]
    fn returns_none_when_the_cursor_is_on_the_receiver_not_the_member_name() {
        let source =
            "class Main { void run() { Person person = new Person(); person.getName(); } }";
        let tree = parse(source);
        let position = position_of(source, "person.getName");

        assert_eq!(member_reference_at(&tree, source, position), None);
    }

    #[test]
    fn resolve_declared_type_at_resolves_a_local_variables_type_from_a_position_within_it() {
        let source = "class Main { void run() { Person person = new Person(); person.na } }";
        let tree = parse(source);
        let position = position_of(source, "person.na");

        let resolved = resolve_declared_type_at(&tree, source, position, "person");

        assert_eq!(resolved, Some("Person".to_string()));
    }

    #[test]
    fn resolve_declared_type_at_returns_none_for_an_identifier_not_in_scope() {
        let source = "class Main { void run() { person.na } }";
        let tree = parse(source);
        let position = position_of(source, "person.na");

        assert_eq!(
            resolve_declared_type_at(&tree, source, position, "person"),
            None
        );
    }

    #[test]
    fn returns_none_when_the_cursor_is_not_on_an_identifier() {
        let source = "class Main {}";
        let tree = parse(source);

        assert_eq!(
            member_reference_at(&tree, source, Position::new(0, 0)),
            None
        );
    }
}
