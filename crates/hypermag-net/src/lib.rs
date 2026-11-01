//! Peer transport: TCP dial/listen and handshake.

#![forbid(unsafe_code)]

use std::time::Duration;

use bytes::{Buf, BytesMut};
use hypermag_proto::{DEFAULT_MAX_MESSAGE, Handshake, Message, decode_message};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Proto(#[from] hypermag_proto::Error),
    #[error("handshake timeout")]
    Timeout,
    #[error("info hash mismatch")]
    InfoHashMismatch,
}

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn dial(addr: &str, info_hash: [u8; 20], peer_id: [u8; 20]) -> Result<Peer, Error> {
    let stream = match timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(addr)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(Error::Io(e)),
        Err(_) => return Err(Error::Timeout),
    };
    handshake_outbound(stream, info_hash, peer_id).await
}

pub async fn listen(addr: &str, info_hash: [u8; 20], peer_id: [u8; 20]) -> Result<Peer, Error> {
    let listener = TcpListener::bind(addr).await?;
    let (stream, _peer) = listener.accept().await?;
    handshake_inbound(stream, info_hash, peer_id).await
}

async fn handshake_outbound(
    mut stream: TcpStream,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
) -> Result<Peer, Error> {
    let hs = Handshake::new(info_hash, peer_id);
    stream.write_all(&hs.to_bytes()).await?;
    let mut buf = [0u8; 68];
    match timeout(HANDSHAKE_TIMEOUT, stream.read_exact(&mut buf)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(Error::Io(e)),
        Err(_) => return Err(Error::Timeout),
    }
    let theirs = Handshake::parse(&buf)?;
    if theirs.info_hash != info_hash {
        return Err(Error::InfoHashMismatch);
    }
    Ok(Peer {
        stream,
        buf: BytesMut::with_capacity(4096),
    })
}

async fn handshake_inbound(
    mut stream: TcpStream,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
) -> Result<Peer, Error> {
    let mut buf = [0u8; 68];
    match timeout(HANDSHAKE_TIMEOUT, stream.read_exact(&mut buf)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(Error::Io(e)),
        Err(_) => return Err(Error::Timeout),
    }
    let theirs = Handshake::parse(&buf)?;
    if theirs.info_hash != info_hash {
        return Err(Error::InfoHashMismatch);
    }
    let hs = Handshake::new(info_hash, peer_id);
    stream.write_all(&hs.to_bytes()).await?;
    Ok(Peer {
        stream,
        buf: BytesMut::with_capacity(4096),
    })
}

pub struct Peer {
    stream: TcpStream,
    buf: BytesMut,
}

impl Peer {
    pub async fn send(&mut self, msg: &Message<'_>) -> Result<(), Error> {
        self.stream.write_all(&msg.to_bytes()).await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Message<'static>, Error> {
        loop {
            let needed = if self.buf.len() >= 4 {
                let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
                if len == 0 {
                    self.buf.advance(4);
                    continue;
                }
                4 + len as usize
            } else {
                4
            };
            if self.buf.len() >= needed && needed >= 4 {
                let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
                if len == 0 {
                    self.buf.advance(4);
                    continue;
                }
                let total = needed;
                if self.buf.len() >= total {
                    let bytes = self.buf.split_to(total);
                    let (msg, _consumed) = decode_message(&bytes, DEFAULT_MAX_MESSAGE)?;
                    return Ok(msg.into_owned());
                }
            }
            let n = self.stream.read_buf(&mut self.buf).await?;
            if n == 0 {
                return Err(
                    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "closed").into(),
                );
            }
        }
    }
}
