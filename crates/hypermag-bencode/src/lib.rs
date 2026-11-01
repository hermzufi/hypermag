//! Zero-copy bencode decoding and canonical encoding.
//!
//! Bencode is the serialization format used by torrent metadata and tracker
//! responses. The decoder borrows byte strings directly from the input, so
//! parsing a `.torrent` file does not copy file names, piece hashes or tracker
//! URLs.
//!
//! The decoder is strict about the parts of the grammar that matter for
//! security and canonicalization:
//!
//! - leading zeros in integers and string lengths are rejected,
//! - `-0` is rejected,
//! - duplicate dictionary keys are rejected,
//! - nesting depth is bounded,
//! - trailing bytes are rejected by [`decode`].
//!
//! Dictionary key *ordering* is not enforced on decode (some torrents in the
//! wild are not canonical), but the encoder always writes keys in sorted order.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// Maximum nesting depth accepted by [`Decoder::new`].
pub const DEFAULT_MAX_DEPTH: usize = 128;

/// A borrowed bencode value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<Value<'a>>),
    Dict(BTreeMap<&'a [u8], Value<'a>>),
}

impl<'a> Value<'a> {
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match self {
            Self::Bytes(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&'a str> {
        self.as_bytes()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
    }

    #[must_use]
    pub fn as_list(&self) -> Option<&[Value<'a>]> {
        match self {
            Self::List(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_dict(&self) -> Option<&BTreeMap<&'a [u8], Value<'a>>> {
        match self {
            Self::Dict(value) => Some(value),
            _ => None,
        }
    }

    /// Look up `key` if this value is a dictionary.
    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<&Value<'a>> {
        self.as_dict()?.get(key)
    }
}

/// A dictionary entry with the byte range its value occupied in the input.
///
/// The span is what makes it possible to SHA-1 the *raw* `info` dictionary for
/// the v1 info-hash, rather than re-encoding it and hoping it matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictEntry<'a> {
    pub key: &'a [u8],
    pub value: Value<'a>,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("unexpected end of input at byte {offset}")]
    UnexpectedEof { offset: usize },
    #[error("expected byte {expected:?} at byte {offset}, found {found:?}")]
    ExpectedByte {
        offset: usize,
        expected: u8,
        found: Option<u8>,
    },
    #[error("invalid value type at byte {offset}: found byte {found:?}")]
    InvalidType { offset: usize, found: Option<u8> },
    #[error("invalid integer at byte {offset}: {reason}")]
    InvalidInteger { offset: usize, reason: &'static str },
    #[error("integer overflow at byte {offset}")]
    IntegerOverflow { offset: usize },
    #[error("invalid string length at byte {offset}")]
    InvalidStringLength { offset: usize },
    #[error("string of length {len} at byte {offset} exceeds remaining input ({remaining} bytes)")]
    StringOutOfBounds {
        offset: usize,
        len: usize,
        remaining: usize,
    },
    #[error("dictionary key at byte {offset} is not a byte string")]
    NonStringKey { offset: usize },
    #[error("duplicate dictionary key at byte {offset}")]
    DuplicateKey { offset: usize },
    #[error("nesting depth limit {limit} exceeded at byte {offset}")]
    DepthLimitExceeded { offset: usize, limit: usize },
    #[error("trailing data at byte {offset}")]
    TrailingData { offset: usize },
}

/// A strict, borrowing bencode decoder.
#[derive(Debug, Clone)]
pub struct Decoder<'a> {
    input: &'a [u8],
    pos: usize,
    max_depth: usize,
}

impl<'a> Decoder<'a> {
    #[must_use]
    pub fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }

    #[must_use]
    pub fn with_max_depth(input: &'a [u8], max_depth: usize) -> Self {
        Self {
            input,
            pos: 0,
            max_depth,
        }
    }

    #[must_use]
    pub fn position(&self) -> usize {
        self.pos
    }

    #[must_use]
    pub fn is_eof(&self) -> bool {
        self.pos == self.input.len()
    }

    /// Error unless every input byte has been consumed.
    pub fn finish(&self) -> Result<(), Error> {
        if self.is_eof() {
            Ok(())
        } else {
            Err(Error::TrailingData { offset: self.pos })
        }
    }

    /// Parse one value, leaving trailing bytes unconsumed.
    pub fn parse(&mut self) -> Result<Value<'a>, Error> {
        self.parse_depth(0)
    }

    /// Parse exactly one value and reject trailing bytes.
    pub fn parse_complete(mut self) -> Result<Value<'a>, Error> {
        let value = self.parse_depth(0)?;
        self.finish()?;
        Ok(value)
    }

    /// Parse a top-level dictionary and report the input span of each value.
    ///
    /// Only the outer dictionary is spanned. Nested values are parsed normally;
    /// the span of a nested entry covers that entry's entire value.
    pub fn parse_dict_with_spans(&mut self) -> Result<Vec<DictEntry<'a>>, Error> {
        self.parse_dict_entries(0)
    }

    /// Like [`Self::parse_dict_with_spans`], but reject trailing bytes.
    pub fn parse_dict_with_spans_complete(mut self) -> Result<Vec<DictEntry<'a>>, Error> {
        let entries = self.parse_dict_entries(0)?;
        self.finish()?;
        Ok(entries)
    }

    fn parse_dict_entries(&mut self, depth: usize) -> Result<Vec<DictEntry<'a>>, Error> {
        if depth > self.max_depth {
            return Err(Error::DepthLimitExceeded {
                offset: self.pos,
                limit: self.max_depth,
            });
        }

        self.expect(b'd')?;

        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();

        loop {
            match self.peek() {
                Some(b'e') => {
                    self.pos += 1;
                    break;
                }
                None => return Err(Error::UnexpectedEof { offset: self.pos }),
                Some(_) => {}
            }

            let key_offset = self.pos;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(Error::NonStringKey { offset: key_offset });
            }
            let key = self.parse_bytes()?;
            if !seen.insert(key) {
                return Err(Error::DuplicateKey { offset: key_offset });
            }

            let value_start = self.pos;
            let value = self.parse_depth(depth + 1)?;
            entries.push(DictEntry {
                key,
                value,
                span: value_start..self.pos,
            });
        }

        Ok(entries)
    }

    fn parse_depth(&mut self, depth: usize) -> Result<Value<'a>, Error> {
        if depth > self.max_depth {
            return Err(Error::DepthLimitExceeded {
                offset: self.pos,
                limit: self.max_depth,
            });
        }

        let offset = self.pos;
        match self.next_byte()? {
            b'i' => self.parse_integer(),
            b'l' => self.parse_list(depth + 1),
            b'd' => self.parse_dict(depth + 1),
            b'0'..=b'9' => {
                self.pos -= 1;
                Ok(Value::Bytes(self.parse_bytes()?))
            }
            found => Err(Error::InvalidType {
                offset,
                found: Some(found),
            }),
        }
    }

    fn parse_integer(&mut self) -> Result<Value<'a>, Error> {
        let start = self.pos;
        let mut digits_seen = 0usize;
        let end = loop {
            let byte = self.next_byte()?;
            if byte == b'e' {
                break self.pos - 1;
            }
            let position = digits_seen;
            digits_seen += 1;
            let allowed = byte.is_ascii_digit() || (byte == b'-' && position == 0);
            if !allowed {
                return Err(Error::InvalidInteger {
                    offset: start,
                    reason: "unexpected character",
                });
            }
        };

        let raw = &self.input[start..end];
        let (negative, digits) = match raw.split_first() {
            Some((b'-', rest)) => (true, rest),
            _ => (false, raw),
        };

        if digits.is_empty() {
            return Err(Error::InvalidInteger {
                offset: start,
                reason: "empty integer",
            });
        }
        if digits.len() > 1 && digits[0] == b'0' {
            return Err(Error::InvalidInteger {
                offset: start,
                reason: "leading zero",
            });
        }
        if negative && digits == b"0" {
            return Err(Error::InvalidInteger {
                offset: start,
                reason: "negative zero",
            });
        }

        // `raw` is ASCII by construction.
        let text = std::str::from_utf8(raw).map_err(|_| Error::InvalidInteger {
            offset: start,
            reason: "non-ASCII integer",
        })?;
        let value: i64 = text
            .parse()
            .map_err(|_| Error::IntegerOverflow { offset: start })?;
        Ok(Value::Int(value))
    }

    fn parse_bytes(&mut self) -> Result<&'a [u8], Error> {
        let start = self.pos;
        let mut length = 0usize;
        let mut digits = 0usize;

        loop {
            let byte = self.next_byte()?;
            if byte == b':' {
                break;
            }
            if !byte.is_ascii_digit() {
                return Err(Error::InvalidStringLength { offset: start });
            }
            if digits == 0 && byte == b'0' && self.peek() != Some(b':') {
                return Err(Error::InvalidStringLength { offset: start });
            }
            length = length
                .checked_mul(10)
                .and_then(|value| value.checked_add(usize::from(byte - b'0')))
                .ok_or(Error::InvalidStringLength { offset: start })?;
            // Reject before it can overflow the remaining-input subtraction.
            if length > self.input.len() {
                return Err(Error::InvalidStringLength { offset: start });
            }
            digits += 1;
        }

        if digits == 0 {
            return Err(Error::InvalidStringLength { offset: start });
        }

        let remaining = self.input.len() - self.pos;
        if length > remaining {
            return Err(Error::StringOutOfBounds {
                offset: start,
                len: length,
                remaining,
            });
        }

        let bytes = &self.input[self.pos..self.pos + length];
        self.pos += length;
        Ok(bytes)
    }

    fn parse_list(&mut self, depth: usize) -> Result<Value<'a>, Error> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                Some(b'e') => {
                    self.pos += 1;
                    break;
                }
                None => return Err(Error::UnexpectedEof { offset: self.pos }),
                Some(_) => items.push(self.parse_depth(depth)?),
            }
        }
        Ok(Value::List(items))
    }

    fn parse_dict(&mut self, depth: usize) -> Result<Value<'a>, Error> {
        let mut entries = BTreeMap::new();
        loop {
            match self.peek() {
                Some(b'e') => {
                    self.pos += 1;
                    break;
                }
                None => return Err(Error::UnexpectedEof { offset: self.pos }),
                Some(_) => {}
            }

            let key_offset = self.pos;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(Error::NonStringKey { offset: key_offset });
            }
            let key = self.parse_bytes()?;
            let value = self.parse_depth(depth)?;
            if entries.insert(key, value).is_some() {
                return Err(Error::DuplicateKey { offset: key_offset });
            }
        }
        Ok(Value::Dict(entries))
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn next_byte(&mut self) -> Result<u8, Error> {
        let byte = self
            .input
            .get(self.pos)
            .copied()
            .ok_or(Error::UnexpectedEof { offset: self.pos })?;
        self.pos += 1;
        Ok(byte)
    }

    fn expect(&mut self, expected: u8) -> Result<(), Error> {
        let offset = self.pos;
        match self.next_byte()? {
            byte if byte == expected => Ok(()),
            found => Err(Error::ExpectedByte {
                offset,
                expected,
                found: Some(found),
            }),
        }
    }
}

/// Decode exactly one bencode value and reject trailing bytes.
pub fn decode(input: &[u8]) -> Result<Value<'_>, Error> {
    Decoder::new(input).parse_complete()
}

/// Encode a value into a new buffer.
#[must_use]
pub fn encode(value: &Value<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, &mut out);
    out
}

/// Encode a value by appending to `out`.
///
/// Dictionary keys are always written in sorted order, which is canonical
/// bencode.
pub fn encode_into(value: &Value<'_>, out: &mut Vec<u8>) {
    match value {
        Value::Int(number) => {
            out.push(b'i');
            out.extend_from_slice(number.to_string().as_bytes());
            out.push(b'e');
        }
        Value::Bytes(bytes) => encode_bytes(bytes, out),
        Value::List(items) => {
            out.push(b'l');
            for item in items {
                encode_into(item, out);
            }
            out.push(b'e');
        }
        Value::Dict(entries) => {
            out.push(b'd');
            for (key, entry) in entries {
                encode_bytes(key, out);
                encode_into(entry, out);
            }
            out.push(b'e');
        }
    }
}

fn encode_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_integer() {
        assert_eq!(decode(b"i42e"), Ok(Value::Int(42)));
        assert_eq!(decode(b"i-42e"), Ok(Value::Int(-42)));
        assert_eq!(decode(b"i0e"), Ok(Value::Int(0)));
    }

    #[test]
    fn rejects_bad_integers() {
        for input in [
            b"ie".as_slice(),
            b"i-e",
            b"i-0e",
            b"i03e",
            b"i1-2e",
            b"i9223372036854775808e",
        ] {
            assert!(decode(input).is_err(), "accepted {input:?}");
        }
        assert_eq!(decode(b"i1"), Err(Error::UnexpectedEof { offset: 2 }));
    }

    #[test]
    fn decodes_bytes() {
        assert_eq!(decode(b"4:spam"), Ok(Value::Bytes(b"spam")));
        assert_eq!(decode(b"0:"), Ok(Value::Bytes(b"")));
    }

    #[test]
    fn rejects_bad_bytes() {
        for input in [b"01:a".as_slice(), b"4:abc", b":x", b"-1:x", b"5"] {
            assert!(decode(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn decodes_nested_structures() {
        let value = decode(b"d3:bar4:spam3:fooi42e4:listl1:a1:bee").unwrap();
        assert_eq!(
            value.get(b"bar").unwrap().as_bytes(),
            Some(b"spam".as_slice())
        );
        assert_eq!(value.get(b"foo").unwrap().as_int(), Some(42));
        assert_eq!(
            value.get(b"list").unwrap().as_list().unwrap(),
            [Value::Bytes(b"a"), Value::Bytes(b"b")]
        );
    }

    #[test]
    fn rejects_duplicate_keys() {
        assert_eq!(
            decode(b"d1:ai1e1:ai2ee"),
            Err(Error::DuplicateKey { offset: 7 })
        );
    }

    #[test]
    fn rejects_trailing_data() {
        assert_eq!(decode(b"i1ei2e"), Err(Error::TrailingData { offset: 3 }));
    }

    #[test]
    fn enforces_depth_limit() {
        let deep = || {
            let mut input = b"l".repeat(6);
            input.extend_from_slice(&b"e".repeat(6));
            input
        };
        assert!(
            Decoder::with_max_depth(&deep(), 4)
                .parse_complete()
                .is_err()
        );
        assert!(Decoder::with_max_depth(&deep(), 5).parse_complete().is_ok());
    }

    #[test]
    fn encodes_canonically() {
        let mut dict = BTreeMap::new();
        dict.insert(b"b".as_slice(), Value::Int(2));
        dict.insert(b"a".as_slice(), Value::Bytes(b"x"));
        let value = Value::Dict(dict);
        assert_eq!(encode(&value), b"d1:a1:x1:bi2ee");
        assert_eq!(decode(&encode(&value)), Ok(value));
    }

    #[test]
    fn round_trips_lists() {
        let value = Value::List(vec![
            Value::Int(1),
            Value::List(vec![Value::Bytes(b"nested")]),
        ]);
        assert_eq!(decode(&encode(&value)), Ok(value));
    }

    #[test]
    fn reports_value_spans() {
        let input = b"d4:infod4:name1:x6:lengthi5eee";
        let entries = Decoder::new(input)
            .parse_dict_with_spans_complete()
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, b"info");
        assert_eq!(&input[entries[0].span.clone()], b"d4:name1:x6:lengthi5ee");
    }
}
