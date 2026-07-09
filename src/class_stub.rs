//! Renders a synthetic, decompiled-*skeleton* `.java` source file from a
//! parsed [`ClassFile`] — package + type declaration + visible field/method
//! *signatures*, bodies always `{}`, generics/annotations erased. This lets
//! `SyntaxTree::parse` + `symbol::extract_symbols` (the existing, unmodified
//! Tier 1/2 pipeline) produce real `SymbolInfo`s with a real `file://` URI
//! and real ranges for classfile-derived types, which is what makes
//! go-to-definition work in a real editor: an LSP `Location` needs a real,
//! openable file, not a synthetic scheme a client wouldn't know how to open.

use crate::classfile::{
    ACC_BRIDGE, ACC_PROTECTED, ACC_PUBLIC, ACC_STATIC, ACC_SYNTHETIC, ClassFile, Member,
};

/// Splits a binary class name (e.g. `java/util/List` or
/// `java/util/Map$Entry`) into its package (dotted, if any) and simple name.
/// A nested class's simple name is the substring after the last `$` — it
/// gets rendered as its own flat top-level stub, matching `symbol.rs`'s
/// existing simple-name-only model.
pub fn package_and_simple_name(binary_name: &str) -> (Option<String>, String) {
    let (package, class_segment) = match binary_name.rfind('/') {
        Some(index) => (
            Some(binary_name[..index].replace('/', ".")),
            &binary_name[index + 1..],
        ),
        None => (None, binary_name),
    };
    let simple_name = match class_segment.rfind('$') {
        Some(index) => &class_segment[index + 1..],
        None => class_segment,
    };
    (package, simple_name.to_string())
}

/// Whether `name` could stand alone as a Java type identifier — false for
/// synthetic/anonymous/local nested class names like `1` (from `Outer$1`),
/// which a caller should skip rather than render into an invalid stub.
pub fn is_valid_simple_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_alphabetic() || first == '_' || first == '$' => {
            chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        }
        _ => false,
    }
}

fn binary_name_to_dotted(binary_name: &str) -> String {
    binary_name.replace('/', ".")
}

fn decode_one_field_type(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<String> {
    let mut array_suffix = String::new();
    while chars.peek() == Some(&'[') {
        chars.next();
        array_suffix.push_str("[]");
    }
    let base = match chars.next()? {
        'B' => "byte".to_string(),
        'C' => "char".to_string(),
        'D' => "double".to_string(),
        'F' => "float".to_string(),
        'I' => "int".to_string(),
        'J' => "long".to_string(),
        'S' => "short".to_string(),
        'Z' => "boolean".to_string(),
        'L' => {
            let name: String = chars.by_ref().take_while(|&c| c != ';').collect();
            binary_name_to_dotted(&name)
        }
        _ => return None,
    };
    Some(format!("{base}{array_suffix}"))
}

/// Decodes a single JVM field-type descriptor (JVMS §4.3.2), e.g.
/// `Ljava/lang/String;` -> `java.lang.String`, `[I` -> `int[]`.
pub fn decode_field_type(descriptor: &str) -> String {
    let mut chars = descriptor.chars().peekable();
    decode_one_field_type(&mut chars).unwrap_or_else(|| "java.lang.Object".to_string())
}

fn decode_field_types(descriptor: &str) -> Vec<String> {
    let mut chars = descriptor.chars().peekable();
    let mut result = Vec::new();
    while chars.peek().is_some() {
        match decode_one_field_type(&mut chars) {
            Some(field_type) => result.push(field_type),
            None => break,
        }
    }
    result
}

pub struct MethodSignature {
    pub parameter_types: Vec<String>,
    pub return_type: String,
}

/// Decodes a JVM method descriptor (JVMS §4.3.3), e.g. `(Ljava/lang/String;I)Z`.
pub fn decode_method_descriptor(descriptor: &str) -> MethodSignature {
    let Some(params_part) = descriptor
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(')'))
        .map(|(params, _)| params)
    else {
        return MethodSignature {
            parameter_types: Vec::new(),
            return_type: "java.lang.Object".to_string(),
        };
    };
    let return_part = descriptor
        .rsplit_once(')')
        .map(|(_, ret)| ret)
        .unwrap_or("");

    MethodSignature {
        parameter_types: decode_field_types(params_part),
        return_type: if return_part == "V" {
            "void".to_string()
        } else {
            decode_field_type(return_part)
        },
    }
}

fn is_visible(access_flags: u16) -> bool {
    access_flags & (ACC_PUBLIC | ACC_PROTECTED) != 0
        && access_flags & (ACC_SYNTHETIC | ACC_BRIDGE) == 0
}

fn visibility_keyword(access_flags: u16) -> &'static str {
    if access_flags & ACC_PUBLIC != 0 {
        "public"
    } else {
        "protected"
    }
}

fn render_field(out: &mut String, field: &Member) {
    let type_name = decode_field_type(&field.descriptor);
    out.push_str(&format!(
        "    {} {} {};\n",
        visibility_keyword(field.access_flags),
        type_name,
        field.name
    ));
}

fn render_parameters(parameter_types: &[String]) -> String {
    parameter_types
        .iter()
        .enumerate()
        .map(|(index, param_type)| format!("{param_type} p{index}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_method(out: &mut String, method: &Member, simple_name: &str) {
    let visibility = visibility_keyword(method.access_flags);
    let signature = decode_method_descriptor(&method.descriptor);
    let params = render_parameters(&signature.parameter_types);

    if method.name == "<init>" {
        out.push_str(&format!("    {visibility} {simple_name}({params}) {{}}\n"));
        return;
    }

    let static_keyword = if method.access_flags & ACC_STATIC != 0 {
        "static "
    } else {
        ""
    };
    out.push_str(&format!(
        "    {visibility} {static_keyword}{} {}({params}) {{}}\n",
        signature.return_type, method.name
    ));
}

fn supertypes_clause(class: &ClassFile) -> String {
    let interfaces: Vec<String> = class
        .interfaces
        .iter()
        .map(|name| binary_name_to_dotted(name))
        .collect();

    if class.is_interface() {
        if interfaces.is_empty() {
            String::new()
        } else {
            format!(" extends {}", interfaces.join(", "))
        }
    } else if class.is_enum() {
        if interfaces.is_empty() {
            String::new()
        } else {
            format!(" implements {}", interfaces.join(", "))
        }
    } else {
        let mut clause = String::new();
        if let Some(super_class) = &class.super_class {
            let super_name = binary_name_to_dotted(super_class);
            if super_name != "java.lang.Object" {
                clause.push_str(&format!(" extends {super_name}"));
            }
        }
        if !interfaces.is_empty() {
            clause.push_str(&format!(" implements {}", interfaces.join(", ")));
        }
        clause
    }
}

/// Renders `class` as synthetic `.java` source text — a real package/type
/// declaration with visible member signatures, always parseable by
/// tree-sitter-java, never intended to be compiled.
pub fn render_stub(class: &ClassFile) -> String {
    let (package, simple_name) = package_and_simple_name(&class.this_class);
    let mut out = String::new();

    if let Some(package) = &package {
        out.push_str(&format!("package {package};\n\n"));
    }

    let keyword = if class.is_enum() {
        "enum"
    } else if class.is_interface() {
        "interface"
    } else {
        "class"
    };
    out.push_str(&format!(
        "public {keyword} {simple_name}{}",
        supertypes_clause(class)
    ));
    out.push_str(" {\n");

    // Enum constants can't be expressed as ordinary field declarations
    // (that's a distinct, dedicated syntax) — skipped for M4's scope. The
    // grammar requires a `;` separating an (empty) constant list from the
    // members that follow.
    if keyword == "enum" {
        out.push_str("    ;\n");
    } else {
        for field in class.fields.iter().filter(|f| is_visible(f.access_flags)) {
            render_field(&mut out, field);
        }
    }

    for method in class.methods.iter().filter(|m| is_visible(m.access_flags)) {
        render_method(&mut out, method, &simple_name);
    }

    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol::{self, SymbolKind};
    use crate::syntax::SyntaxTree;
    use lsp_types::Uri;
    use std::path::Path;

    fn fixture_classfile(name: &str) -> ClassFile {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/classfiles")
            .join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{path:?}: {err}"));
        crate::classfile::parse(&bytes).unwrap()
    }

    fn member(name: &str, descriptor: &str, access_flags: u16) -> Member {
        Member {
            name: name.to_string(),
            descriptor: descriptor.to_string(),
            access_flags,
        }
    }

    fn extract(rendered: &str) -> Vec<symbol::SymbolInfo> {
        let syntax = SyntaxTree::parse(rendered);
        assert!(
            !syntax.tree().root_node().has_error(),
            "rendered stub had a syntax error:\n{rendered}"
        );
        let uri: Uri = "file:///Stub.java".parse().unwrap();
        symbol::extract_symbols(&uri, syntax.source(), syntax.tree())
    }

    #[test]
    fn decode_field_type_handles_primitives_objects_and_arrays() {
        assert_eq!(decode_field_type("I"), "int");
        assert_eq!(decode_field_type("Z"), "boolean");
        assert_eq!(decode_field_type("Ljava/util/List;"), "java.util.List");
        assert_eq!(decode_field_type("[I"), "int[]");
        assert_eq!(
            decode_field_type("[[Ljava/lang/String;"),
            "java.lang.String[][]"
        );
    }

    #[test]
    fn decode_method_descriptor_reads_parameters_and_return_type() {
        let signature = decode_method_descriptor("(Ljava/lang/String;I)Z");

        assert_eq!(
            signature.parameter_types,
            vec!["java.lang.String".to_string(), "int".to_string()]
        );
        assert_eq!(signature.return_type, "boolean");
    }

    #[test]
    fn decode_method_descriptor_handles_no_arguments_and_void_return() {
        let signature = decode_method_descriptor("()V");

        assert!(signature.parameter_types.is_empty());
        assert_eq!(signature.return_type, "void");
    }

    #[test]
    fn package_and_simple_name_splits_a_top_level_binary_name() {
        assert_eq!(
            package_and_simple_name("java/util/List"),
            (Some("java.util".to_string()), "List".to_string())
        );
    }

    #[test]
    fn package_and_simple_name_handles_a_type_with_no_package() {
        assert_eq!(package_and_simple_name("Main"), (None, "Main".to_string()));
    }

    #[test]
    fn package_and_simple_name_strips_the_outer_class_prefix_of_a_nested_type() {
        assert_eq!(
            package_and_simple_name("java/util/Map$Entry"),
            (Some("java.util".to_string()), "Entry".to_string())
        );
    }

    #[test]
    fn is_valid_simple_name_rejects_anonymous_class_names() {
        assert!(is_valid_simple_name("Entry"));
        assert!(!is_valid_simple_name("1"));
        assert!(!is_valid_simple_name(""));
    }

    #[test]
    fn render_stub_of_a_class_round_trips_through_extract_symbols() {
        let class = fixture_classfile("Simple.class");

        let rendered = render_stub(&class);
        let symbols = extract(&rendered);

        assert_eq!(
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Class)
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Simple"]
        );
        // `name` is a private field — excluded from the stub entirely.
        assert_eq!(
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Field)
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["count"]
        );
        let mut methods: Vec<&str> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .map(|s| s.name.as_str())
            .collect();
        methods.sort_unstable();
        assert_eq!(methods, vec!["getCount", "setName"]);
        assert_eq!(
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Constructor)
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Simple"]
        );
    }

    #[test]
    fn render_stub_of_an_interface_round_trips_through_extract_symbols() {
        let class = fixture_classfile("Greetable.class");

        let rendered = render_stub(&class);
        let symbols = extract(&rendered);

        assert_eq!(
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Interface)
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Greetable"]
        );
        assert_eq!(
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Method)
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["greet"]
        );
    }

    #[test]
    fn render_stub_of_an_implementing_class_declares_implements_clause() {
        let class = fixture_classfile("Impl.class");

        let rendered = render_stub(&class);
        assert!(rendered.contains("implements Greetable"));
        let symbols = extract(&rendered);

        assert_eq!(
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Class)
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Impl"]
        );
    }

    #[test]
    fn render_stub_excludes_a_package_private_field_and_the_class_initializer() {
        let class = fixture_classfile("WithStatic.class");

        let rendered = render_stub(&class);
        let symbols = extract(&rendered);

        assert!(
            symbols.iter().all(|s| s.kind != SymbolKind::Field),
            "package-private field should not appear in the stub: {rendered}"
        );
        assert_eq!(
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Constructor)
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["WithStatic"]
        );
    }

    #[test]
    fn render_stub_of_an_enum_omits_extends_and_field_declarations() {
        let class = ClassFile {
            access_flags: crate::classfile::ACC_PUBLIC | crate::classfile::ACC_ENUM,
            this_class: "Color".to_string(),
            super_class: Some("java/lang/Enum".to_string()),
            interfaces: Vec::new(),
            fields: vec![member(
                "RED",
                "LColor;",
                crate::classfile::ACC_PUBLIC | ACC_STATIC,
            )],
            methods: vec![member(
                "values",
                "()[LColor;",
                crate::classfile::ACC_PUBLIC | ACC_STATIC,
            )],
        };

        let rendered = render_stub(&class);
        assert!(!rendered.contains("extends"));
        assert!(!rendered.contains("RED"));
        let symbols = extract(&rendered);

        assert_eq!(
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Enum)
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Color"]
        );
        assert_eq!(
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Method)
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["values"]
        );
    }
}
