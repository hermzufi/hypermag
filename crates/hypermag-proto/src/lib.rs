//! BitTorrent peer wire protocol (BEP 3), plus BEP 10 extension framing.
//!
//! Runtime-free, allocation-light codec. Messages borrow their payload bytes
//! from the input buffer.

#![forbid(unsafe_code)]

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("input too short: need {needed} bytes, have {have}")]
    Truncated { needed: usize, have: usize },
    #[error("invalid handshake: {0}")]
    BadHandshake(&'static str),
    #[error("message length {length} exceeds maximum {max}")]
    TooLarge { length: u32, max: u32 },
    #[error("unknown message id {0}")]
    UnknownId(u8),
}

pub const PROTOCOL: &[u8] = b"BitTorrent protocol";
pub const HANDSHAKE_LEN: usize = 68;
pub const DEFAULT_MAX_MESSAGE: u32 = 1 << 20; // 1 MiB, above any sane block

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    pub reserved: [u8; 8],
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

impl Handshake {
    #[must_use]
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Self {
        Self {
            reserved: [0u8; 8],
            info_hash,
            peer_id,
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(19);
        out.extend_from_slice(PROTOCOL);
        out.extend_from_slice(&self.reserved);
        out.extend_from_slice(&self.info_hash);
        out.extend_from_slice(&self.peer_id);
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HANDSHAKE_LEN);
        self.encode(&mut out);
        out
    }

    pub fn parse(input: &[u8]) -> Result<Self, Error> {
        if input.len() < HANDSHAKE_LEN {
            return Err(Error::Truncated {
                needed: HANDSHAKE_LEN,
                have: input.len(),
            });
        }
        if input[0] != 19 || &input[1..20] != PROTOCOL {
            return Err(Error::BadHandshake("protocol string mismatch"));
        }
        let mut reserved = [0u8; 8];
        reserved.copy_from_slice(&input[20..28]);
        let mut info_hash = [0u8; 20];
        info_hash.copy_from_slice(&input[28..48]);
        let mut peer_id = [0u8; 20];
        peer_id.copy_from_slice(&input[48..68]);
        Ok(Self {
            reserved,
            info_hash,
            peer_id,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message<'a> {
    KeepAlive,
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(u32),
    Bitfield(&'a [u8]),
    Request {
        index: u32,
        begin: u32,
        length: u32,
    },
    Piece {
        index: u32,
        begin: u32,
        block: &'a [u8],
    },
    Cancel {
        index: u32,
        begin: u32,
        length: u32,
    },
    Port(u16),
    Extended {
        id: u8,
        payload: &'a [u8],
    },
    Unknown {
        id: u8,
        payload: &'a [u8],
    },
}

impl<'a> Message<'a> {
    #[must_use]
    pub fn id(&self) -> Option<u8> {
        match self {
            Self::KeepAlive => None,
            Self::Choke => Some(0),
            Self::Unchoke => Some(1),
            Self::Interested => Some(2),
            Self::NotInterested => Some(3),
            Self::Have(_) => Some(4),
            Self::Bitfield(_) => Some(5),
            Self::Request { .. } => Some(6),
            Self::Piece { .. } => Some(7),
            Self::Cancel { .. } => Some(8),
            Self::Port(_) => Some(9),
            Self::Extended { .. } => Some(20),
            Self::Unknown { id, .. } => Some(*id),
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::KeepAlive => out.extend_from_slice(&0u32.to_be_bytes()),
            Self::Choke => write_frame(out, 0, &[]),
            Self::Unchoke => write_frame(out, 1, &[]),
            Self::Interested => write_frame(out, 2, &[]),
            Self::NotInterested => write_frame(out, 3, &[]),
            Self::Have(index) => write_frame(out, 4, &index.to_be_bytes()),
            Self::Bitfield(bits) => write_frame(out, 5, bits),
            Self::Request {
                index,
                begin,
                length,
            } => {
                let mut buf = [0u8; 12];
                buf[0..4].copy_from_slice(&index.to_be_bytes());
                buf[4..8].copy_from_slice(&begin.to_be_bytes());
                buf[8..12].copy_from_slice(&length.to_be_bytes());
                write_frame(out, 6, &buf);
            }
            Self::Piece {
                index,
                begin,
                block,
            } => {
                let mut header = [0u8; 8];
                header[0..4].copy_from_slice(&index.to_be_bytes());
                header[4..8].copy_from_slice(&begin.to_be_bytes());
                let mut payload = Vec::with_capacity(8 + block.len());
                payload.extend_from_slice(&header);
                payload.extend_from_slice(block);
                write_frame(out, 7, &payload);
            }
            Self::Cancel {
                index,
                begin,
                length,
            } => {
                let mut buf = [0u8; 12];
                buf[0..4].copy_from_slice(&index.to_be_bytes());
                buf[4..8].copy_from_slice(&begin.to_be_bytes());
                buf[8..12].copy_from_slice(&length.to_be_bytes());
                write_frame(out, 8, &buf);
            }
            Self::Port(port) => write_frame(out, 9, &port.to_be_bytes()),
            Self::Extended { id, payload } => {
                let mut body = Vec::with_capacity(1 + payload.len());
                body.push(*id);
                body.extend_from_slice(payload);
                write_frame(out, 20, &body);
            }
            Self::Unknown { id, payload } => write_frame(out, *id, payload),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

fn write_frame(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
    let len = (1 + payload.len()) as u32;
    out.extend_from_slice(&len.to_be_bytes());
    out.push(id);
    out.extend_from_slice(payload);
}

/// Decode one framed message from `input`.
///
/// Returns the message and the number of bytes consumed (including the 4-byte
/// length prefix). Returns `Error::Truncated` if the frame is incomplete.
pub fn decode_message(input: &[u8], max: u32) -> Result<(Message<'_>, usize), Error> {
    if input.len() < 4 {
        return Err(Error::Truncated {
            needed: 4,
            have: input.len(),
        });
    }
    let length = u32::from_be_bytes([input[0], input[1], input[2], input[3]]);
    if length == 0 {
        return Ok((Message::KeepAlive, 4));
    }
    if length > max {
        return Err(Error::TooLarge { length, max });
    }
    let total = 4 + length as usize;
    if input.len() < total {
        return Err(Error::Truncated {
            needed: total,
            have: input.len(),
        });
    }
    let body = &input[4..total];
    let id = body[0];
    let payload = &body[1..];
    let msg = match id {
        0 => Message::Choke,
        1 => Message::Unchoke,
        2 => Message::Interested,
        3 => Message::NotInterested,
        4 => {
            if payload.len() < 4 {
                return Err(Error::Truncated {
                    needed: 4,
                    have: payload.len(),
                });
            }
            Message::Have(u32::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ]))
        }
        5 => Message::Bitfield(payload),
        6 => {
            if payload.len() < 12 {
                return Err(Error::Truncated {
                    needed: 12,
                    have: payload.len(),
                });
            }
            Message::Request {
                index: u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]),
                begin: u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]),
                length: u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]),
            }
        }
        7 => {
            if payload.len() < 8 {
                return Err(Error::Truncated {
                    needed: 8,
                    have: payload.len(),
                });
            }
            Message::Piece {
                index: u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]),
                begin: u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]),
                block: &payload[8..],
            }
        }
        8 => {
            if payload.len() < 12 {
                return Err(Error::Truncated {
                    needed: 12,
                    have: payload.len(),
                });
            }
            Message::Cancel {
                index: u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]),
                begin: u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]),
                length: u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]),
            }
        }
        9 => {
            if payload.len() < 2 {
                return Err(Error::Truncated {
                    needed: 2,
                    have: payload.len(),
                });
            }
            Message::Port(u16::from_be_bytes([payload[0], payload[1]]))
        }
        20 => {
            if payload.is_empty() {
                return Err(Error::Truncated { needed: 1, have: 0 });
            }
            Message::Extended {
                id: payload[0],
                payload: &payload[1..],
            }
        }
        other => Message::Unknown { id: other, payload },
    };
    Ok((msg, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_round_trip() {
        let h = Handshake::new([7u8; 20], [9u8; 20]);
        let bytes = h.to_bytes();
        assert_eq!(bytes.len(), HANDSHAKE_LEN);
        let parsed = Handshake::parse(&bytes).unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn handshake_rejects_bad_protocol() {
        let mut bytes = Handshake::new([0u8; 20], [0u8; 20]).to_bytes();
        bytes[1] = b'X';
        assert!(matches!(
            Handshake::parse(&bytes),
            Err(Error::BadHandshake(_))
        ));
    }

    #[test]
    fn handshake_truncated() {
        let bytes = Handshake::new([0u8; 20], [0u8; 20]).to_bytes();
        assert!(matches!(
            Handshake::parse(&bytes[..60]),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn keep_alive() {
        let (msg, n) = decode_message(&[0, 0, 0, 0], DEFAULT_MAX_MESSAGE).unwrap();
        assert_eq!(msg, Message::KeepAlive);
        assert_eq!(n, 4);
        assert_eq!(Message::KeepAlive.to_bytes(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn simple_messages_round_trip() {
        for msg in [
            Message::Choke,
            Message::Unchoke,
            Message::Interested,
            Message::NotInterested,
        ] {
            let bytes = msg.to_bytes();
            let (dec, n) = decode_message(&bytes, DEFAULT_MAX_MESSAGE).unwrap();
            assert_eq!(dec, msg);
            assert_eq!(n, bytes.len());
        }
    }

    #[test]
    fn have_round_trip() {
        let msg = Message::Have(42);
        let bytes = msg.to_bytes();
        let (dec, _) = decode_message(&bytes, DEFAULT_MAX_MESSAGE).unwrap();
        assert_eq!(dec, msg);
    }

    #[test]
    fn request_piece_cancel_round_trip() {
        let req = Message::Request {
            index: 1,
            begin: 2,
            length: 3,
        };
        let bytes = req.to_bytes();
        let (dec, _) = decode_message(&bytes, DEFAULT_MAX_MESSAGE).unwrap();
        assert_eq!(dec, req);

        let piece = Message::Piece {
            index: 1,
            begin: 2,
            block: b"hello",
        };
        let bytes = piece.to_bytes();
        let (dec, n) = decode_message(&bytes, DEFAULT_MAX_MESSAGE).unwrap();
        assert_eq!(dec, piece);
        assert_eq!(n, bytes.len());

        let cancel = Message::Cancel {
            index: 1,
            begin: 2,
            length: 3,
        };
        let bytes = cancel.to_bytes();
        let (dec, _) = decode_message(&bytes, DEFAULT_MAX_MESSAGE).unwrap();
        assert_eq!(dec, cancel);
    }

    #[test]
    fn bitfield_and_port() {
        let bf = Message::Bitfield(b"\xaa\x55");
        let bytes = bf.to_bytes();
        let (dec, _) = decode_message(&bytes, DEFAULT_MAX_MESSAGE).unwrap();
        assert_eq!(dec, bf);

        let port = Message::Port(6881);
        let bytes = port.to_bytes();
        let (dec, _) = decode_message(&bytes, DEFAULT_MAX_MESSAGE).unwrap();
        assert_eq!(dec, port);
    }

    #[test]
    fn extended_round_trip() {
        let msg = Message::Extended {
            id: 1,
            payload: b"metadata",
        };
        let bytes = msg.to_bytes();
        let (dec, _) = decode_message(&bytes, DEFAULT_MAX_MESSAGE).unwrap();
        assert_eq!(dec, msg);
    }

    #[test]
    fn truncated_frame() {
        // length says 10 but only 4 bytes present
        let mut bytes = vec![0, 0, 0, 10];
        assert!(matches!(
            decode_message(&bytes, DEFAULT_MAX_MESSAGE),
            Err(Error::Truncated { .. })
        ));
        bytes.extend_from_slice(&[0u8; 6]);
        assert!(matches!(
            decode_message(&bytes, DEFAULT_MAX_MESSAGE),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn too_large() {
        let bytes = [0xff, 0xff, 0xff, 0xff];
        assert!(matches!(
            decode_message(&bytes, 100),
            Err(Error::TooLarge { .. })
        ));
    }

    #[test]
    fn unknown_id_preserved() {
        // id 99, empty payload -> length 1
        let bytes = [0, 0, 0, 1, 99];
        let (msg, n) = decode_message(&bytes, DEFAULT_MAX_MESSAGE).unwrap();
        assert_eq!(
            msg,
            Message::Unknown {
                id: 99,
                payload: &[]
            }
        );
        assert_eq!(n, 5);
    }

    #[test]
    fn multiple_frames_consumed_incrementally() {
        let mut buf = Vec::new();
        Message::Choke.encode(&mut buf);
        Message::Have(5).encode(&mut buf);
        let (m1, n1) = decode_message(&buf, DEFAULT_MAX_MESSAGE).unwrap();
        assert_eq!(m1, Message::Choke);
        let (m2, n2) = decode_message(&buf[n1..], DEFAULT_MAX_MESSAGE).unwrap();
        assert_eq!(m2, Message::Have(5));
        assert_eq!(n1 + n2, buf.len());
    }
}
