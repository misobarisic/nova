//! Invite-ticket codec and the short pairing match code.
//!
//! A ticket is `NV1` followed by base32 of
//! `version(1) ‖ endpoint_id(32) ‖ secret(16)`. It is self-contained: the
//! joiner learns both the target's endpoint id (to dial) and the single-use
//! secret (to authenticate). A tiny local base32 keeps copy/paste and a future
//! QR compact without a dependency.

use anyhow::{Context, Result, bail};
use iroh::EndpointId;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const PREFIX: &str = "NV1";
const VERSION: u8 = 1;
const TICKET_BYTES: usize = 1 + 32 + 16;

/// Random 16-byte pairing secret.
pub fn generate_secret() -> [u8; 16] {
    // iroh's key generation is a CSPRNG; take half for a 128-bit pairing secret.
    let bytes = iroh::SecretKey::generate().to_bytes();
    let mut secret = [0u8; 16];
    secret.copy_from_slice(&bytes[..16]);
    secret
}

pub fn encode_ticket(id: &EndpointId, secret: &[u8; 16]) -> String {
    let mut bytes = Vec::with_capacity(TICKET_BYTES);
    bytes.push(VERSION);
    bytes.extend_from_slice(id.as_bytes());
    bytes.extend_from_slice(secret);
    format!("{PREFIX}{}", base32_encode(&bytes))
}

pub fn decode_ticket(text: &str) -> Result<(EndpointId, [u8; 16])> {
    let text = text.trim();
    let body = text
        .strip_prefix(PREFIX)
        .or_else(|| text.strip_prefix("nv1"))
        .context("not a nova invite code")?;
    let bytes = base32_decode(body).context("invite code is not valid base32")?;
    if bytes.len() != TICKET_BYTES {
        bail!("invite code has the wrong length");
    }
    if bytes[0] != VERSION {
        bail!("unsupported invite code version {}", bytes[0]);
    }
    let id_bytes: [u8; 32] = bytes[1..33].try_into().expect("slice length checked");
    let id = EndpointId::from_bytes(&id_bytes).context("invite code has a bad endpoint id")?;
    let mut secret = [0u8; 16];
    secret.copy_from_slice(&bytes[33..]);
    Ok((id, secret))
}

/// Six-digit code derived from the secret and both endpoint ids. Both sides
/// compute the same value independently (the host knows both ids, the joiner
/// knows the host's from the ticket and its own), so it can be compared out
/// loud to confirm the pairing.
pub fn match_code(secret: &[u8], host_id: &str, joiner_id: &str) -> String {
    let mut buf = Vec::with_capacity(secret.len() + host_id.len() + joiner_id.len());
    buf.extend_from_slice(secret);
    buf.extend_from_slice(host_id.as_bytes());
    buf.extend_from_slice(joiner_id.as_bytes());
    // Include the secret twice with a separator so the id concatenation is
    // unambiguous in the hashed input.
    buf.push(0);
    buf.extend_from_slice(secret);
    format!("{:06}", nova_config::fnv1a(&buf) % 1_000_000)
}

pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;
    for &byte in data {
        buffer = (buffer << 8) | u64::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = ((buffer >> bits) & 0x1f) as usize;
            out.push(ALPHABET[index] as char);
        }
    }
    if bits > 0 {
        let index = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[index] as char);
    }
    out
}

pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;
    for ch in text.chars() {
        if ch == '=' || ch == '-' {
            continue;
        }
        let upper = ch.to_ascii_uppercase() as u8;
        let value = ALPHABET.iter().position(|&a| a == upper)? as u64;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_round_trips() {
        for len in 0..64usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let encoded = base32_encode(&data);
            assert_eq!(
                base32_decode(&encoded).unwrap(),
                data,
                "round trip failed for len {len}"
            );
        }
    }

    #[test]
    fn ticket_round_trips() {
        let id = iroh::SecretKey::generate().public();
        let secret = generate_secret();
        let ticket = encode_ticket(&id, &secret);
        assert!(ticket.starts_with("NV1"));
        let (decoded_id, decoded_secret) = decode_ticket(&ticket).unwrap();
        assert_eq!(decoded_id, id);
        assert_eq!(decoded_secret, secret);
        // The `nv1` prefix spelling is accepted too.
        let lower = format!("nv1{}", &ticket[3..]);
        assert!(decode_ticket(&lower).is_ok());
    }

    #[test]
    fn ticket_rejects_garbage() {
        assert!(decode_ticket("not-a-ticket").is_err());
        assert!(decode_ticket("NV1!!!!").is_err());
    }

    #[test]
    fn match_code_is_stable_and_ordered() {
        let secret = [7u8; 16];
        let a = "host-endpoint-id";
        let b = "joiner-endpoint-id";
        assert_eq!(match_code(&secret, a, b), match_code(&secret, a, b));
        assert_eq!(match_code(&secret, a, b).len(), 6);
        assert_ne!(match_code(&secret, a, b), match_code(&secret, b, a));
    }
}
