//! HTTP tracker announce (minimal).

#![forbid(unsafe_code)]

use hypermag_bencode::decode;
use std::net::SocketAddr;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Bencode(#[from] hypermag_bencode::Error),
    #[error("tracker error: {0}")]
    Tracker(String),
}

#[derive(Debug, Clone)]
pub struct Peer {
    pub addr: SocketAddr,
}

#[derive(Debug, Clone)]
pub struct AnnounceResponse {
    pub peers: Vec<Peer>,
    pub interval: Option<i64>,
    pub min_interval: Option<i64>,
    pub peers6: Vec<Peer>,
}

fn decode_compact_peers(bytes: &[u8]) -> Vec<Peer> {
    let mut out = Vec::new();
    for chunk in bytes.chunks_exact(6) {
        let ip = std::net::Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);
        let port = u16::from_be_bytes([chunk[4], chunk[5]]);
        out.push(Peer {
            addr: SocketAddr::new(std::net::IpAddr::V4(ip), port),
        });
    }
    out
}

fn decode_compact_peers6(bytes: &[u8]) -> Vec<Peer> {
    let mut out = Vec::new();
    for chunk in bytes.chunks_exact(18) {
        let mut ip = [0u8; 16];
        ip.copy_from_slice(&chunk[0..16]);
        let port = u16::from_be_bytes([chunk[16], chunk[17]]);
        out.push(Peer {
            addr: SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::from(ip)), port),
        });
    }
    out
}

pub fn parse_announce_response(data: &[u8]) -> Result<AnnounceResponse, Error> {
    let value = decode(data)?;
    let dict = value
        .as_dict()
        .ok_or_else(|| Error::Tracker("not a dict".into()))?;
    if let Some(failure) = dict.get(&b"failure reason"[..]) {
        if let Some(s) = failure.as_str() {
            return Err(Error::Tracker(s.to_string()));
        }
    }
    let mut peers = Vec::new();
    if let Some(p) = dict.get(&b"peers"[..]) {
        if let Some(bytes) = p.as_bytes() {
            peers = decode_compact_peers(bytes);
        } else if let Some(list) = p.as_list() {
            for item in list {
                if let Some(d) = item.as_dict() {
                    if let Some(ip) = d
                        .get(&b"ip"[..])
                        .and_then(|v| v.as_bytes())
                        .and_then(|b| std::str::from_utf8(b).ok())
                    {
                        if let Some(port) = d.get(&b"port"[..]).and_then(|v| v.as_int()) {
                            if let Ok(sa) = format!("{ip}:{port}").parse::<SocketAddr>() {
                                peers.push(Peer { addr: sa });
                            }
                        }
                    }
                }
            }
        }
    }
    let mut peers6 = Vec::new();
    if let Some(p) = dict.get(&b"peers6"[..]) {
        if let Some(bytes) = p.as_bytes() {
            peers6 = decode_compact_peers6(bytes);
        }
    }
    let interval = dict.get(&b"interval"[..]).and_then(|v| v.as_int());
    let min_interval = dict.get(&b"min interval"[..]).and_then(|v| v.as_int());
    Ok(AnnounceResponse {
        peers,
        interval,
        min_interval,
        peers6,
    })
}

pub fn build_announce_url(
    tracker: &str,
    info_hash: &[u8; 20],
    peer_id: &[u8; 20],
    port: u16,
    uploaded: u64,
    downloaded: u64,
    left: u64,
) -> String {
    let mut url = tracker.to_string();
    if !url.contains('?') {
        url.push('?');
    } else {
        url.push('&');
    }
    let ih = urlencoding::encode_binary(info_hash);
    let pid = urlencoding::encode_binary(peer_id);
    url.push_str(&format!(
        "info_hash={}&peer_id={}&port={}&uploaded={}&downloaded={}&left={}&compact=1&event=started",
        ih, pid, port, uploaded, downloaded, left
    ));
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypermag_bencode::{Value, encode};
    use std::collections::BTreeMap;

    #[test]
    fn parses_compact_peers() {
        let mut dict = BTreeMap::new();
        dict.insert(&b"peers"[..], Value::Bytes(&[127, 0, 0, 1, 0x1a, 0xe1]));
        let resp = parse_announce_response(&encode(&Value::Dict(dict))).unwrap();
        assert_eq!(resp.peers.len(), 1);
    }
}
