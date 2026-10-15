//! Copy/paste capability tickets. Secrets never enter logs or persisted state.
use anyhow::{Context, Result, ensure};
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

const PREFIX: &str = "NVP2";
const MAX_TICKET_BYTES: usize = 4_096;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Ticket {
    pub host: EndpointAddr,
    pub room_id: String,
    pub capability: [u8; 32],
    pub expires_unix: u64,
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn random_hex() -> String {
    encode_hex(&iroh::SecretKey::generate().to_bytes()[..16])
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        text.push(HEX[(b >> 4) as usize] as char);
        text.push(HEX[(b & 15) as usize] as char);
    }
    text
}

impl Ticket {
    pub fn encode(&self) -> Result<String> {
        let bytes = postcard::to_allocvec(self)?;
        ensure!(bytes.len() <= MAX_TICKET_BYTES, "party address too large");
        Ok(format!("{PREFIX}{}", encode_hex(&bytes)))
    }

    pub fn decode(text: &str) -> Result<Self> {
        let body = text
            .trim()
            .strip_prefix(PREFIX)
            .context("not a party invite")?;
        ensure!(
            body.len() <= MAX_TICKET_BYTES * 2 && body.len().is_multiple_of(2),
            "invalid party invite size"
        );
        let bytes: Result<Vec<u8>> = body
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let high = (pair[0] as char)
                    .to_digit(16)
                    .context("invalid invite character")?;
                let low = (pair[1] as char)
                    .to_digit(16)
                    .context("invalid invite character")?;
                Ok(((high << 4) | low) as u8)
            })
            .collect();
        let bytes = bytes?;
        let (ticket, trailing): (Self, &[u8]) = postcard::take_from_bytes(&bytes)?;
        ensure!(trailing.is_empty(), "trailing party invite data");
        ensure!(
            ticket.room_id.len() == 32 && ticket.room_id.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid room identifier"
        );
        let now = unix_now();
        ensure!(
            ticket.expires_unix > now && ticket.expires_unix <= now.saturating_add(3_900),
            "party invite expired or clock differs"
        );
        Ok(ticket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticket_bounds_expiry_and_roundtrip() {
        let ticket = Ticket {
            host: iroh::SecretKey::generate().public().into(),
            room_id: random_hex(),
            capability: [17; 32],
            expires_unix: unix_now() + 60,
        };
        let encoded = ticket.encode().unwrap();
        assert!(encoded.starts_with("NVP2"));
        assert!(Ticket::decode(&encoded.replacen("NVP2", "NVP1", 1)).is_err());
        let decoded = Ticket::decode(&encoded).unwrap();
        assert_eq!(decoded.capability, ticket.capability);
        assert_eq!(decoded.host, ticket.host);
        assert!(Ticket::decode(&(encoded.clone() + "00")).is_err());
        assert!(Ticket::decode(&format!("NVP2{}", "a".repeat(10_000))).is_err());
        assert!(
            Ticket::decode(
                &Ticket {
                    expires_unix: unix_now().saturating_sub(1),
                    ..ticket
                }
                .encode()
                .unwrap()
            )
            .is_err()
        );
    }
}
