//! Single-peer download and swarm utilities.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;

use hypermag_bencode::{Value, decode, encode};
use hypermag_meta::{Magnet, Metainfo};
use hypermag_proto::Message;
use hypermag_storage::{Layout, verify_piece};
use tokio::fs::{self, File};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::time::{Duration, timeout};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Net(#[from] hypermag_net::Error),
    #[error(transparent)]
    Meta(#[from] hypermag_meta::Error),
    #[error(transparent)]
    Bencode(#[from] hypermag_bencode::Error),
    #[error("piece hash mismatch index {0}")]
    HashMismatch(u64),
    #[error("timeout")]
    Timeout,
    #[error("metadata not found")]
    NoMetadata,
}

#[derive(Debug, Clone)]
pub struct SwarmStats {
    pub downloaded: u64,
    pub uploaded: u64,
    pub peers: usize,
}

#[derive(Debug, Clone)]
pub struct SeedingPolicy {
    pub seed_ratio: f64,
    pub seed_time_hours: u32,
}

impl Default for SeedingPolicy {
    fn default() -> Self {
        Self {
            seed_ratio: 1.0,
            seed_time_hours: 24,
        }
    }
}

pub struct Downloader {
    pub peer_id: [u8; 20],
}

impl Downloader {
    #[must_use]
    pub fn new(peer_id: [u8; 20]) -> Self {
        Self { peer_id }
    }

    pub async fn download_from_magnet(
        &self,
        magnet_uri: &str,
        addr: &str,
        out_dir: &Path,
    ) -> Result<(), Error> {
        let mag = Magnet::parse(magnet_uri)
            .map_err(|e| hypermag_meta::Error::InvalidMagnet(e.to_string()))?;
        let info_hash = mag
            .info_hash_v1
            .ok_or_else(|| hypermag_meta::Error::InvalidMagnet("v1 info hash required".into()))?;
        let mut peer = hypermag_net::dial(addr, info_hash, self.peer_id).await?;
        let _bitfield = timeout(Duration::from_secs(5), peer.recv())
            .await
            .map_err(|_| Error::Timeout)??;
        let mut ext = BTreeMap::new();
        ext.insert(
            &b"m"[..],
            Value::Dict(BTreeMap::from([(&b"ut_metadata"[..], Value::Int(1))])),
        );
        ext.insert(&b"p"[..], Value::Int(6881));
        ext.insert(&b"v"[..], Value::Bytes(b"hypermag"));
        let ext_dict = Value::Dict(ext);
        let payload = encode(&ext_dict);
        peer.send(&Message::Extended {
            id: 0,
            payload: std::borrow::Cow::Borrowed(&payload),
        })
        .await?;
        let mut info_dict_bytes: Option<Vec<u8>> = None;
        for piece in 0..64 {
            let mut req = BTreeMap::new();
            req.insert(&b"msg_type"[..], Value::Int(0));
            req.insert(&b"piece"[..], Value::Int(piece));
            let req_payload = encode(&Value::Dict(req));
            peer.send(&Message::Extended {
                id: 1,
                payload: std::borrow::Cow::Borrowed(&req_payload),
            })
            .await?;
            if let Ok(Ok(Message::Extended { payload, .. })) =
                timeout(Duration::from_secs(5), peer.recv()).await
            {
                if let Ok(v) = decode(&payload) {
                    if let Some(d) = v.as_dict() {
                        if let Some(mt) = d.get(&b"msg_type"[..]).and_then(|x| x.as_int()) {
                            if mt == 1 {
                                // try to find raw info dict after bencoded header? crude: if dict starts with 'd' look for total_size; store payload
                                if info_dict_bytes.is_none() {
                                    // heuristic: search for start of info dict '4:name' or try whole payload as info if small? better to pass through
                                    info_dict_bytes = Some(payload.into_owned());
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(info) = info_dict_bytes {
            // try to parse as wrapped torrent
            let mut root = BTreeMap::new();
            if let Ok(iv) = decode(&info) {
                root.insert(&b"info"[..], iv);
            } else {
                return Err(Error::NoMetadata);
            }
            let torrent = encode(&Value::Dict(root));
            return self.download_one_peer(addr, &torrent, out_dir).await;
        }
        Err(Error::NoMetadata)
    }

    pub async fn download_one_peer(
        &self,
        addr: &str,
        torrent: &[u8],
        out_dir: &Path,
    ) -> Result<(), Error> {
        let meta = Metainfo::parse(torrent)?;
        let layout = Layout::from_meta(&meta);
        let mut peer = hypermag_net::dial(addr, meta.info_hash_v1, self.peer_id).await?;

        let _bitfield = timeout(Duration::from_secs(5), peer.recv())
            .await
            .map_err(|_| Error::Timeout)??;
        peer.send(&Message::Interested).await?;
        let _unchoke = timeout(Duration::from_secs(5), peer.recv())
            .await
            .map_err(|_| Error::Timeout)??;

        fs::create_dir_all(out_dir).await?;
        let mut files: Vec<File> = Vec::with_capacity(layout.files.len());
        for file in &layout.files {
            let path = out_dir.join(file.path.join("/"));
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).await?;
            }
            let f = File::create(&path).await?;
            f.set_len(file.length).await?;
            files.push(f);
        }

        let piece_count = layout.piece_count();
        for index in 0..piece_count {
            let p_len = layout.piece_length(index).unwrap_or(0);
            if p_len == 0 {
                break;
            }
            peer.send(&Message::Request {
                index: index as u32,
                begin: 0,
                length: p_len as u32,
            })
            .await?;
            let msg = timeout(Duration::from_secs(30), peer.recv())
                .await
                .map_err(|_| Error::Timeout)??;
            let block = match msg {
                Message::Piece { block, .. } => block.into_owned(),
                _ => continue,
            };
            if let Some(expected) = meta.info.piece_hash(index) {
                if !verify_piece(&block, &expected) {
                    return Err(Error::HashMismatch(index));
                }
            }
            let piece_start = index * layout.piece_length;
            let mut written: u64 = 0;
            let mut cum: u64 = 0;
            for (fi, file) in layout.files.iter().enumerate() {
                if written >= p_len {
                    break;
                }
                let file_end = cum + file.length;
                while written < p_len {
                    let global = piece_start + written;
                    if global >= file_end {
                        break;
                    }
                    let chunk = ((p_len - written).min(file_end - global)) as usize;
                    let foff = global - cum;
                    files[fi].seek(std::io::SeekFrom::Start(foff)).await?;
                    files[fi]
                        .write_all(&block[written as usize..written as usize + chunk])
                        .await?;
                    written += chunk as u64;
                }
                cum += file.length;
            }
        }
        for mut f in files {
            let _ = f.shutdown().await;
        }
        Ok(())
    }
}
