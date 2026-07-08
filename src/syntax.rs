use crate::text_position::{
    byte_offset_to_position, position_to_byte_offset, position_to_byte_offset_and_column,
};
use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range, TextDocumentContentChangeEvent};
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

fn java_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("tree-sitter-java grammar is always a valid language");
    parser
}

fn parse_fresh(source: &str) -> Tree {
    java_parser()
        .parse(source, None)
        .expect("parsing a &str never times out or is cancelled")
}

fn point_after_insert(start: Point, inserted: &str) -> Point {
    match inserted.rfind('\n') {
        Some(last_newline_byte) => Point {
            row: start.row + inserted.matches('\n').count(),
            column: inserted.len() - last_newline_byte - 1,
        },
        None => Point {
            row: start.row,
            column: start.column + inserted.len(),
        },
    }
}

pub struct SyntaxTree {
    source: String,
    tree: Tree,
}

impl SyntaxTree {
    pub fn parse(source: &str) -> Self {
        Self {
            source: source.to_string(),
            tree: parse_fresh(source),
        }
    }

    pub fn edit(&mut self, change: &TextDocumentContentChangeEvent) {
        let Some(range) = change.range else {
            self.source = change.text.clone();
            self.tree = parse_fresh(&self.source);
            return;
        };

        let (mut start_byte, start_column) =
            position_to_byte_offset_and_column(&self.source, range.start);
        let (mut old_end_byte, old_end_column) =
            position_to_byte_offset_and_column(&self.source, range.end);
        let mut start_position = Point {
            row: range.start.line as usize,
            column: start_column,
        };
        let mut old_end_position = Point {
            row: range.end.line as usize,
            column: old_end_column,
        };

        if start_byte > old_end_byte {
            std::mem::swap(&mut start_byte, &mut old_end_byte);
            std::mem::swap(&mut start_position, &mut old_end_position);
        }

        let new_end_position = point_after_insert(start_position, &change.text);

        let mut new_source = self.source.clone();
        new_source.replace_range(start_byte..old_end_byte, &change.text);
        let new_end_byte = start_byte + change.text.len();

        self.tree.edit(&InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position,
        });
        self.source = new_source;

        self.tree = java_parser()
            .parse(&self.source, Some(&self.tree))
            .expect("parsing a &str never times out or is cancelled");
    }

    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        if !self.tree.root_node().has_error() {
            return Vec::new();
        }

        let mut diagnostics = Vec::new();
        collect_error_diagnostics(self.tree.root_node(), &self.source, &mut diagnostics);
        diagnostics
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn identifier_at(&self, position: Position) -> Option<(String, Range)> {
        let byte_offset = position_to_byte_offset(&self.source, position);
        let node = self
            .tree
            .root_node()
            .descendant_for_byte_range(byte_offset, byte_offset)?;

        if !matches!(node.kind(), "identifier" | "type_identifier") {
            return None;
        }

        let range = Range::new(
            byte_offset_to_position(&self.source, node.start_byte()),
            byte_offset_to_position(&self.source, node.end_byte()),
        );
        Some((
            self.source[node.start_byte()..node.end_byte()].to_string(),
            range,
        ))
    }

    pub fn identifier_prefix_before(&self, position: Position) -> String {
        let byte_offset = position_to_byte_offset(&self.source, position);
        let mut start = byte_offset;
        for (idx, ch) in self.source[..byte_offset].char_indices().rev() {
            if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                start = idx;
            } else {
                break;
            }
        }
        self.source[start..byte_offset].to_string()
    }
}

fn collect_error_diagnostics(node: Node, source: &str, diagnostics: &mut Vec<Diagnostic>) {
    if node.is_error() || node.is_missing() {
        diagnostics.push(Diagnostic {
            range: Range::new(
                byte_offset_to_position(source, node.start_byte()),
                byte_offset_to_position(source, node.end_byte()),
            ),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("java-lsp".to_string()),
            message: if node.is_missing() {
                format!("syntax error: missing {}", node.kind())
            } else {
                "syntax error".to_string()
            },
            ..Diagnostic::default()
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_error_diagnostics(child, source, diagnostics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Position;

    #[test]
    fn valid_source_has_no_diagnostics() {
        let tree = SyntaxTree::parse("class Main { public static void main(String[] args) {} }");

        assert_eq!(tree.diagnostics(), Vec::new());
    }

    #[test]
    fn missing_semicolon_produces_a_diagnostic() {
        let tree = SyntaxTree::parse("class Main { void run() { int x = 1 } }");

        let diagnostics = tree.diagnostics();

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
    }

    #[test]
    fn unbalanced_brace_produces_a_diagnostic() {
        let tree = SyntaxTree::parse("class Main { void run() {");

        assert!(!tree.diagnostics().is_empty());
    }

    #[test]
    fn full_document_change_without_range_reparses_from_scratch() {
        let mut tree = SyntaxTree::parse("class Main { void run() { int x = 1 } }");
        assert!(!tree.diagnostics().is_empty());

        tree.edit(&TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "class Main { void run() { int x = 1; } }".to_string(),
        });

        assert!(tree.diagnostics().is_empty());
    }

    #[test]
    fn incremental_change_inserting_missing_semicolon_clears_the_diagnostic() {
        let mut tree = SyntaxTree::parse("class Main { void run() { int x = 1 } }");
        assert!(!tree.diagnostics().is_empty());

        let insert_at = Position::new(0, "class Main { void run() { int x = 1".len() as u32);
        tree.edit(&TextDocumentContentChangeEvent {
            range: Some(Range::new(insert_at, insert_at)),
            range_length: None,
            text: ";".to_string(),
        });

        assert_eq!(tree.diagnostics(), Vec::new());
    }

    #[test]
    fn incremental_change_deleting_a_brace_introduces_a_diagnostic() {
        let source = "class Main { void run() { int x = 1; } }";
        let mut tree = SyntaxTree::parse(source);
        assert!(tree.diagnostics().is_empty());

        let end = Position::new(0, source.len() as u32);
        let start = Position::new(0, source.len() as u32 - 1);
        tree.edit(&TextDocumentContentChangeEvent {
            range: Some(Range::new(start, end)),
            range_length: None,
            text: String::new(),
        });

        assert!(!tree.diagnostics().is_empty());
    }

    #[test]
    fn edit_with_a_backwards_range_does_not_panic() {
        let source = "class Main { void run() { int x = 1; } }";
        let mut tree = SyntaxTree::parse(source);

        let earlier = Position::new(0, 5);
        let later = Position::new(0, 10);
        tree.edit(&TextDocumentContentChangeEvent {
            range: Some(Range::new(later, earlier)),
            range_length: None,
            text: String::new(),
        });

        assert!(!tree.diagnostics().is_empty());
    }

    #[test]
    fn edit_inserting_text_with_newlines_tracks_the_new_end_position() {
        let mut tree = SyntaxTree::parse("class Main { void run() { int x = 1 } }");

        let insert_at = Position::new(0, "class Main { void run() { int x = 1".len() as u32);
        tree.edit(&TextDocumentContentChangeEvent {
            range: Some(Range::new(insert_at, insert_at)),
            range_length: None,
            text: ";\n    int y = 2;\n".to_string(),
        });

        assert_eq!(tree.diagnostics(), Vec::new());
    }

    #[test]
    fn tree_and_source_accessors_expose_current_state() {
        let tree = SyntaxTree::parse("class Main {}");

        assert_eq!(tree.source(), "class Main {}");
        assert!(!tree.tree().root_node().has_error());
    }

    #[test]
    fn identifier_at_returns_the_identifier_under_the_cursor() {
        let source = "class Main {}";
        let tree = SyntaxTree::parse(source);
        let position = "class Ma".len() as u32;

        let (name, range) = tree.identifier_at(Position::new(0, position)).unwrap();

        assert_eq!(name, "Main");
        assert_eq!(
            range,
            Range::new(
                Position::new(0, "class ".len() as u32),
                Position::new(0, "class Main".len() as u32)
            )
        );
    }

    #[test]
    fn identifier_at_recognizes_type_identifier_nodes() {
        let source = "class Main { Helper helper; }";
        let tree = SyntaxTree::parse(source);
        let position = "class Main { Hel".len() as u32;

        let (name, _) = tree.identifier_at(Position::new(0, position)).unwrap();

        assert_eq!(name, "Helper");
    }

    #[test]
    fn identifier_at_returns_none_when_not_on_an_identifier() {
        let tree = SyntaxTree::parse("class Main {}");

        assert_eq!(tree.identifier_at(Position::new(0, 0)), None);
    }

    #[test]
    fn identifier_prefix_before_returns_the_partial_word_before_the_cursor() {
        let tree = SyntaxTree::parse("class Main { void run() { int gree } }");
        let position = "class Main { void run() { int gree".len() as u32;

        let prefix = tree.identifier_prefix_before(Position::new(0, position));

        assert_eq!(prefix, "gree");
    }

    #[test]
    fn identifier_prefix_before_is_empty_right_after_a_non_identifier_character() {
        let tree = SyntaxTree::parse("class Main {}");

        let prefix = tree.identifier_prefix_before(Position::new(0, 6));

        assert_eq!(prefix, "");
    }
}
