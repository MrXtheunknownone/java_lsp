//! Hand-rolled parser for the subset of the JVM classfile format ([JVMS §4])
//! this project needs: enough of the constant pool to resolve class and
//! member names, `this_class`/`super_class`/`interfaces`, and field/method
//! name+descriptor+flags. Bytecode (`Code` attribute bodies), annotations,
//! and generic `Signature` attributes are deliberately unparsed — skipped by
//! their declared length, never interpreted.
//!
//! [JVMS §4]: https://docs.oracle.com/javase/specs/jvms/se21/html/jvms-4.html

const MAGIC: u32 = 0xCAFE_BABE;

pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_PROTECTED: u16 = 0x0004;
pub const ACC_STATIC: u16 = 0x0008;
pub const ACC_INTERFACE: u16 = 0x0200;
pub const ACC_ABSTRACT: u16 = 0x0400;
pub const ACC_SYNTHETIC: u16 = 0x1000;
pub const ACC_ANNOTATION: u16 = 0x2000;
pub const ACC_ENUM: u16 = 0x4000;
pub const ACC_BRIDGE: u16 = 0x0040;

#[derive(Debug, PartialEq, Eq)]
pub enum ClassFileError {
    InvalidMagic,
    UnexpectedEof,
    InvalidConstantPoolIndex,
    InvalidConstantPoolTag(u8),
    InvalidModifiedUtf8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub name: String,
    pub descriptor: String,
    pub access_flags: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassFile {
    pub access_flags: u16,
    pub this_class: String,
    pub super_class: Option<String>,
    pub interfaces: Vec<String>,
    pub fields: Vec<Member>,
    pub methods: Vec<Member>,
}

impl ClassFile {
    pub fn is_interface(&self) -> bool {
        self.access_flags & ACC_INTERFACE != 0
    }

    pub fn is_enum(&self) -> bool {
        self.access_flags & ACC_ENUM != 0
    }

    pub fn is_annotation(&self) -> bool {
        self.access_flags & ACC_ANNOTATION != 0
    }
}

pub fn parse(bytes: &[u8]) -> Result<ClassFile, ClassFileError> {
    let mut reader = Reader::new(bytes);

    if reader.read_u32()? != MAGIC {
        return Err(ClassFileError::InvalidMagic);
    }
    reader.skip(4)?; // minor_version, major_version

    let pool = read_constant_pool(&mut reader)?;

    let access_flags = reader.read_u16()?;
    let this_class = class_name_at(&pool, reader.read_u16()?)?.to_string();
    let super_class_index = reader.read_u16()?;
    let super_class = if super_class_index == 0 {
        None
    } else {
        Some(class_name_at(&pool, super_class_index)?.to_string())
    };

    let interfaces_count = reader.read_u16()?;
    let mut interfaces = Vec::with_capacity(interfaces_count as usize);
    for _ in 0..interfaces_count {
        let index = reader.read_u16()?;
        interfaces.push(class_name_at(&pool, index)?.to_string());
    }

    let fields_count = reader.read_u16()?;
    let mut fields = Vec::with_capacity(fields_count as usize);
    for _ in 0..fields_count {
        fields.push(read_member(&mut reader, &pool)?);
    }

    let methods_count = reader.read_u16()?;
    let mut methods = Vec::with_capacity(methods_count as usize);
    for _ in 0..methods_count {
        let member = read_member(&mut reader, &pool)?;
        if member.name != "<clinit>" {
            methods.push(member);
        }
    }

    skip_attributes(&mut reader)?;

    Ok(ClassFile {
        access_flags,
        this_class,
        super_class,
        interfaces,
        fields,
        methods,
    })
}

enum CpEntry {
    Utf8(String),
    ClassRef(u16),
    Other,
    Unusable,
}

fn read_constant_pool(reader: &mut Reader) -> Result<Vec<CpEntry>, ClassFileError> {
    let count = reader.read_u16()?;
    let mut pool = Vec::with_capacity(count as usize);
    pool.push(CpEntry::Unusable); // index 0 is never a valid constant

    let mut index = 1u16;
    while index < count {
        let tag = reader.read_u8()?;
        match tag {
            1 => {
                let length = reader.read_u16()? as usize;
                let bytes = reader.read_bytes(length)?;
                pool.push(CpEntry::Utf8(decode_modified_utf8(bytes)?));
            }
            3 | 4 => {
                reader.skip(4)?; // Integer, Float
                pool.push(CpEntry::Other);
            }
            5 | 6 => {
                reader.skip(8)?; // Long, Double: occupy two pool slots (JVMS 4.4.5)
                pool.push(CpEntry::Other);
                pool.push(CpEntry::Unusable);
                index += 1;
            }
            7 => {
                let name_index = reader.read_u16()?;
                pool.push(CpEntry::ClassRef(name_index));
            }
            8 | 16 | 19 | 20 => {
                reader.skip(2)?; // String, MethodType, Module, Package
                pool.push(CpEntry::Other);
            }
            9 | 10 | 11 | 12 | 17 | 18 => {
                reader.skip(4)?; // Fieldref, Methodref, InterfaceMethodref, NameAndType, Dynamic, InvokeDynamic
                pool.push(CpEntry::Other);
            }
            15 => {
                reader.skip(3)?; // MethodHandle
                pool.push(CpEntry::Other);
            }
            other => return Err(ClassFileError::InvalidConstantPoolTag(other)),
        }
        index += 1;
    }

    Ok(pool)
}

fn utf8_at(pool: &[CpEntry], index: u16) -> Result<&str, ClassFileError> {
    match pool.get(index as usize) {
        Some(CpEntry::Utf8(value)) => Ok(value.as_str()),
        _ => Err(ClassFileError::InvalidConstantPoolIndex),
    }
}

fn class_name_at(pool: &[CpEntry], index: u16) -> Result<&str, ClassFileError> {
    match pool.get(index as usize) {
        Some(CpEntry::ClassRef(name_index)) => utf8_at(pool, *name_index),
        _ => Err(ClassFileError::InvalidConstantPoolIndex),
    }
}

fn read_member(reader: &mut Reader, pool: &[CpEntry]) -> Result<Member, ClassFileError> {
    let access_flags = reader.read_u16()?;
    let name = utf8_at(pool, reader.read_u16()?)?.to_string();
    let descriptor = utf8_at(pool, reader.read_u16()?)?.to_string();
    skip_attributes(reader)?;
    Ok(Member {
        name,
        descriptor,
        access_flags,
    })
}

fn skip_attributes(reader: &mut Reader) -> Result<(), ClassFileError> {
    let count = reader.read_u16()?;
    for _ in 0..count {
        reader.read_u16()?; // attribute_name_index
        let length = reader.read_u32()? as usize;
        reader.skip(length)?;
    }
    Ok(())
}

/// Decodes the JVM's "modified UTF-8" ([JVMS §4.4.7]): identical to standard
/// UTF-8 except `NUL` is encoded as the two-byte sequence `0xC0 0x80` (never
/// as a literal `0x00`) and characters outside the Basic Multilingual Plane
/// are encoded as a surrogate pair, each half individually encoded as if it
/// were its own three-byte codepoint, rather than as a single four-byte
/// sequence.
///
/// [JVMS §4.4.7]: https://docs.oracle.com/javase/specs/jvms/se21/html/jvms-4.html#jvms-4.4.7
fn decode_modified_utf8(bytes: &[u8]) -> Result<String, ClassFileError> {
    let mut result = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        if b0 & 0x80 == 0 {
            result.push(b0 as char);
            i += 1;
        } else if b0 & 0xE0 == 0xC0 {
            let b1 = byte_at(bytes, i + 1)?;
            let value = (u32::from(b0 & 0x1F) << 6) | u32::from(b1 & 0x3F);
            result.push(char_from_u32(value)?);
            i += 2;
        } else if b0 & 0xF0 == 0xE0 {
            let b1 = byte_at(bytes, i + 1)?;
            let b2 = byte_at(bytes, i + 2)?;
            let value =
                (u32::from(b0 & 0x0F) << 12) | (u32::from(b1 & 0x3F) << 6) | u32::from(b2 & 0x3F);
            if (0xD800..=0xDBFF).contains(&value) {
                let b3 = byte_at(bytes, i + 3)?;
                let b4 = byte_at(bytes, i + 4)?;
                let b5 = byte_at(bytes, i + 5)?;
                if b3 & 0xF0 != 0xE0 {
                    return Err(ClassFileError::InvalidModifiedUtf8);
                }
                let low = (u32::from(b3 & 0x0F) << 12)
                    | (u32::from(b4 & 0x3F) << 6)
                    | u32::from(b5 & 0x3F);
                if !(0xDC00..=0xDFFF).contains(&low) {
                    return Err(ClassFileError::InvalidModifiedUtf8);
                }
                let codepoint = 0x10000 + ((value - 0xD800) << 10) + (low - 0xDC00);
                result.push(char_from_u32(codepoint)?);
                i += 6;
            } else {
                result.push(char_from_u32(value)?);
                i += 3;
            }
        } else {
            return Err(ClassFileError::InvalidModifiedUtf8);
        }
    }
    Ok(result)
}

fn byte_at(bytes: &[u8], index: usize) -> Result<u8, ClassFileError> {
    bytes
        .get(index)
        .copied()
        .ok_or(ClassFileError::InvalidModifiedUtf8)
}

fn char_from_u32(value: u32) -> Result<char, ClassFileError> {
    char::from_u32(value).ok_or(ClassFileError::InvalidModifiedUtf8)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], ClassFileError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(ClassFileError::UnexpectedEof)?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or(ClassFileError::UnexpectedEof)?;
        self.pos = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, ClassFileError> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16, ClassFileError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, ClassFileError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn skip(&mut self, len: usize) -> Result<(), ClassFileError> {
        self.read_bytes(len).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/classfiles")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|err| panic!("failed to read {path:?}: {err}"))
    }

    #[test]
    fn parses_this_class_and_super_class_from_a_real_classfile() {
        let class = parse(&fixture("Simple.class")).unwrap();

        assert_eq!(class.this_class, "Simple");
        assert_eq!(class.super_class, Some("java/lang/Object".to_string()));
    }

    #[test]
    fn parses_interfaces_from_a_real_classfile() {
        let class = parse(&fixture("Impl.class")).unwrap();

        assert_eq!(class.interfaces, vec!["Greetable".to_string()]);
    }

    #[test]
    fn parses_an_interface_declaration_with_access_flags() {
        let class = parse(&fixture("Greetable.class")).unwrap();

        assert!(class.is_interface());
        assert_eq!(class.access_flags & ACC_ABSTRACT, ACC_ABSTRACT);
        assert_eq!(class.methods.len(), 1);
        assert_eq!(class.methods[0].name, "greet");
        assert_eq!(class.methods[0].descriptor, "()Ljava/lang/String;");
    }

    #[test]
    fn parses_fields_with_descriptor_and_access_flags() {
        let class = parse(&fixture("Simple.class")).unwrap();

        assert_eq!(class.fields.len(), 2);
        let count = class.fields.iter().find(|f| f.name == "count").unwrap();
        assert_eq!(count.descriptor, "I");
        assert_eq!(count.access_flags & ACC_PUBLIC, ACC_PUBLIC);

        let name = class.fields.iter().find(|f| f.name == "name").unwrap();
        assert_eq!(name.descriptor, "Ljava/lang/String;");
        assert_eq!(name.access_flags & ACC_PUBLIC, 0);
    }

    #[test]
    fn parses_methods_including_the_constructor() {
        let class = parse(&fixture("Simple.class")).unwrap();

        let names: Vec<&str> = class.methods.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["<init>", "getCount", "setName"]);
    }

    #[test]
    fn excludes_the_static_initializer_from_methods() {
        let class = parse(&fixture("WithStatic.class")).unwrap();

        let names: Vec<&str> = class.methods.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["<init>"]);
        assert!(!names.contains(&"<clinit>"));
    }

    #[test]
    fn parse_returns_err_on_invalid_magic() {
        let result = parse(&[0, 0, 0, 0, 0, 0, 0, 0]);

        assert_eq!(result, Err(ClassFileError::InvalidMagic));
    }

    #[test]
    fn parse_returns_err_instead_of_panicking_on_truncated_input() {
        let full = fixture("Simple.class");

        for len in [0, 1, 4, 8, 10, 20, full.len() / 2] {
            let result = parse(&full[..len]);
            assert!(result.is_err(), "expected an error for truncated len {len}");
        }
    }

    #[test]
    fn decode_modified_utf8_reads_plain_ascii() {
        assert_eq!(decode_modified_utf8(b"hello").unwrap(), "hello");
    }

    #[test]
    fn decode_modified_utf8_reads_nul_encoded_as_two_bytes() {
        let decoded = decode_modified_utf8(&[0xC0, 0x80]).unwrap();

        assert_eq!(decoded, "\0");
    }

    #[test]
    fn decode_modified_utf8_reads_a_supplementary_character_as_a_surrogate_pair() {
        // U+10000, encoded as a high/low surrogate pair per JVMS 4.4.7,
        // each half individually encoded as a three-byte sequence.
        let bytes = [0xED, 0xA0, 0x80, 0xED, 0xB0, 0x80];

        let decoded = decode_modified_utf8(&bytes).unwrap();

        assert_eq!(decoded.chars().collect::<Vec<_>>(), vec!['\u{10000}']);
    }

    #[test]
    fn decode_modified_utf8_returns_err_for_an_invalid_leading_byte() {
        assert_eq!(
            decode_modified_utf8(&[0xFF]),
            Err(ClassFileError::InvalidModifiedUtf8)
        );
    }

    #[test]
    fn decode_modified_utf8_returns_err_for_a_truncated_multibyte_sequence() {
        assert_eq!(
            decode_modified_utf8(&[0xC0]),
            Err(ClassFileError::InvalidModifiedUtf8)
        );
    }

    /// A hand-built minimal classfile (no fields, two trivial methods, and an
    /// oversized bogus attribute between the class-level attribute count and
    /// EOF) — proves attribute skipping honors `attribute_length` generically,
    /// rather than only ever seeing the specific attributes `javac` happens
    /// to emit in the fixture files above.
    #[test]
    fn parse_skips_an_attribute_of_arbitrary_length() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC.to_be_bytes());
        bytes.extend_from_slice(&[0, 0]); // minor_version
        bytes.extend_from_slice(&[0, 65]); // major_version

        // Constant pool: #1 Utf8 "Main", #2 Class -> #1, #3 Utf8 "java/lang/Object", #4 Class -> #3
        bytes.extend_from_slice(&[0, 5]); // constant_pool_count = 5 (4 real entries)
        push_utf8(&mut bytes, "Main");
        push_class(&mut bytes, 1);
        push_utf8(&mut bytes, "java/lang/Object");
        push_class(&mut bytes, 3);

        bytes.extend_from_slice(&[0, 0x21]); // access_flags: ACC_PUBLIC | ACC_SUPER
        bytes.extend_from_slice(&[0, 2]); // this_class -> #2 (Main)
        bytes.extend_from_slice(&[0, 4]); // super_class -> #4 (java/lang/Object)
        bytes.extend_from_slice(&[0, 0]); // interfaces_count
        bytes.extend_from_slice(&[0, 0]); // fields_count
        bytes.extend_from_slice(&[0, 0]); // methods_count

        bytes.extend_from_slice(&[0, 1]); // attributes_count
        bytes.extend_from_slice(&[0, 1]); // attribute_name_index (doesn't need to resolve to anything real)
        let garbage = vec![0xAAu8; 200];
        bytes.extend_from_slice(&(garbage.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&garbage);

        let class = parse(&bytes).unwrap();

        assert_eq!(class.this_class, "Main");
        assert_eq!(class.super_class, Some("java/lang/Object".to_string()));
    }

    fn push_utf8(bytes: &mut Vec<u8>, value: &str) {
        bytes.push(1); // tag: Utf8
        bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        bytes.extend_from_slice(value.as_bytes());
    }

    fn push_class(bytes: &mut Vec<u8>, name_index: u16) {
        bytes.push(7); // tag: Class
        bytes.extend_from_slice(&name_index.to_be_bytes());
    }
}
