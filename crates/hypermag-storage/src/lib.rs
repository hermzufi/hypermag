//! Piece/file offset mapping and SHA-1 piece verification.
//!
//! Runtime-free helpers used by the storage and engine layers. Actual file I/O
//! lives behind a `BlockStore` trait in later milestones.

#![forbid(unsafe_code)]

use hypermag_meta::{FileEntry, Metainfo};
use sha1::{Digest, Sha1};

/// A contiguous byte range inside one file belonging to a single piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub file_index: usize,
    pub file_offset: u64,
    pub length: u64,
}

/// Torrent layout derived from metainfo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub files: Vec<FileEntry>,
    pub piece_length: u64,
    pub total_length: u64,
}

impl Layout {
    #[must_use]
    pub fn from_meta(meta: &Metainfo<'_>) -> Self {
        Self {
            files: meta.info.file_entries(),
            piece_length: meta.info.piece_length,
            total_length: meta.info.total_length,
        }
    }

    #[must_use]
    pub fn piece_count(&self) -> u64 {
        if self.total_length == 0 || self.piece_length == 0 {
            0
        } else {
            self.total_length.div_ceil(self.piece_length)
        }
    }

    #[must_use]
    pub fn piece_offset(&self, index: u64) -> Option<u64> {
        let off = index.checked_mul(self.piece_length)?;
        if off >= self.total_length {
            None
        } else {
            Some(off)
        }
    }

    #[must_use]
    pub fn piece_length(&self, index: u64) -> Option<u64> {
        let start = self.piece_offset(index)?;
        let end = (start + self.piece_length).min(self.total_length);
        Some(end - start)
    }

    #[must_use]
    pub fn piece_segments(&self, index: u64) -> Option<Vec<Segment>> {
        let piece_len = self.piece_length(index)?;
        if piece_len == 0 {
            return Some(Vec::new());
        }
        let start = self.piece_offset(index)?;
        let end = start + piece_len;

        let mut cumulative = Vec::with_capacity(self.files.len() + 1);
        cumulative.push(0u64);
        for file in &self.files {
            let next = cumulative.last().copied().unwrap_or(0) + file.length;
            cumulative.push(next);
        }

        let mut segments = Vec::new();
        let mut pos = start;
        let mut file_idx = 0usize;
        while file_idx < self.files.len() && cumulative[file_idx + 1] <= pos {
            file_idx += 1;
        }
        while pos < end {
            if file_idx >= self.files.len() {
                return None;
            }
            let file_start = cumulative[file_idx];
            let file_end = cumulative[file_idx + 1];
            let chunk = (file_end - pos).min(end - pos);
            if chunk == 0 {
                file_idx += 1;
                continue;
            }
            segments.push(Segment {
                file_index: file_idx,
                file_offset: pos - file_start,
                length: chunk,
            });
            pos += chunk;
            if pos >= file_end {
                file_idx += 1;
            }
        }
        Some(segments)
    }
}

#[must_use]
pub fn sha1_piece(data: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(data);
    hasher.finalize().into()
}

#[must_use]
pub fn verify_piece(data: &[u8], expected: &[u8; 20]) -> bool {
    sha1_piece(data) == *expected
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypermag_meta::Metainfo;

    fn meta_bytes() -> Vec<u8> {
        use hypermag_bencode::Value;
        use std::collections::BTreeMap;
        let pieces = [0u8; 40];
        let info = Value::Dict(BTreeMap::from([
            (b"length".as_slice(), Value::Int(5)),
            (b"name".as_slice(), Value::Bytes(b"a.txt")),
            (b"piece length".as_slice(), Value::Int(4)),
            (b"pieces".as_slice(), Value::Bytes(&pieces)),
        ]));
        let root = Value::Dict(BTreeMap::from([(b"info".as_slice(), info)]));
        hypermag_bencode::encode(&root)
    }

    #[test]
    fn single_file_layout() {
        let bytes = meta_bytes();
        let meta = Metainfo::parse(&bytes).unwrap();
        let layout = Layout::from_meta(&meta);
        assert_eq!(layout.piece_count(), 2);
        assert_eq!(layout.piece_offset(0), Some(0));
        assert_eq!(layout.piece_length(0), Some(4));
        assert_eq!(layout.piece_length(1), Some(1));
        let segs = layout.piece_segments(0).unwrap();
        assert_eq!(
            segs,
            vec![Segment {
                file_index: 0,
                file_offset: 0,
                length: 4
            }]
        );
        assert!(layout.piece_offset(2).is_none());
    }

    #[test]
    fn multi_file_segments() {
        use hypermag_bencode::Value;
        use std::collections::BTreeMap;
        let pieces = [0u8; 20];
        let info = Value::Dict(BTreeMap::from([
            (b"name".as_slice(), Value::Bytes(b"dir")),
            (b"piece length".as_slice(), Value::Int(16)),
            (b"pieces".as_slice(), Value::Bytes(&pieces)),
            (
                b"files".as_slice(),
                Value::List(vec![
                    Value::Dict(BTreeMap::from([
                        (b"length".as_slice(), Value::Int(4)),
                        (
                            b"path".as_slice(),
                            Value::List(vec![Value::Bytes(b"a"), Value::Bytes(b"b.bin")]),
                        ),
                    ])),
                    Value::Dict(BTreeMap::from([
                        (b"length".as_slice(), Value::Int(6)),
                        (b"path".as_slice(), Value::List(vec![Value::Bytes(b"c")])),
                    ])),
                ]),
            ),
        ]));
        let root = Value::Dict(BTreeMap::from([(b"info".as_slice(), info)]));
        let bytes = hypermag_bencode::encode(&root);
        let meta = Metainfo::parse(&bytes).unwrap();
        let layout = Layout::from_meta(&meta);
        let segs = layout.piece_segments(0).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].file_index, 0);
        assert_eq!(segs[0].file_offset, 0);
        assert_eq!(segs[0].length, 4);
        assert_eq!(segs[1].file_index, 1);
        assert_eq!(segs[1].file_offset, 0);
        assert_eq!(segs[1].length, 6);
    }

    #[test]
    fn sha1_verify() {
        let data = b"abc";
        let expected = sha1_piece(data);
        assert!(verify_piece(data, &expected));
        let mut bad = expected;
        bad[0] ^= 1;
        assert!(!verify_piece(data, &bad));
    }
}
