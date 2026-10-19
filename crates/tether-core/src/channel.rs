//! Noise XX secured message channel over any async byte stream.
//!
//! Wire format: `u16` big endian length, then one Noise transport frame. The plaintext of a
//! transport frame is one flag byte (0 = more chunks follow, 1 = last chunk) plus up to
//! 65518 bytes of a postcard encoded [`Message`]. Messages larger than one frame are
//! chunked and reassembled, capped at `MAX_MESSAGE_LEN`.
//!
//! Uses snow's stateless transport so the send and receive halves are independent and can
//! live in different tasks later (needed once input and media share one connection).

use crate::identity::Identity;
use snow::{Builder, StatelessTransportState};
use std::{io, sync::Arc};
use tether_proto::{decode, encode, CodecError, Message, MAX_MESSAGE_LEN, NOISE_PARAMS, PROLOGUE};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};

pub const MAX_FRAME: usize = u16::MAX as usize;
const TAG_LEN: usize = 16;
const MAX_CHUNK: usize = MAX_FRAME - TAG_LEN - 1;
const FLAG_MORE: u8 = 0;
const FLAG_LAST: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("noise error: {0}")]
    Noise(#[from] snow::Error),
    #[error("codec error: {0}")]
    Codec(#[from] CodecError),
    #[error("message exceeds the maximum size")]
    MessageTooLarge,
    #[error("protocol violation: {0}")]
    Violation(&'static str),
    #[error("connection closed")]
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

pub struct SecureSender<W> {
    w: W,
    state: Arc<StatelessTransportState>,
    nonce: u64,
}

pub struct SecureReceiver<R> {
    r: R,
    state: Arc<StatelessTransportState>,
    nonce: u64,
}

pub struct SecureChannel<S> {
    pub sender: SecureSender<WriteHalf<S>>,
    pub receiver: SecureReceiver<ReadHalf<S>>,
    remote_static: [u8; 32],
    handshake_hash: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> SecureChannel<S> {
    /// Runs the Noise XX handshake. Both sides learn and authenticate the other's static key.
    pub async fn handshake(
        stream: S,
        identity: &Identity,
        role: Role,
    ) -> Result<Self, ChannelError> {
        let (mut r, mut w) = tokio::io::split(stream);
        let params = NOISE_PARAMS.parse().expect("static noise params");
        let builder = Builder::new(params)
            .local_private_key(identity.private_key())
            .prologue(PROLOGUE);
        let mut hs = match role {
            Role::Initiator => builder.build_initiator()?,
            Role::Responder => builder.build_responder()?,
        };

        let mut buf = vec![0u8; MAX_FRAME];
        let mut my_turn = role == Role::Initiator;
        while !hs.is_handshake_finished() {
            if my_turn {
                let n = hs.write_message(&[], &mut buf)?;
                write_frames(&mut w, &[&buf[..n]]).await?;
            } else {
                let frame = read_frame(&mut r).await?;
                hs.read_message(&frame, &mut buf)?;
            }
            my_turn = !my_turn;
        }

        let remote_static: [u8; 32] = hs
            .get_remote_static()
            .and_then(|k| k.try_into().ok())
            .ok_or(ChannelError::Violation("peer sent no static key"))?;
        let handshake_hash = hs.get_handshake_hash().to_vec();
        let state = Arc::new(hs.into_stateless_transport_mode()?);

        Ok(Self {
            sender: SecureSender {
                w,
                state: state.clone(),
                nonce: 0,
            },
            receiver: SecureReceiver { r, state, nonce: 0 },
            remote_static,
            handshake_hash,
        })
    }
}

impl<S> SecureChannel<S> {
    pub fn remote_static(&self) -> &[u8; 32] {
        &self.remote_static
    }

    /// Unique to this session and identical on both ends. Used to bind pairing proofs.
    pub fn handshake_hash(&self) -> &[u8] {
        &self.handshake_hash
    }

    pub async fn send(&mut self, msg: &Message) -> Result<(), ChannelError>
    where
        S: AsyncWrite + Unpin,
    {
        self.sender.send(msg).await
    }

    pub async fn recv(&mut self) -> Result<Message, ChannelError>
    where
        S: AsyncRead + Unpin,
    {
        self.receiver.recv().await
    }
}

impl<W: AsyncWrite + Unpin> SecureSender<W> {
    pub async fn send(&mut self, msg: &Message) -> Result<(), ChannelError> {
        let bytes = encode(msg)?;
        if bytes.len() > MAX_MESSAGE_LEN {
            return Err(ChannelError::MessageTooLarge);
        }
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut chunks = bytes.chunks(MAX_CHUNK).peekable();
        while let Some(chunk) = chunks.next() {
            let flag = if chunks.peek().is_none() {
                FLAG_LAST
            } else {
                FLAG_MORE
            };
            let mut plain = Vec::with_capacity(chunk.len() + 1);
            plain.push(flag);
            plain.extend_from_slice(chunk);
            let mut out = vec![0u8; plain.len() + TAG_LEN];
            let n = self.state.write_message(self.nonce, &plain, &mut out)?;
            self.nonce += 1;
            out.truncate(n);
            frames.push(out);
        }
        let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        write_frames(&mut self.w, &refs).await?;
        Ok(())
    }
}

impl<R: AsyncRead + Unpin> SecureReceiver<R> {
    pub async fn recv(&mut self) -> Result<Message, ChannelError> {
        let mut message = Vec::new();
        loop {
            let frame = read_frame(&mut self.r).await?;
            let mut plain = vec![0u8; frame.len()];
            let n = self.state.read_message(self.nonce, &frame, &mut plain)?;
            self.nonce += 1;
            if n == 0 {
                return Err(ChannelError::Violation("empty transport frame"));
            }
            let chunk = &plain[1..n];
            if message.len() + chunk.len() > MAX_MESSAGE_LEN {
                return Err(ChannelError::MessageTooLarge);
            }
            message.extend_from_slice(chunk);
            match plain[0] {
                FLAG_LAST => return Ok(decode(&message)?),
                FLAG_MORE => continue,
                _ => return Err(ChannelError::Violation("bad chunk flag")),
            }
        }
    }
}

async fn write_frames<W: AsyncWrite + Unpin>(w: &mut W, frames: &[&[u8]]) -> io::Result<()> {
    let mut wire = Vec::with_capacity(frames.iter().map(|f| f.len() + 2).sum());
    for f in frames {
        debug_assert!(!f.is_empty() && f.len() <= MAX_FRAME);
        wire.extend_from_slice(&(f.len() as u16).to_be_bytes());
        wire.extend_from_slice(f);
    }
    w.write_all(&wire).await?;
    w.flush().await
}

async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>, ChannelError> {
    let eof = |e: io::Error| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            ChannelError::Closed
        } else {
            ChannelError::Io(e)
        }
    };
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await.map_err(eof)?;
    let n = u16::from_be_bytes(len) as usize;
    if n == 0 {
        return Err(ChannelError::Violation("empty frame"));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).await.map_err(eof)?;
    Ok(buf)
}
