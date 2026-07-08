use std::io::{self, BufRead, Write};

const MAX_MESSAGE_LEN: usize = 64 * 1024 * 1024;

pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    let mut saw_any_header_line = false;

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;

        if bytes_read == 0 {
            if saw_any_header_line {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended in the middle of a message header",
                ));
            }
            return Ok(None);
        }

        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        saw_any_header_line = true;

        if let Some(value) = line
            .split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim())
        {
            content_length = Some(value.parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length value")
            })?);
        }
    }

    let content_length = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;

    if content_length > MAX_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Content-Length exceeds maximum message size",
        ));
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

pub fn write_message<W: Write>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_message_returns_body_for_well_formed_message() {
        let input = b"Content-Length: 13\r\n\r\n{\"foo\":\"bar\"}".to_vec();
        let mut cursor = Cursor::new(input);

        let body = read_message(&mut cursor).unwrap();

        assert_eq!(body, Some(b"{\"foo\":\"bar\"}".to_vec()));
    }

    #[test]
    fn read_message_returns_none_on_clean_eof() {
        let mut cursor = Cursor::new(Vec::new());

        let body = read_message(&mut cursor).unwrap();

        assert_eq!(body, None);
    }

    #[test]
    fn read_message_errors_on_missing_content_length_header() {
        let input = b"Foo: bar\r\n\r\nirrelevant".to_vec();
        let mut cursor = Cursor::new(input);

        let result = read_message(&mut cursor);

        assert!(result.is_err());
    }

    #[test]
    fn read_message_errors_on_content_length_exceeding_maximum() {
        let input = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_LEN + 1).into_bytes();
        let mut cursor = Cursor::new(input);

        let result = read_message(&mut cursor);

        assert!(result.is_err());
    }

    #[test]
    fn write_message_round_trips_through_read_message() {
        let body = b"{\"hello\":\"world\"}".to_vec();
        let mut buffer = Vec::new();

        write_message(&mut buffer, &body).unwrap();
        let mut cursor = Cursor::new(buffer);
        let read_back = read_message(&mut cursor).unwrap();

        assert_eq!(read_back, Some(body));
    }
}
