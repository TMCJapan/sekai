//! Hand-rolled NBT parser: raw NBT bytes in, owned value tree out.
//!
//! NBT is a small tag/length/value format, so a dependency-free
//! recursive-descent parser keeps this crate `no_std` with a minimal
//! dependency surface. Parsing is strict:
//! truncated, overlong, mistyped, or trailing input is a loud error, never
//! a silent partial value. Untrusted chunk payloads also bound two
//! resources: nesting depth (stack) and allocation (every allocation is
//! backed by bytes actually consumed from the input, so a corrupt length
//! prefix fails with `UnexpectedEnd` before it can over-allocate).

use alloc::{string::String, vec::Vec};

use crate::error::NbtError;

/// Owned NBT value tree.
///
/// No `Serialize` impl by design: the derived form would be externally
/// tagged, competing with the SNBT `Display` that machine-readable output
/// actually uses (CLI serializes diff values as SNBT strings).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(Vec<Self>),
    Compound(Vec<(String, Self)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Value {
    pub const fn tag_id(&self) -> u8 {
        match self {
            Self::Byte(_) => 1,
            Self::Short(_) => 2,
            Self::Int(_) => 3,
            Self::Long(_) => 4,
            Self::Float(_) => 5,
            Self::Double(_) => 6,
            Self::ByteArray(_) => 7,
            Self::String(_) => 8,
            Self::List(_) => 9,
            Self::Compound(_) => 10,
            Self::IntArray(_) => 11,
            Self::LongArray(_) => 12,
        }
    }
}

const MAX_DEPTH: usize = 64;

pub(crate) fn parse_root(bytes: &[u8]) -> Result<Value, NbtError> {
    if bytes.is_empty() {
        return Err(NbtError::EmptyInput);
    }

    let mut cursor = Cursor::new(bytes);

    let tag = cursor.read_u8()?;
    if tag != 10 {
        return Err(NbtError::UnexpectedRoot(tag));
    }

    cursor.read_string()?;

    let value = Value::Compound(parse_compound_body(&mut cursor, 0)?);

    let trailing = cursor.remaining();
    if trailing != 0 {
        return Err(NbtError::TrailingData(trailing));
    }

    Ok(value)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    const fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn read_u8(&mut self) -> Result<u8, NbtError> {
        let value = *self.bytes.get(self.pos).ok_or(NbtError::UnexpectedEnd)?;

        self.pos += 1;
        Ok(value)
    }

    fn read_i8(&mut self) -> Result<i8, NbtError> {
        Ok(self.read_u8()?.cast_signed())
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], NbtError> {
        let end = self.pos.checked_add(len).ok_or(NbtError::UnexpectedEnd)?;
        let bytes = self
            .bytes
            .get(self.pos..end)
            .ok_or(NbtError::UnexpectedEnd)?;

        self.pos = end;
        Ok(bytes)
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], NbtError> {
        self.read_bytes(N)?
            .try_into()
            .map_err(|_| NbtError::UnexpectedEnd)
    }

    fn read_u16(&mut self) -> Result<u16, NbtError> {
        Ok(u16::from_be_bytes(self.read_array()?))
    }

    fn read_i16(&mut self) -> Result<i16, NbtError> {
        Ok(i16::from_be_bytes(self.read_array()?))
    }

    fn read_i32(&mut self) -> Result<i32, NbtError> {
        Ok(i32::from_be_bytes(self.read_array()?))
    }

    fn read_i64(&mut self) -> Result<i64, NbtError> {
        Ok(i64::from_be_bytes(self.read_array()?))
    }

    fn read_f32(&mut self) -> Result<f32, NbtError> {
        Ok(f32::from_bits(u32::from_be_bytes(self.read_array()?)))
    }

    fn read_f64(&mut self) -> Result<f64, NbtError> {
        Ok(f64::from_bits(u64::from_be_bytes(self.read_array()?)))
    }

    fn read_string(&mut self) -> Result<String, NbtError> {
        let len = usize::from(self.read_u16()?);
        decode_mutf8(self.read_bytes(len)?)
    }
}

const fn check_depth(depth: usize) -> Result<(), NbtError> {
    if depth >= MAX_DEPTH {
        Err(NbtError::TooDeeplyNested)
    } else {
        Ok(())
    }
}

fn decode_mutf8(bytes: &[u8]) -> Result<String, NbtError> {
    let mut output = String::with_capacity(bytes.len());
    let mut pos = 0;

    while pos < bytes.len() {
        let lead = bytes[pos];

        match lead {
            0x01..=0x7F => {
                output.push(char::from(lead));
                pos += 1;
            }

            0xC0..=0xDF => {
                let cont = *bytes.get(pos + 1).ok_or(NbtError::InvalidString)?;

                if !is_continuation(cont) {
                    return Err(NbtError::InvalidString);
                }

                if lead == 0xC0 && cont == 0x80 {
                    output.push('\0');
                } else {
                    let value = (u32::from(lead & 0x1F) << 6) | u32::from(cont & 0x3F);

                    if value < 0x80 {
                        return Err(NbtError::InvalidString);
                    }

                    output.push(char::from_u32(value).ok_or(NbtError::InvalidString)?);
                }

                pos += 2;
            }

            0xE0..=0xEF => {
                let cont0 = *bytes.get(pos + 1).ok_or(NbtError::InvalidString)?;
                let cont1 = *bytes.get(pos + 2).ok_or(NbtError::InvalidString)?;

                if !is_continuation(cont0) || !is_continuation(cont1) {
                    return Err(NbtError::InvalidString);
                }

                let value = (u32::from(lead & 0x0F) << 12)
                    | (u32::from(cont0 & 0x3F) << 6)
                    | u32::from(cont1 & 0x3F);

                if value < 0x800 {
                    return Err(NbtError::InvalidString);
                }

                if (0xD800..0xDC00).contains(&value) {
                    let low = decode_surrogate_tail(bytes, pos + 3)?;
                    let scalar = 0x1_0000 + ((value - 0xD800) << 10) + (low - 0xDC00);

                    output.push(char::from_u32(scalar).ok_or(NbtError::InvalidString)?);

                    pos += 6;
                } else if (0xDC00..0xE000).contains(&value) {
                    return Err(NbtError::InvalidString);
                } else {
                    output.push(char::from_u32(value).ok_or(NbtError::InvalidString)?);
                    pos += 3;
                }
            }

            _ => return Err(NbtError::InvalidString),
        }
    }

    Ok(output)
}

const fn is_continuation(byte: u8) -> bool {
    byte & 0xC0 == 0x80
}

fn decode_surrogate_tail(bytes: &[u8], pos: usize) -> Result<u32, NbtError> {
    let [lead, cont0, cont1] = bytes
        .get(pos..pos + 3)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(NbtError::InvalidString)?;

    if lead != 0xED || !is_continuation(cont0) || !is_continuation(cont1) {
        return Err(NbtError::InvalidString);
    }

    let value =
        (u32::from(lead & 0x0F) << 12) | (u32::from(cont0 & 0x3F) << 6) | u32::from(cont1 & 0x3F);

    if !(0xDC00..0xE000).contains(&value) {
        return Err(NbtError::InvalidString);
    }

    Ok(value)
}

fn parse_payload(cursor: &mut Cursor<'_>, tag: u8, depth: usize) -> Result<Value, NbtError> {
    match tag {
        1 => Ok(Value::Byte(cursor.read_i8()?)),
        2 => Ok(Value::Short(cursor.read_i16()?)),
        3 => Ok(Value::Int(cursor.read_i32()?)),
        4 => Ok(Value::Long(cursor.read_i64()?)),
        5 => Ok(Value::Float(cursor.read_f32()?)),
        6 => Ok(Value::Double(cursor.read_f64()?)),

        7 => {
            let len = read_len(cursor)?;
            let raw = cursor.read_bytes(len)?;

            Ok(Value::ByteArray(
                raw.iter().map(|&byte| byte.cast_signed()).collect(),
            ))
        }

        8 => Ok(Value::String(cursor.read_string()?)),
        9 => parse_list(cursor, depth),
        10 => Ok(Value::Compound(parse_compound_body(cursor, depth)?)),
        11 => Ok(Value::IntArray(read_i32_array(cursor)?)),
        12 => Ok(Value::LongArray(read_i64_array(cursor)?)),

        other => Err(NbtError::UnknownTag(other)),
    }
}

fn read_len(cursor: &mut Cursor<'_>) -> Result<usize, NbtError> {
    let len = cursor.read_i32()?;

    if len < 0 {
        return Err(NbtError::InvalidLength(len));
    }

    usize::try_from(len).map_err(|_| NbtError::InvalidLength(len))
}

fn read_i32_array(cursor: &mut Cursor<'_>) -> Result<Vec<i32>, NbtError> {
    let len = read_len(cursor)?;
    let mut values = Vec::with_capacity(len);

    for _ in 0..len {
        values.push(cursor.read_i32()?);
    }

    Ok(values)
}

fn read_i64_array(cursor: &mut Cursor<'_>) -> Result<Vec<i64>, NbtError> {
    let len = read_len(cursor)?;
    let mut values = Vec::with_capacity(len);

    for _ in 0..len {
        values.push(cursor.read_i64()?);
    }

    Ok(values)
}

fn parse_compound_body(
    cursor: &mut Cursor<'_>,
    depth: usize,
) -> Result<Vec<(String, Value)>, NbtError> {
    check_depth(depth)?;

    let mut entries = Vec::new();

    loop {
        let tag = cursor.read_u8()?;

        if tag == 0 {
            break;
        }

        if tag > 12 {
            return Err(NbtError::UnknownTag(tag));
        }

        let name = cursor.read_string()?;
        let value = parse_payload(cursor, tag, depth + 1)?;

        entries.push((name, value));
    }

    // Sorting makes manually constructed Values and parsed Values use the same
    // canonical compound ordering. Keep the first occurrence of duplicate keys.
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.dedup_by(|a, b| a.0 == b.0);

    Ok(entries)
}

fn parse_list(cursor: &mut Cursor<'_>, depth: usize) -> Result<Value, NbtError> {
    check_depth(depth)?;

    let element_tag = cursor.read_u8()?;

    if element_tag > 12 {
        return Err(NbtError::UnknownTag(element_tag));
    }

    let raw_len = cursor.read_i32()?;

    if raw_len < 0 || (element_tag == 0 && raw_len != 0) {
        return Err(NbtError::InvalidLength(raw_len));
    }

    let len = usize::try_from(raw_len).map_err(|_| NbtError::InvalidLength(raw_len))?;

    let mut values = Vec::with_capacity(len);

    for _ in 0..len {
        values.push(parse_payload(cursor, element_tag, depth + 1)?);
    }

    Ok(Value::List(values))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;
    use alloc::vec;

    struct Writer {
        bytes: Vec<u8>,
    }

    impl Writer {
        fn new() -> Self {
            Self { bytes: Vec::new() }
        }

        fn tag(&mut self, id: u8) {
            self.bytes.push(id);
        }

        fn name(&mut self, name: &str) {
            self.u16_bytes(name.len());
            self.bytes.extend_from_slice(name.as_bytes());
        }

        fn u16_bytes(&mut self, len: usize) {
            let len = u16::try_from(len).unwrap_or(u16::MAX);
            self.bytes.extend_from_slice(&len.to_be_bytes());
        }

        fn i32_bytes(&mut self, value: i32) {
            self.bytes.extend_from_slice(&value.to_be_bytes());
        }

        fn named(&mut self, id: u8, name: &str) {
            self.tag(id);
            self.name(name);
        }

        fn root(&mut self) {
            self.tag(10);
            self.name("");
        }

        fn end(&mut self) {
            self.tag(0);
        }

        fn int(&mut self, name: &str, value: i32) {
            self.named(3, name);
            self.i32_bytes(value);
        }

        fn string(&mut self, value: &[u8]) {
            self.named(8, "k");
            self.u16_bytes(value.len());
            self.bytes.extend_from_slice(value);
        }

        fn compound(&mut self, name: &str, body: &[u8]) {
            self.named(10, name);
            self.bytes.extend_from_slice(body);
            self.end();
        }
    }

    fn chunk_nbt(last_update: i64, inhabited: i64, status: &str) -> Vec<u8> {
        let mut writer = Writer::new();

        writer.root();

        writer.named(8, "Status");
        writer.u16_bytes(status.len());
        writer.bytes.extend_from_slice(status.as_bytes());

        writer.named(4, "LastUpdate");
        writer.bytes.extend_from_slice(&last_update.to_be_bytes());

        writer.named(4, "InhabitedTime");
        writer.bytes.extend_from_slice(&inhabited.to_be_bytes());

        writer.named(1, "xPos");
        writer.bytes.push(3);

        let mut inner = Writer::new();
        inner.named(1, "Y");
        inner.bytes.push(0);

        writer.compound("section", &inner.bytes);

        writer.named(9, "entities");
        writer.bytes.push(10);
        writer.i32_bytes(0);
        writer.end();

        writer.bytes
    }

    #[test]
    fn parses_chunk_shape_with_sorted_compound() {
        let value = parse_root(&chunk_nbt(100, 42, "minecraft:full")).unwrap();

        let Value::Compound(entries) = value else {
            panic!("root must be a compound");
        };

        let keys: Vec<&str> = entries.iter().map(|(key, _)| key.as_str()).collect();

        assert_eq!(
            keys,
            vec![
                "InhabitedTime",
                "LastUpdate",
                "Status",
                "entities",
                "section",
                "xPos",
            ]
        );

        assert!(matches!(
            entries.iter().find(|(key, _)| key == "xPos"),
            Some((_, Value::Byte(3)))
        ));
    }

    #[test]
    fn rejects_malformed_inputs() {
        assert_eq!(parse_root(&[]), Err(NbtError::EmptyInput));
        assert_eq!(parse_root(&[10, 0, 0, 8]), Err(NbtError::UnexpectedEnd));
        assert_eq!(parse_root(&[10, 0, 0, 13]), Err(NbtError::UnknownTag(13)));
        assert_eq!(parse_root(&[1, 0, 0, 7]), Err(NbtError::UnexpectedRoot(1)));

        let mut bytes = chunk_nbt(1, 1, "s");
        bytes.push(0);

        assert_eq!(parse_root(&bytes), Err(NbtError::TrailingData(1)));

        let bytes = vec![10, 0, 0, 11, 0, 1, b'a', 0xFF, 0xFF, 0xFF, 0xFF, 0];

        assert_eq!(parse_root(&bytes), Err(NbtError::InvalidLength(-1)));

        let bytes = vec![10, 0, 0, 9, 0, 1, b'l', 0, 0, 0, 0, 1, 0];

        assert_eq!(parse_root(&bytes), Err(NbtError::InvalidLength(1)));
    }

    #[test]
    fn decodes_modified_utf8() {
        let mut writer = Writer::new();

        writer.root();

        writer.named(8, "nul");
        writer.u16_bytes(2);
        writer.bytes.extend_from_slice(&[0xC0, 0x80]);

        writer.named(8, "e");
        writer.u16_bytes(2);
        writer.bytes.extend_from_slice(&[0xC3, 0xA9]);

        writer.named(8, "emoji");
        writer.u16_bytes(6);
        writer
            .bytes
            .extend_from_slice(&[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]);

        writer.end();

        let Value::Compound(entries) = parse_root(&writer.bytes).unwrap() else {
            panic!("root must be a compound");
        };

        let get = |key: &str| {
            entries
                .iter()
                .find(|(entry_key, _)| entry_key == key)
                .map(|(_, value)| value.clone())
        };

        assert_eq!(get("nul"), Some(Value::String("\0".to_owned())));
        assert_eq!(get("e"), Some(Value::String("é".to_owned())));
        assert_eq!(get("emoji"), Some(Value::String("😀".to_owned())));
    }

    #[test]
    fn rejects_invalid_strings() {
        for payload in [
            vec![0x00],
            vec![0xC0, 0xAF],
            vec![0xED, 0xA0, 0x80],
            vec![0xF0, 0x9F, 0x98, 0x80],
            vec![0x80],
        ] {
            let mut writer = Writer::new();

            writer.root();
            writer.string(&payload);
            writer.end();

            assert_eq!(parse_root(&writer.bytes), Err(NbtError::InvalidString));
        }
    }

    #[test]
    fn bounds_nesting_depth() {
        fn nested(depth: usize) -> Vec<u8> {
            let mut writer = Writer::new();
            writer.root();

            for i in 0..depth {
                writer.named(10, alloc::format!("c{i}").as_str());
            }

            for _ in 0..=depth {
                writer.end();
            }

            writer.bytes
        }

        assert_eq!(parse_root(&nested(70)), Err(NbtError::TooDeeplyNested));
        assert!(parse_root(&nested(8)).is_ok());
    }

    #[test]
    fn first_duplicate_key_wins() {
        let mut writer = Writer::new();

        writer.root();
        writer.int("a", 1);
        writer.int("a", 2);
        writer.end();

        let Value::Compound(entries) = parse_root(&writer.bytes).unwrap() else {
            panic!("root must be a compound");
        };

        assert_eq!(entries, vec![("a".to_owned(), Value::Int(1))]);
    }
}
