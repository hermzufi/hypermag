//! `.torrent` metainfo and magnet URI parsing.
//!
//! The v1 info-hash is computed over the exact bytes of the `info` dictionary
//! as they appeared in the input, not over a re-encoding. This matters for
//! interoperability with torrents whose dictionaries are not canonically
//! ordered.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashSet};

use data_encoding::{BASE32_NOPAD, HEXLOWER_PERMISSIVE};
use hypermag_bencode::{Decoder, DictEntry, Value};
use sha1::{Digest, Sha1};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Bencode(#[from] hypermag_bencode::Error),
    #[error("missing required field `{0}`")]
    MissingField(&'static str),
    #[error("field `{field}` has the wrong type")]
    WrongType { field: &'static str },
    #[error("invalid value for `{field}`: {reason}")]
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },
    #[error("torrent has zero total length")]
    EmptyTorrent,
    #[error("`pieces` length is not a multiple of 20")]
    InvalidPiecesLength,
    #[error("piece count mismatch: infohash implies {expected}, `pieces` holds {actual}")]
    PieceCountMismatch { expected: u64, actual: u64 },
    #[error("numeric overflow while computing torrent size")]
    LengthOverflow,
    #[error("invalid magnet URI: {0}")]
    InvalidMagnet(String),
    #[error("invalid info hash: {0}")]
    InvalidInfoHash(String),
}

/// One file in a torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Path components relative to the torrent root.
    pub path: Vec<String>,
    pub length: u64,
}

/// The `info` dictionary, split by torrent mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    SingleFile { length: u64 },
    MultiFile { files: Vec<FileEntry> },
}

impl Mode {
    fn total_length(&self) -> Result<u64, Error> {
        match self {
            Self::SingleFile { length } => Ok(*length),
            Self::MultiFile { files } => files
                .iter()
                .try_fold(0u64, |total, file| total.checked_add(file.length))
                .ok_or(Error::LengthOverflow),
        }
    }
}

/// Parsed `info` dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info<'a> {
    pub name: String,
    pub piece_length: u64,
    /// Concatenated 20-byte SHA-1 piece hashes.
    pub pieces: &'a [u8],
    pub private: bool,
    pub meta_version: Option<i64>,
    pub mode: Mode,
    pub total_length: u64,
}

impl Info<'_> {
    #[must_use]
    pub fn piece_count(&self) -> u64 {
        self.pieces.len() as u64 / 20
    }

    /// Copy one piece hash out of the concatenated `pieces` field.
    #[must_use]
    pub fn piece_hash(&self, index: u64) -> Option<[u8; 20]> {
        let start = usize::try_from(index).ok()?.checked_mul(20)?;
        let end = start.checked_add(20)?;
        let mut hash = [0u8; 20];
        hash.copy_from_slice(self.pieces.get(start..end)?);
        Some(hash)
    }

    /// All files, synthesizing a single entry for single-file torrents.
    ///
    /// This clones; hot paths should iterate [`Mode`] directly.
    #[must_use]
    pub fn file_entries(&self) -> Vec<FileEntry> {
        match &self.mode {
            Mode::SingleFile { length } => vec![FileEntry {
                path: vec![self.name.clone()],
                length: *length,
            }],
            Mode::MultiFile { files } => files.clone(),
        }
    }
}

/// A parsed `.torrent` file.
#[derive(Debug, Clone)]
pub struct Metainfo<'a> {
    pub announce: Option<String>,
    pub announce_list: Vec<Vec<String>>,
    pub info: Info<'a>,
    pub info_hash_v1: [u8; 20],
    /// Exact bencoded bytes of the `info` dictionary.
    pub info_bytes: &'a [u8],
    pub comment: Option<String>,
    pub created_by: Option<String>,
    pub creation_date: Option<i64>,
    pub url_list: Vec<String>,
}

impl Metainfo<'_> {
    /// Parse a complete `.torrent` file.
    pub fn parse(bytes: &[u8]) -> Result<Metainfo<'_>, Error> {
        let entries = Decoder::new(bytes).parse_dict_with_spans_complete()?;

        let mut announce = None;
        let mut announce_list = Vec::new();
        let mut comment = None;
        let mut created_by = None;
        let mut creation_date = None;
        let mut url_list = Vec::new();
        let mut info_entry: Option<&DictEntry<'_>> = None;

        for entry in &entries {
            match entry.key {
                b"announce" => announce = Some(text(value_bytes(&entry.value, "announce")?)),
                b"announce-list" => announce_list = parse_tiers(&entry.value)?,
                b"info" => info_entry = Some(entry),
                b"comment" => comment = Some(text(value_bytes(&entry.value, "comment")?)),
                b"created by" => {
                    created_by = Some(text(value_bytes(&entry.value, "created by")?));
                }
                b"creation date" => creation_date = Some(value_int(&entry.value, "creation date")?),
                b"url-list" => url_list = parse_url_list(&entry.value)?,
                _ => {}
            }
        }

        let info_entry = info_entry.ok_or(Error::MissingField("info"))?;
        let info_dict = info_entry
            .value
            .as_dict()
            .ok_or(Error::WrongType { field: "info" })?;

        let name = text(required_bytes(info_dict, b"name", "name")?);
        let piece_length = required_int(info_dict, b"piece length", "piece length")?;
        if piece_length <= 0 {
            return Err(Error::InvalidField {
                field: "piece length",
                reason: "must be positive",
            });
        }
        let piece_length = piece_length as u64;

        let pieces = required_bytes(info_dict, b"pieces", "pieces")?;
        if pieces.len() % 20 != 0 {
            return Err(Error::InvalidPiecesLength);
        }

        let mode = if let Some(length) = optional_int(info_dict, b"length", "length")? {
            if length < 0 {
                return Err(Error::InvalidField {
                    field: "length",
                    reason: "must not be negative",
                });
            }
            Mode::SingleFile {
                length: length as u64,
            }
        } else {
            let files_value = info_dict
                .get(b"files".as_slice())
                .ok_or(Error::MissingField("files"))?;
            let files = files_value
                .as_list()
                .ok_or(Error::WrongType { field: "files" })?;
            let mut parsed = Vec::with_capacity(files.len());
            for file in files {
                let file_dict = file.as_dict().ok_or(Error::WrongType { field: "files" })?;
                let length = required_int(file_dict, b"length", "files.length")?;
                if length < 0 {
                    return Err(Error::InvalidField {
                        field: "files.length",
                        reason: "must not be negative",
                    });
                }
                let path_value = file_dict
                    .get(b"path".as_slice())
                    .ok_or(Error::MissingField("files.path"))?;
                let path_list = path_value.as_list().ok_or(Error::WrongType {
                    field: "files.path",
                })?;
                let mut path = Vec::with_capacity(path_list.len());
                for component in path_list {
                    path.push(text(component.as_bytes().ok_or(Error::WrongType {
                        field: "files.path",
                    })?));
                }
                if path.is_empty() {
                    return Err(Error::InvalidField {
                        field: "files.path",
                        reason: "must not be empty",
                    });
                }
                parsed.push(FileEntry {
                    path,
                    length: length as u64,
                });
            }
            if parsed.is_empty() {
                return Err(Error::InvalidField {
                    field: "files",
                    reason: "must not be empty",
                });
            }
            Mode::MultiFile { files: parsed }
        };

        let total_length = mode.total_length()?;
        if total_length == 0 {
            return Err(Error::EmptyTorrent);
        }

        let expected = total_length.div_ceil(piece_length);
        let actual = pieces.len() as u64 / 20;
        if expected != actual {
            return Err(Error::PieceCountMismatch { expected, actual });
        }

        let private = optional_int(info_dict, b"private", "private")? == Some(1);
        let meta_version = optional_int(info_dict, b"meta version", "meta version")?;

        let info_bytes = &bytes[info_entry.span.clone()];
        let mut hasher = Sha1::new();
        hasher.update(info_bytes);
        let info_hash_v1: [u8; 20] = hasher.finalize().into();

        Ok(Metainfo {
            announce,
            announce_list,
            info: Info {
                name,
                piece_length,
                pieces,
                private,
                meta_version,
                mode,
                total_length,
            },
            info_hash_v1,
            info_bytes,
            comment,
            created_by,
            creation_date,
            url_list,
        })
    }

    #[must_use]
    pub fn info_hash_hex(&self) -> String {
        data_encoding::HEXLOWER.encode(&self.info_hash_v1)
    }

    /// All trackers, deduplicated, with `announce` first.
    #[must_use]
    pub fn trackers(&self) -> Vec<&str> {
        let mut seen = HashSet::new();
        let mut trackers = Vec::new();
        for tracker in std::iter::once(self.announce.as_deref())
            .flatten()
            .chain(self.announce_list.iter().flatten().map(String::as_str))
        {
            if seen.insert(tracker) {
                trackers.push(tracker);
            }
        }
        trackers
    }

    /// Build a magnet URI for this torrent.
    #[must_use]
    pub fn magnet_link(&self) -> String {
        let mut link = format!("magnet:?xt=urn:btih:{}", self.info_hash_hex());
        if !self.info.name.is_empty() {
            link.push_str("&dn=");
            link.push_str(&percent_encode(self.info.name.as_bytes()));
        }
        for tracker in self.trackers() {
            link.push_str("&tr=");
            link.push_str(&percent_encode(tracker.as_bytes()));
        }
        link
    }
}

/// A parsed magnet URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Magnet {
    pub info_hash_v1: Option<[u8; 20]>,
    pub info_hash_v2: Option<[u8; 32]>,
    pub display_name: Option<String>,
    pub trackers: Vec<String>,
    pub peers: Vec<String>,
}

impl Magnet {
    /// Parse a `magnet:?xt=...` URI.
    pub fn parse(uri: &str) -> Result<Self, Error> {
        let query = uri
            .strip_prefix("magnet:?")
            .ok_or_else(|| Error::InvalidMagnet("URI must start with `magnet:?`".into()))?;

        let mut magnet = Self {
            info_hash_v1: None,
            info_hash_v2: None,
            display_name: None,
            trackers: Vec::new(),
            peers: Vec::new(),
        };

        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, raw) = pair.split_once('=').unwrap_or((pair, ""));
            let value = percent_decode(raw)?;
            match key {
                "xt" => parse_xt(&value, &mut magnet)?,
                "dn" => magnet.display_name = Some(text(&value)),
                "tr" => magnet.trackers.push(text(&value)),
                "x.pe" => magnet.peers.push(text(&value)),
                _ => {}
            }
        }

        if magnet.info_hash_v1.is_none() && magnet.info_hash_v2.is_none() {
            return Err(Error::InvalidMagnet("no `xt` info hash found".into()));
        }
        Ok(magnet)
    }

    #[must_use]
    pub fn info_hash_v1_hex(&self) -> Option<String> {
        self.info_hash_v1
            .as_ref()
            .map(|hash| data_encoding::HEXLOWER.encode(hash))
    }

    #[must_use]
    pub fn info_hash_v2_hex(&self) -> Option<String> {
        self.info_hash_v2
            .as_ref()
            .map(|hash| data_encoding::HEXLOWER.encode(hash))
    }
}

fn value_bytes<'a>(value: &Value<'a>, field: &'static str) -> Result<&'a [u8], Error> {
    value.as_bytes().ok_or(Error::WrongType { field })
}

fn value_int(value: &Value<'_>, field: &'static str) -> Result<i64, Error> {
    value.as_int().ok_or(Error::WrongType { field })
}

fn required_bytes<'a>(
    dict: &BTreeMap<&'a [u8], Value<'a>>,
    key: &[u8],
    field: &'static str,
) -> Result<&'a [u8], Error> {
    dict.get(key)
        .ok_or(Error::MissingField(field))?
        .as_bytes()
        .ok_or(Error::WrongType { field })
}

fn required_int(
    dict: &BTreeMap<&[u8], Value<'_>>,
    key: &[u8],
    field: &'static str,
) -> Result<i64, Error> {
    dict.get(key)
        .ok_or(Error::MissingField(field))?
        .as_int()
        .ok_or(Error::WrongType { field })
}

fn optional_int(
    dict: &BTreeMap<&[u8], Value<'_>>,
    key: &[u8],
    field: &'static str,
) -> Result<Option<i64>, Error> {
    match dict.get(key) {
        None => Ok(None),
        Some(value) => value.as_int().map(Some).ok_or(Error::WrongType { field }),
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn parse_tiers(value: &Value<'_>) -> Result<Vec<Vec<String>>, Error> {
    let list = value.as_list().ok_or(Error::WrongType {
        field: "announce-list",
    })?;
    let mut tiers = Vec::with_capacity(list.len());
    for tier in list {
        match tier {
            Value::List(items) => {
                let mut trackers = Vec::with_capacity(items.len());
                for item in items {
                    trackers.push(text(value_bytes(item, "announce-list")?));
                }
                tiers.push(trackers);
            }
            Value::Bytes(bytes) => tiers.push(vec![text(bytes)]),
            _ => {
                return Err(Error::WrongType {
                    field: "announce-list",
                });
            }
        }
    }
    Ok(tiers)
}

fn parse_url_list(value: &Value<'_>) -> Result<Vec<String>, Error> {
    match value {
        Value::Bytes(bytes) => Ok(vec![text(bytes)]),
        Value::List(items) => {
            let mut urls = Vec::with_capacity(items.len());
            for item in items {
                urls.push(text(value_bytes(item, "url-list")?));
            }
            Ok(urls)
        }
        _ => Err(Error::WrongType { field: "url-list" }),
    }
}

fn parse_xt(value: &[u8], magnet: &mut Magnet) -> Result<(), Error> {
    let text = std::str::from_utf8(value)
        .map_err(|_| Error::InvalidMagnet("`xt` is not valid UTF-8".into()))?;
    let lower = text.to_ascii_lowercase();

    if let Some(hash) = lower.strip_prefix("urn:btih:") {
        magnet.info_hash_v1 = Some(decode_btih(hash)?);
    } else if let Some(rest) = lower.strip_prefix("urn:btmh:") {
        if let Some(hex) = rest.strip_prefix("1220") {
            let bytes = HEXLOWER_PERMISSIVE.decode(hex.as_bytes()).map_err(|_| {
                Error::InvalidMagnet(format!("invalid `btmh` hash encoding: {hex}"))
            })?;
            let hash: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
                Error::InvalidMagnet(format!("`btmh` hash must be 32 bytes, got {}", bytes.len()))
            })?;
            magnet.info_hash_v2 = Some(hash);
        }
    }

    Ok(())
}

fn decode_btih(hash: &str) -> Result<[u8; 20], Error> {
    let bytes = match hash.len() {
        40 => HEXLOWER_PERMISSIVE
            .decode(hash.as_bytes())
            .map_err(|_| Error::InvalidInfoHash(format!("invalid hex info hash: {hash}")))?,
        32 => BASE32_NOPAD
            .decode(hash.to_ascii_uppercase().as_bytes())
            .map_err(|_| Error::InvalidInfoHash(format!("invalid base32 info hash: {hash}")))?,
        length => {
            return Err(Error::InvalidInfoHash(format!(
                "expected 40 hex or 32 base32 chars, got {length}"
            )));
        }
    };

    bytes.try_into().map_err(|bytes: Vec<u8>| {
        Error::InvalidInfoHash(format!("info hash must be 20 bytes, got {}", bytes.len()))
    })
}

fn percent_decode(input: &str) -> Result<Vec<u8>, Error> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let high = bytes.get(index + 1).copied().and_then(hex_value);
                let low = bytes.get(index + 2).copied().and_then(hex_value);
                match (high, low) {
                    (Some(high), Some(low)) => out.push((high << 4) | low),
                    _ => {
                        return Err(Error::InvalidMagnet(format!(
                            "truncated percent-escape at byte {index}"
                        )));
                    }
                }
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn percent_encode(input: &[u8]) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(input.len());
    for &byte in input {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Build Metainfo bytes from an info dict (for fetched metadata).
pub fn metainfo_from_info(info_dict_bytes: &[u8]) -> Vec<u8> {
    use std::collections::BTreeMap;
    let mut root = BTreeMap::new();
    // Reconstruct minimal torrent by re-parsing? Easier: wrap as dict
    // Parse info dict
    let info_val = hypermag_bencode::decode(info_dict_bytes)
        .unwrap_or(hypermag_bencode::Value::Dict(BTreeMap::new()));
    root.insert(&b"info"[..], info_val);
    hypermag_bencode::encode(&hypermag_bencode::Value::Dict(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single_file_torrent() -> (Vec<u8>, [u8; 20]) {
        let pieces = [0u8; 40];
        let info = Value::Dict(BTreeMap::from([
            (b"length".as_slice(), Value::Int(5)),
            (b"name".as_slice(), Value::Bytes(b"a.txt")),
            (b"piece length".as_slice(), Value::Int(4)),
            (b"pieces".as_slice(), Value::Bytes(&pieces)),
        ]));

        let mut hasher = Sha1::new();
        hasher.update(hypermag_bencode::encode(&info));
        let expected_hash: [u8; 20] = hasher.finalize().into();

        let root = Value::Dict(BTreeMap::from([
            (
                b"announce".as_slice(),
                Value::Bytes(b"http://tracker.example/announce"),
            ),
            (
                b"announce-list".as_slice(),
                Value::List(vec![Value::List(vec![Value::Bytes(
                    b"udp://tracker.example:80",
                )])]),
            ),
            (b"info".as_slice(), info),
        ]));

        (hypermag_bencode::encode(&root), expected_hash)
    }

    #[test]
    fn parses_single_file_torrent() {
        let (bytes, expected_hash) = single_file_torrent();
        let meta = Metainfo::parse(&bytes).unwrap();

        assert_eq!(meta.info.name, "a.txt");
        assert_eq!(meta.info.total_length, 5);
        assert_eq!(meta.info.piece_length, 4);
        assert_eq!(meta.info.piece_count(), 2);
        assert_eq!(meta.info_hash_v1, expected_hash);
        assert_eq!(meta.info.mode, Mode::SingleFile { length: 5 });
        assert_eq!(
            meta.info.file_entries(),
            vec![FileEntry {
                path: vec!["a.txt".into()],
                length: 5
            }]
        );
        assert_eq!(
            meta.trackers(),
            vec![
                "http://tracker.example/announce",
                "udp://tracker.example:80"
            ]
        );
        assert!(meta.magnet_link().starts_with("magnet:?xt=urn:btih:"));
        assert_eq!(meta.info.piece_hash(0), Some([0u8; 20]));
        assert_eq!(meta.info.piece_hash(2), None);
    }

    #[test]
    fn parses_multi_file_torrent() {
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
        assert_eq!(meta.info.total_length, 10);
        assert_eq!(meta.info.piece_count(), 1);
        assert_eq!(
            meta.info.mode,
            Mode::MultiFile {
                files: vec![
                    FileEntry {
                        path: vec!["a".into(), "b.bin".into()],
                        length: 4
                    },
                    FileEntry {
                        path: vec!["c".into()],
                        length: 6
                    },
                ]
            }
        );
        assert!(meta.announce.is_none());
        assert!(meta.trackers().is_empty());
    }

    #[test]
    fn rejects_piece_count_mismatch() {
        let pieces = [0u8; 20];
        let info = Value::Dict(BTreeMap::from([
            (b"length".as_slice(), Value::Int(100)),
            (b"name".as_slice(), Value::Bytes(b"a")),
            (b"piece length".as_slice(), Value::Int(16)),
            (b"pieces".as_slice(), Value::Bytes(&pieces)),
        ]));
        let root = Value::Dict(BTreeMap::from([(b"info".as_slice(), info)]));
        let bytes = hypermag_bencode::encode(&root);

        assert!(matches!(
            Metainfo::parse(&bytes),
            Err(Error::PieceCountMismatch {
                expected: 7,
                actual: 1
            })
        ));
    }

    #[test]
    fn rejects_missing_info() {
        let root = Value::Dict(BTreeMap::from([(
            b"announce".as_slice(),
            Value::Bytes(b"http://tracker"),
        )]));
        let bytes = hypermag_bencode::encode(&root);
        assert!(matches!(
            Metainfo::parse(&bytes),
            Err(Error::MissingField("info"))
        ));
    }

    #[test]
    fn parses_magnet_hex() {
        let hash = data_encoding::HEXLOWER.encode(&[7u8; 20]);
        let uri = format!("magnet:?xt=urn:btih:{hash}&dn=My%20File&tr=udp%3A%2F%2Ftracker%3A80");
        let magnet = Magnet::parse(&uri).unwrap();
        assert_eq!(magnet.info_hash_v1, Some([7u8; 20]));
        assert_eq!(magnet.display_name.as_deref(), Some("My File"));
        assert_eq!(magnet.trackers, vec!["udp://tracker:80".to_owned()]);
    }

    #[test]
    fn parses_magnet_base32() {
        let hash = [9u8; 20];
        let encoded = BASE32_NOPAD.encode(&hash);
        let magnet = Magnet::parse(&format!("magnet:?xt=urn:btih:{encoded}")).unwrap();
        assert_eq!(magnet.info_hash_v1, Some(hash));
    }

    #[test]
    fn parses_magnet_v2() {
        let hash = [3u8; 32];
        let uri = format!(
            "magnet:?xt=urn:btih:{}&xt=urn:btmh:1220{}",
            data_encoding::HEXLOWER.encode(&[1u8; 20]),
            data_encoding::HEXLOWER.encode(&hash)
        );
        let magnet = Magnet::parse(&uri).unwrap();
        assert_eq!(magnet.info_hash_v2, Some(hash));
        assert_eq!(magnet.info_hash_v2_hex().unwrap().len(), 64);
    }

    #[test]
    fn rejects_magnet_without_hash() {
        assert!(matches!(
            Magnet::parse("magnet:?dn=name"),
            Err(Error::InvalidMagnet(_))
        ));
        assert!(matches!(
            Magnet::parse("http://example.com"),
            Err(Error::InvalidMagnet(_))
        ));
    }

    #[test]
    fn percent_encode_round_trips() {
        let encoded = percent_encode(b"a b/c?d");
        assert_eq!(encoded, "a%20b%2Fc%3Fd");
        assert_eq!(percent_decode(&encoded).unwrap(), b"a b/c?d");
    }
}
