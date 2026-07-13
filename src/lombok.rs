//! Detects whether a Java source file uses Lombok, so only files that
//! actually need it get sent through the background `javac`+Lombok compile
//! (see `javac_compile`) — everything else stays on the existing, fast Tier
//! 2 tree-sitter path untouched. Also computes, from a compiled class and
//! its original hand-written source, which of the stub's indexed symbols
//! should be excluded (already correctly indexed from the real source) or
//! redirected (a generated accessor, pointed at its backing field instead
//! of the synthetic stub) — see `stub_symbol_overrides`.

use crate::class_stub;
use crate::classfile::ClassFile;
use crate::symbol::{SymbolInfo, SymbolKind};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, Tree};

/// True if `tree` contains an import of the `lombok` package (single-type
/// or wildcard) or a fully-qualified `lombok.*` annotation use.
pub fn uses_lombok(tree: &Tree, source: &str) -> bool {
    walk(tree.root_node(), source)
}

fn walk(node: Node, source: &str) -> bool {
    let matches = match node.kind() {
        "import_declaration" => import_path_is_lombok(node, source),
        "annotation" | "marker_annotation" => annotation_name_is_lombok(node, source),
        _ => false,
    };
    if matches {
        return true;
    }

    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| walk(child, source))
}

fn import_path_is_lombok(node: Node, source: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| matches!(child.kind(), "scoped_identifier" | "identifier"))
        .any(|path_node| is_lombok_path(node_text(path_node, source)))
}

fn annotation_name_is_lombok(node: Node, source: &str) -> bool {
    node.child_by_field_name("name")
        .filter(|name_node| name_node.kind() == "scoped_identifier")
        .is_some_and(|name_node| is_lombok_path(node_text(name_node, source)))
}

fn is_lombok_path(path: &str) -> bool {
    path == "lombok" || path.starts_with("lombok.")
}

fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

/// A non-cryptographic content hash, used only to skip re-triggering a
/// Lombok recompile for byte-identical content (e.g. a save with no actual
/// edit) — a hash collision here would at worst skip a redundant, harmless
/// recompile, not cause incorrect results.
pub fn content_hash(source: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

/// Extracts a file's declared package (e.g. `"dev.javalsp.testbed"`), or
/// `None` for a default-package file — needed to locate a compiled class's
/// expected path on disk from its source file's own name.
pub fn source_package(tree: &Tree, source: &str) -> Option<String> {
    let mut cursor = tree.root_node().walk();
    let package_node = tree
        .root_node()
        .children(&mut cursor)
        .find(|node| node.kind() == "package_declaration")?;

    let mut inner_cursor = package_node.walk();
    package_node
        .children(&mut inner_cursor)
        .find(|child| matches!(child.kind(), "scoped_identifier" | "identifier"))
        .map(|name_node| node_text(name_node, source).to_string())
}

/// Decides, for each of a compiled class's methods, whether the
/// corresponding entry the generic classfile→stub pipeline would index
/// should instead be **excluded** (`None` — the method is hand-written, and
/// `original_symbols` — from parsing the real source — already indexes it
/// correctly) or **redirected** (`Some(field)` — the method is a
/// Lombok-generated `getX`/`setX`/`isX` accessor for a field that really
/// exists in the original source). A generated method that's neither (e.g.
/// `equals`/`toString`, or a generated no-args constructor with no
/// hand-written counterpart) has no entry in the returned map at all — it's
/// left indexed via the stub, since there's no better real location.
///
/// Keyed by whatever name the stub's own `extract_symbols` would produce:
/// the method's own name for ordinary methods, but the class's *simple
/// name* for a constructor (`class.methods` names it `"<init>"`, but
/// `class_stub::render_stub` + `extract_symbols` name it after the class).
pub fn stub_symbol_overrides(
    class: &ClassFile,
    original_symbols: &[SymbolInfo],
) -> HashMap<String, Option<SymbolInfo>> {
    let mut overrides = HashMap::new();

    let has_hand_written_constructor = original_symbols
        .iter()
        .any(|symbol| symbol.kind == SymbolKind::Constructor);
    if has_hand_written_constructor && class.methods.iter().any(|m| m.name == "<init>") {
        let (_, simple_name) = class_stub::package_and_simple_name(&class.this_class);
        overrides.insert(simple_name, None);
    }

    let hand_written_method_names: HashSet<&str> = original_symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Method)
        .map(|symbol| symbol.name.as_str())
        .collect();
    let fields_by_name: HashMap<&str, &SymbolInfo> = original_symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Field)
        .map(|symbol| (symbol.name.as_str(), symbol))
        .collect();

    for method in &class.methods {
        if method.name == "<init>" {
            continue;
        }
        if hand_written_method_names.contains(method.name.as_str()) {
            overrides.insert(method.name.clone(), None);
            continue;
        }
        if let Some(field_name) = field_name_for_accessor(&method.name)
            && let Some(field) = fields_by_name.get(field_name.as_str())
        {
            overrides.insert(
                method.name.clone(),
                Some(SymbolInfo {
                    name: method.name.clone(),
                    kind: SymbolKind::Method,
                    uri: field.uri.clone(),
                    range: field.range,
                    selection_range: field.selection_range,
                    owner: field.owner.clone(),
                }),
            );
        }
    }

    overrides
}

/// The field name a JavaBean-shaped accessor exposes, e.g. `"getName"` /
/// `"setName"` -> `Some("name")`, `"isActive"` -> `Some("active")`. Requires
/// the character right after the prefix to be uppercase (standard JavaBean
/// capitalization) — rejects anything else, including a bare `"get"`/`"is"`/
/// `"set"` with nothing to decapitalize.
fn field_name_for_accessor(method_name: &str) -> Option<String> {
    let stripped = method_name
        .strip_prefix("get")
        .or_else(|| method_name.strip_prefix("set"))
        .or_else(|| method_name.strip_prefix("is"))?;
    let mut chars = stripped.chars();
    let first = chars.next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    Some(format!("{}{}", first.to_ascii_lowercase(), chars.as_str()))
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

    #[test]
    fn detects_a_single_type_lombok_import() {
        let source = "import lombok.Getter;\nclass Foo {}";
        assert!(uses_lombok(&parse(source), source));
    }

    #[test]
    fn detects_a_wildcard_lombok_import() {
        let source = "import lombok.*;\nclass Foo {}";
        assert!(uses_lombok(&parse(source), source));
    }

    #[test]
    fn detects_a_fully_qualified_lombok_annotation() {
        let source = "@lombok.Getter\nclass Foo {}";
        assert!(uses_lombok(&parse(source), source));
    }

    #[test]
    fn detects_a_fully_qualified_lombok_annotation_with_arguments() {
        let source = "@lombok.Getter(lazy = true)\nclass Foo {}";
        assert!(uses_lombok(&parse(source), source));
    }

    #[test]
    fn returns_false_for_code_with_no_lombok_usage() {
        let source = "import java.util.List;\nclass Foo { List names; }";
        assert!(!uses_lombok(&parse(source), source));
    }

    #[test]
    fn does_not_false_positive_on_a_similarly_named_package() {
        let source = "import com.acme.lombokish.Thing;\nclass Foo {}";
        assert!(!uses_lombok(&parse(source), source));
    }

    #[test]
    fn content_hash_is_stable_for_identical_content() {
        assert_eq!(content_hash("class Foo {}"), content_hash("class Foo {}"));
    }

    #[test]
    fn content_hash_differs_for_different_content() {
        assert_ne!(content_hash("class Foo {}"), content_hash("class Bar {}"));
    }

    #[test]
    fn source_package_extracts_the_declared_package() {
        let source = "package dev.javalsp.testbed;\nclass Foo {}";
        assert_eq!(
            source_package(&parse(source), source),
            Some("dev.javalsp.testbed".to_string())
        );
    }

    #[test]
    fn source_package_returns_none_for_a_default_package_file() {
        let source = "class Foo {}";
        assert_eq!(source_package(&parse(source), source), None);
    }

    #[test]
    fn field_name_for_accessor_handles_get_set_and_is() {
        assert_eq!(field_name_for_accessor("getName"), Some("name".to_string()));
        assert_eq!(field_name_for_accessor("setName"), Some("name".to_string()));
        assert_eq!(
            field_name_for_accessor("isActive"),
            Some("active".to_string())
        );
    }

    #[test]
    fn field_name_for_accessor_rejects_non_accessor_shapes() {
        assert_eq!(field_name_for_accessor("get"), None);
        assert_eq!(field_name_for_accessor("is"), None);
        assert_eq!(field_name_for_accessor("foo"), None);
        assert_eq!(field_name_for_accessor("getname"), None);
    }

    use crate::classfile::Member;
    use lsp_types::{Position, Range, Uri};

    fn class_file(this_class: &str, methods: Vec<Member>) -> ClassFile {
        ClassFile {
            access_flags: 0x0001,
            this_class: this_class.to_string(),
            super_class: Some("java/lang/Object".to_string()),
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods,
        }
    }

    fn member(name: &str) -> Member {
        Member {
            name: name.to_string(),
            descriptor: "()V".to_string(),
            access_flags: 0x0001,
        }
    }

    fn symbol(name: &str, kind: SymbolKind) -> SymbolInfo {
        symbol_with_owner(name, kind, None)
    }

    fn symbol_with_owner(name: &str, kind: SymbolKind, owner: Option<&str>) -> SymbolInfo {
        let uri: Uri = "file:///Person.java".parse().unwrap();
        let range = Range::new(Position::new(0, 0), Position::new(0, 1));
        SymbolInfo {
            name: name.to_string(),
            kind,
            uri,
            range,
            selection_range: range,
            owner: owner.map(str::to_string),
        }
    }

    #[test]
    fn stub_symbol_overrides_excludes_a_hand_written_method() {
        let class = class_file("Person", vec![member("sayHello")]);
        let original_symbols = vec![symbol("sayHello", SymbolKind::Method)];

        let overrides = stub_symbol_overrides(&class, &original_symbols);

        assert_eq!(overrides.get("sayHello"), Some(&None));
    }

    #[test]
    fn stub_symbol_overrides_excludes_a_hand_written_constructor() {
        let class = class_file("Person", vec![member("<init>")]);
        let original_symbols = vec![symbol("Person", SymbolKind::Constructor)];

        let overrides = stub_symbol_overrides(&class, &original_symbols);

        assert_eq!(overrides.get("Person"), Some(&None));
    }

    #[test]
    fn stub_symbol_overrides_leaves_a_generated_constructor_with_no_hand_written_one_alone() {
        let class = class_file("Person", vec![member("<init>")]);
        let original_symbols: Vec<SymbolInfo> = vec![];

        let overrides = stub_symbol_overrides(&class, &original_symbols);

        assert!(!overrides.contains_key("Person"));
    }

    #[test]
    fn stub_symbol_overrides_redirects_a_generated_accessor_to_its_field() {
        let class = class_file("Person", vec![member("getName")]);
        let field = symbol("name", SymbolKind::Field);
        let original_symbols = vec![field.clone()];

        let overrides = stub_symbol_overrides(&class, &original_symbols);

        let redirected = overrides
            .get("getName")
            .expect("getName should have an override entry")
            .as_ref()
            .expect("getName should redirect, not exclude");
        assert_eq!(redirected.name, "getName");
        assert_eq!(redirected.kind, SymbolKind::Method);
        assert_eq!(redirected.uri, field.uri);
        assert_eq!(redirected.range, field.range);
    }

    #[test]
    fn stub_symbol_overrides_redirected_accessor_keeps_the_fields_owner() {
        let class = class_file("Person", vec![member("getName")]);
        let field = symbol_with_owner("name", SymbolKind::Field, Some("Person"));
        let original_symbols = vec![field];

        let overrides = stub_symbol_overrides(&class, &original_symbols);

        let redirected = overrides
            .get("getName")
            .expect("getName should have an override entry")
            .as_ref()
            .expect("getName should redirect, not exclude");
        assert_eq!(redirected.owner.as_deref(), Some("Person"));
    }

    #[test]
    fn stub_symbol_overrides_redirects_an_accessor_to_its_fields_owner() {
        let class = class_file("Person", vec![member("getName")]);
        let field = symbol_with_owner("name", SymbolKind::Field, Some("Person"));
        let original_symbols = vec![field];

        let overrides = stub_symbol_overrides(&class, &original_symbols);

        let redirected = overrides
            .get("getName")
            .expect("getName should have an override entry")
            .as_ref()
            .expect("getName should redirect, not exclude");
        assert_eq!(redirected.owner.as_deref(), Some("Person"));
    }

    #[test]
    fn stub_symbol_overrides_leaves_a_generated_non_accessor_member_alone() {
        let class = class_file("Person", vec![member("toString")]);
        let original_symbols: Vec<SymbolInfo> = vec![];

        let overrides = stub_symbol_overrides(&class, &original_symbols);

        assert!(!overrides.contains_key("toString"));
    }

    #[test]
    fn stub_symbol_overrides_does_not_redirect_an_accessor_with_no_matching_field() {
        let class = class_file("Person", vec![member("getName")]);
        let original_symbols: Vec<SymbolInfo> = vec![];

        let overrides = stub_symbol_overrides(&class, &original_symbols);

        assert!(!overrides.contains_key("getName"));
    }
}
