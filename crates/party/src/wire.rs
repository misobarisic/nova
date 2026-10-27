//! One bounded postcard frame and FIN per bidirectional request stream.
use crate::{ControlRejectionReason, HostCommand, LocalReport, PartyMedia, RoomSnapshot};
use anyhow::{Context, Result, ensure};
use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub(crate) const MAX_FRAME: usize = 16 * 1_024;

#[derive(Serialize, Deserialize)]
pub(crate) struct Request {
    pub room_id: String,
    pub capability: [u8; 32],
    pub sequence: u64,
    pub operation: Operation,
}

#[derive(Serialize, Deserialize)]
pub(crate) enum Operation {
    Join {
        name: String,
    },
    Report(LocalReport),
    Leave,
    Control {
        generation: u64,
        control_revision: u64,
        intent: ControlIntent,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum ControlIntent {
    Playback(HostCommand),
    ProposeMedia(PartyMedia),
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Response {
    pub sequence: u64,
    pub host_now_ms: u64,
    pub snapshot: Option<RoomSnapshot>,
    pub error: Option<String>,
    pub terminal: bool,
    pub rejection: Option<ControlRejectionReason>,
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_FRAME,
        "invalid party frame size"
    );
    let (value, trailing) = postcard::take_from_bytes(bytes).context("decode party frame")?;
    ensure!(trailing.is_empty(), "trailing party frame data");
    Ok(value)
}

pub(crate) async fn read<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<T> {
    let bytes = recv
        .read_to_end(MAX_FRAME)
        .await
        .context("read party frame")?;
    decode(&bytes)
}

pub(crate) async fn write<T: Serialize>(send: &mut SendStream, value: &T) -> Result<()> {
    let bytes = postcard::to_allocvec(value)?;
    ensure!(bytes.len() <= MAX_FRAME, "party frame too large");
    send.write_all(&bytes).await?;
    send.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_empty_truncated_oversized_and_trailing_frames() {
        assert!(decode::<Response>(&[]).is_err());
        assert!(decode::<Response>(&[0; MAX_FRAME + 1]).is_err());
        let value = Response {
            sequence: 9,
            host_now_ms: 42,
            snapshot: None,
            error: None,
            terminal: false,
            rejection: None,
        };
        let mut bytes = postcard::to_allocvec(&value).unwrap();
        assert!(decode::<Response>(&bytes).is_ok());
        bytes.pop();
        assert!(decode::<Response>(&bytes).is_err());
        let mut bytes = postcard::to_allocvec(&value).unwrap();
        bytes.push(0);
        assert!(decode::<Response>(&bytes).is_err());
    }
}
