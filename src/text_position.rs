use lsp_types::Position;

pub fn position_to_byte_offset(source: &str, position: Position) -> usize {
    position_to_byte_offset_and_column(source, position).0
}

/// Returns the byte offset of `position`, together with its column expressed
/// as a byte offset within its line (as tree-sitter's `Point::column` expects,
/// unlike `Position::character`'s UTF-16 units).
pub fn position_to_byte_offset_and_column(source: &str, position: Position) -> (usize, usize) {
    let mut line = 0u32;
    let mut character = 0u32;
    let mut line_start_byte = 0usize;

    for (byte_offset, ch) in source.char_indices() {
        if line == position.line && character >= position.character {
            return (byte_offset, byte_offset - line_start_byte);
        }
        if ch == '\n' {
            if line == position.line {
                return (byte_offset, byte_offset - line_start_byte);
            }
            line += 1;
            character = 0;
            line_start_byte = byte_offset + ch.len_utf8();
        } else if line == position.line {
            character += ch.len_utf16() as u32;
        }
    }

    (source.len(), source.len() - line_start_byte)
}

pub fn byte_offset_to_position(source: &str, byte_offset: usize) -> Position {
    let mut line = 0u32;
    let mut character = 0u32;

    for (idx, ch) in source.char_indices() {
        if idx >= byte_offset {
            return Position::new(line, character);
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }

    Position::new(line, character)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_to_byte_offset_finds_column_on_first_line() {
        let source = "abc";

        let offset = position_to_byte_offset(source, Position::new(0, 2));

        assert_eq!(offset, 2);
    }

    #[test]
    fn position_to_byte_offset_finds_column_on_later_line() {
        let source = "abc\ndef\nghi";

        let offset = position_to_byte_offset(source, Position::new(2, 1));

        assert_eq!(offset, 9);
    }

    #[test]
    fn position_to_byte_offset_handles_multibyte_characters_before_target_column() {
        let source = "€abc";

        let offset = position_to_byte_offset(source, Position::new(0, 2));

        assert_eq!(offset, "€a".len());
    }

    #[test]
    fn position_to_byte_offset_counts_astral_characters_as_two_utf16_units() {
        let source = "𝄞x";

        let offset = position_to_byte_offset(source, Position::new(0, 2));

        assert_eq!(offset, "𝄞".len());
    }

    #[test]
    fn position_to_byte_offset_clamps_character_past_end_of_line() {
        let source = "ab\ncd";

        let offset = position_to_byte_offset(source, Position::new(0, 100));

        assert_eq!(offset, 2);
    }

    #[test]
    fn position_to_byte_offset_clamps_line_past_end_of_source() {
        let source = "abc";

        let offset = position_to_byte_offset(source, Position::new(5, 0));

        assert_eq!(offset, source.len());
    }

    #[test]
    fn byte_offset_to_position_round_trips_with_position_to_byte_offset() {
        let source = "abc\ndef\nghi";
        let position = Position::new(2, 1);

        let offset = position_to_byte_offset(source, position);
        let round_tripped = byte_offset_to_position(source, offset);

        assert_eq!(round_tripped, position);
    }

    #[test]
    fn byte_offset_to_position_handles_multibyte_characters() {
        let source = "€abc";
        let offset = "€".len();

        let position = byte_offset_to_position(source, offset);

        assert_eq!(position, Position::new(0, 1));
    }

    #[test]
    fn position_to_byte_offset_and_column_reports_byte_column_not_utf16_column() {
        let source = "€ab";

        let (offset, column) = position_to_byte_offset_and_column(source, Position::new(0, 2));

        assert_eq!((offset, column), ("€a".len(), "€a".len()));
    }

    #[test]
    fn position_to_byte_offset_and_column_resets_column_after_newline() {
        let source = "abc\ndef";

        let (offset, column) = position_to_byte_offset_and_column(source, Position::new(1, 0));

        assert_eq!((offset, column), (source.find('d').unwrap(), 0));
    }
}
