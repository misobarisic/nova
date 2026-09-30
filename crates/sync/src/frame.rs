//! Length-prefixed postcard framing shared by the sync and pairing protocols.
//!
//! postcard is a compact, non-self-describing binary serde format. Because it
//! is not self-describing, wire structs must not rely on `#[serde(default)]`:
//! every field is always present. Schema changes therefore require a protocol
//! version bump (the sync ALPN is `/3`).
//!
//! Two framings exist: [`write_frame`]/[`read_frame`] (raw postcard, used by
//! the pairing and removal protocols) and [`write_frame_c`]/[`read_frame_c`]
//! (a codec byte plus optional deflate, used by the sync protocol so its large
//! digests and record batches compress on the wire). Keeping them separate
//! means the pairing/removal wire schema is untouched by compression.

use std::borrow::Cow;
use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use iroh::endpoint::{ReadExactError, RecvStream, SendStream};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Upper bound for a single frame. Record sets are a personal library plus
/// watch history; pairing frames are tiny.
pub(crate) const MAX_FRAME: usize = 32 * 1024 * 1024;

/// Frame codec byte: the body is postcard as-is.
const CODEC_RAW: u8 = 0;
/// Frame codec byte: the body is a deflate-compressed postcard frame.
const CODEC_DEFLATE: u8 = 1;

/// Bodies below this are cheaper to send raw than to pay the deflate framing
/// and per-frame compression overhead.
const COMPRESS_THRESHOLD: usize = 256;

/// Write one `msg` as a 4-byte big-endian length followed by its postcard
/// bytes.
pub(crate) async fn write_frame<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<()> {
    let bytes = postcard::to_allocvec(msg).context("encode frame")?;
    if bytes.len() > MAX_FRAME {
        bail!("frame too large: {} bytes", bytes.len());
    }
    send.write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .context("write frame length")?;
    send.write_all(&bytes).await.context("write frame body")?;
    Ok(())
}

/// Read one frame, or `None` when the peer finished the stream cleanly.
pub(crate) async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(()) => {}
        Err(ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(ReadExactError::FinishedEarly(_)) => bail!("truncated frame header"),
        Err(ReadExactError::ReadError(e)) => return Err(e.into()),
    }
    let n = u32::from_be_bytes(len) as usize;
    if n == 0 || n > MAX_FRAME {
        bail!("bad frame length {n}");
    }
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf).await.context("read frame body")?;
    decode(&buf)
}

/// Write one `msg` using the compressible framing: a 4-byte big-endian length,
/// a codec byte, then the body. Bodies at or above [`COMPRESS_THRESHOLD`] are
/// deflated when that actually shrinks them; otherwise they stay raw.
///
/// Only the sync protocol uses this, so a compressed frame never reaches a
/// pairing/removal peer.
pub(crate) async fn write_frame_c<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<()> {
    let raw = postcard::to_allocvec(msg).context("encode frame")?;
    if raw.len() > MAX_FRAME {
        bail!("frame too large: {} bytes", raw.len());
    }
    let (codec, body): (u8, Cow<'_, [u8]>) = if raw.len() >= COMPRESS_THRESHOLD {
        let deflated = deflate(&raw)?;
        if deflated.len() < raw.len() {
            (CODEC_DEFLATE, Cow::Owned(deflated))
        } else {
            (CODEC_RAW, Cow::Borrowed(&raw))
        }
    } else {
        (CODEC_RAW, Cow::Borrowed(&raw))
    };
    // The codec byte counts toward the frame length.
    let total = body.len() + 1;
    if total > MAX_FRAME {
        bail!("frame too large: {total} bytes");
    }
    let mut header = [0u8; 5];
    header[..4].copy_from_slice(&(total as u32).to_be_bytes());
    header[4] = codec;
    send.write_all(&header)
        .await
        .context("write frame header")?;
    send.write_all(&body).await.context("write frame body")?;
    Ok(())
}

/// Read one frame written by [`write_frame_c`], or `None` on clean stream end.
pub(crate) async fn read_frame_c<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(()) => {}
        Err(ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(ReadExactError::FinishedEarly(_)) => bail!("truncated frame header"),
        Err(ReadExactError::ReadError(e)) => return Err(e.into()),
    }
    let n = u32::from_be_bytes(len) as usize;
    if n == 0 || n > MAX_FRAME {
        bail!("bad frame length {n}");
    }
    let mut codec = [0u8; 1];
    recv.read_exact(&mut codec)
        .await
        .context("read frame codec")?;
    let mut body = vec![0u8; n - 1];
    recv.read_exact(&mut body)
        .await
        .context("read frame body")?;
    match codec[0] {
        CODEC_RAW => decode(&body),
        // A hostile/buggy peer could inflate a small frame into a huge one;
        // `inflate` caps the decompressed size at `MAX_FRAME`.
        CODEC_DEFLATE => decode(&inflate(&body)?),
        other => bail!("unknown frame codec {other}"),
    }
}

fn deflate(raw: &[u8]) -> Result<Vec<u8>> {
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
    enc.write_all(raw).context("deflate frame")?;
    enc.finish().context("finish deflate frame")
}

fn inflate(body: &[u8]) -> Result<Vec<u8>> {
    // `take` bounds the output so a decompression bomb cannot allocate past
    // `MAX_FRAME`; reading one extra byte lets us tell "exactly at cap" from
    // "over cap".
    let mut out = Vec::new();
    DeflateDecoder::new(body)
        .take(MAX_FRAME as u64 + 1)
        .read_to_end(&mut out)
        .context("inflate frame")?;
    if out.len() > MAX_FRAME {
        bail!("frame too large after decompress: {} bytes", out.len());
    }
    Ok(out)
}

fn decode<T: DeserializeOwned>(raw: &[u8]) -> Result<Option<T>> {
    let (msg, rest) = postcard::take_from_bytes(raw).context("decode frame")?;
    if !rest.is_empty() {
        bail!("trailing bytes after frame");
    }
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deflate_round_trips() {
        let raw = b"abcdef".repeat(1000);
        let packed = deflate(&raw).unwrap();
        assert!(packed.len() < raw.len());
        assert_eq!(inflate(&packed).unwrap(), raw);
    }

    #[test]
    fn inflate_rejects_oversized_output() {
        // A small compressed body that inflates past MAX_FRAME must be
        // rejected rather than allocated.
        let bomb = deflate(&vec![0u8; MAX_FRAME + 1]).unwrap();
        assert!(bomb.len() < MAX_FRAME);
        let err = inflate(&bomb).unwrap_err();
        assert!(err.to_string().contains("too large after decompress"));
    }
}
