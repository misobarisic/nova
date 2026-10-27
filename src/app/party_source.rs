//! Exact, non-reversible source hints. A hint can label only an existing local
//! result; it never creates a source or grants access to an addon.
use super::*;
use nova_party::{RoomSnapshot, SourceFingerprint};
use nova_playback_api::MediaIdentity;
use sha2::{Digest, Sha256};

const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;

pub(super) fn same_media(a: &MediaIdentity, b: &MediaIdentity) -> bool {
    !a.title_id.is_empty()
        && !a.episode_id.is_empty()
        && a.media_type == b.media_type
        && a.title_id == b.title_id
        && a.episode_id == b.episode_id
}

/// Prefer the same broker-confirmed episode alias used by the party timeline.
/// Display names and inferred release families provide no source identity proof.
pub(super) fn canonical_media(mut identity: MediaIdentity, videos: &[Video]) -> MediaIdentity {
    if let Some(video) = videos.iter().find(|v| v.id == identity.episode_id) {
        let season = video.season;
        let episode = video.episode.or(video.number);
        if let Some(id) = confirmed_episode_stream_ids(video)
            .into_iter()
            .find(|id| id.starts_with("tt"))
        {
            let pieces = id.rsplitn(3, ':').collect::<Vec<_>>();
            if pieces.len() == 3
                && pieces[0].parse::<u32>().ok() == episode
                && pieces[1].parse::<u32>().ok() == season
            {
                identity.title_id = pieces[2].into();
                identity.episode_id = id;
            }
        }
    }
    identity
}

fn hash_part(hash: &mut Sha256, part: &[u8]) {
    hash.update((part.len() as u64).to_le_bytes());
    hash.update(part);
}

fn torrent_hash(input: &str) -> Option<Vec<u8>> {
    if input.len() > MAX_URL_BYTES {
        return None;
    }
    let input = input.trim();
    let magnet;
    let value = if input.starts_with("magnet:") {
        magnet = url::Url::parse(input).ok()?;
        let hashes: Vec<_> = magnet
            .query_pairs()
            .filter(|(key, value)| key == "xt" && value.starts_with("urn:btih:"))
            .map(|(_, value)| value.into_owned())
            .collect();
        if hashes.len() != 1 {
            return None;
        }
        return torrent_hash(&hashes[0]);
    } else {
        input.strip_prefix("urn:btih:").unwrap_or(input)
    };
    if matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return value
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                Some(
                    (pair[0] as char).to_digit(16)? as u8 * 16
                        + (pair[1] as char).to_digit(16)? as u8,
                )
            })
            .collect();
    }
    // A v1 infohash can also use the standard 32-character base32 spelling.
    if value.len() != 32 {
        return None;
    }
    let mut bytes = Vec::with_capacity(20);
    let mut buffer = 0u32;
    let mut bits = 0;
    for b in value.bytes().map(|b| b.to_ascii_uppercase()) {
        let digit = match b {
            b'A'..=b'Z' => b - b'A',
            b'2'..=b'7' => b - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | digit as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(bytes)
}

pub(super) fn fingerprint(source: &StreamSource) -> Option<SourceFingerprint> {
    let mut hash = Sha256::new();
    hash_part(&mut hash, b"nova/party-source/1");
    match source {
        StreamSource::Torrent {
            info_hash,
            file_idx: Some(index),
        } => {
            hash_part(&mut hash, b"torrent");
            hash_part(&mut hash, &torrent_hash(info_hash)?);
            hash_part(&mut hash, &index.to_le_bytes());
        }
        StreamSource::Url(url) => hash_url(&mut hash, url, &[])?,
        StreamSource::UrlWithOptions { url, headers, .. } => hash_url(&mut hash, url, headers)?,
        // A provider token may describe a menu action, temporary capability or
        // addon-specific row. No stable asset identity is proven by this type.
        // Likewise a local path/job ID cannot prove cross-device file identity.
        _ => return None,
    }
    Some(hash.finalize().into())
}

fn hash_url(hash: &mut Sha256, value: &str, headers: &[(String, String)]) -> Option<()> {
    if value.len() > MAX_URL_BYTES
        || headers.len() > 32
        || headers
            .iter()
            .map(|(k, v)| k.len().saturating_add(v.len()))
            .sum::<usize>()
            > MAX_HEADER_BYTES
    {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    // Device-relative endpoints can name different files with identical text
    // on two participants (for example two local media proxies on port 8080).
    match url.host()? {
        url::Host::Domain(host) if host == "localhost" || host.ends_with(".localhost") => {
            return None;
        }
        url::Host::Ipv4(ip) if ip.is_loopback() || ip.is_unspecified() || ip.is_link_local() => {
            return None;
        }
        url::Host::Ipv6(ip)
            if ip.is_loopback() || ip.is_unspecified() || ip.is_unicast_link_local() =>
        {
            return None;
        }
        _ => {}
    }
    hash_part(hash, b"http");
    // Preserve signed queries and header order byte-for-byte. Different signed
    // URLs deliberately do not match even when their display titles agree.
    hash_part(hash, value.as_bytes());
    hash_part(hash, &(headers.len() as u64).to_le_bytes());
    for (key, value) in headers {
        hash_part(hash, key.as_bytes());
        hash_part(hash, value.as_bytes());
    }
    Some(())
}

pub(super) fn recommended(
    room: Option<&RoomSnapshot>,
    media: &MediaIdentity,
    source: &StreamSource,
) -> bool {
    let Some(room) = room.filter(|room| room.connected && !room.ended) else {
        return false;
    };
    if !room
        .media
        .as_ref()
        .is_some_and(|expected| same_media(&expected.identity, media))
    {
        return false;
    }
    let Some(fingerprint) = fingerprint(source) else {
        return false;
    };
    room.participants
        .iter()
        .any(|p| p.id != room.member_id && p.connected && p.source_fingerprint == Some(fingerprint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_party::{Participant, PartyMedia, PartyRole, Timeline};
    fn media() -> MediaIdentity {
        MediaIdentity {
            media_type: "series".into(),
            title_id: "tt42".into(),
            episode_id: "tt42:1:2".into(),
            ..Default::default()
        }
    }
    fn room(fingerprint: SourceFingerprint) -> RoomSnapshot {
        RoomSnapshot {
            pending_media: None,
            role: PartyRole::Guest,
            room_id: "room".into(),
            host_id: "host".into(),
            member_id: "self".into(),
            media: Some(PartyMedia {
                identity: media(),
                season: Some(1),
                episode: Some(2),
                duration: 600.0,
            }),
            generation: 2,
            revision: 1,
            timeline: Timeline {
                position: 0.0,
                anchor_ms: 0,
                paused: true,
            },
            participants: vec![Participant {
                id: "host".into(),
                name: "Host".into(),
                is_host: true,
                connected: true,
                ready: true,
                buffering: false,
                position: 0.0,
                buffered_ahead: 2.0,
                reason: None,
                source_fingerprint: Some(fingerprint),
            }],
            waiting: Vec::new(),
            wait_for_guests: true,
            connected: true,
            ended: false,
            error: None,
            clock_offset_ms: 0.0,
        }
    }
    #[test]
    fn exact_torrent_asset_matches_across_addons_and_hash_spellings_but_not_files() {
        let source = |hash: &str, index| StreamSource::Torrent {
            info_hash: hash.into(),
            file_idx: index,
        };
        let a = source(&"A0".repeat(20), Some(2));
        let b = source(&"a0".repeat(20), Some(2));
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_ne!(
            fingerprint(&a),
            fingerprint(&source(&"a0".repeat(20), Some(3)))
        );
        assert!(fingerprint(&source(&"a0".repeat(20), None)).is_none());
        assert_eq!(
            fingerprint(&source(&"00".repeat(20), Some(0))),
            fingerprint(&source(&"A".repeat(32), Some(0)))
        );
        assert_eq!(
            fingerprint(&a),
            fingerprint(&source(
                &format!("magnet:?xt=urn:btih:{}&dn=ignored", "a0".repeat(20)),
                Some(2)
            ))
        );
        assert!(fingerprint(&source("not-an-infohash", Some(2))).is_none());
    }
    #[test]
    fn exact_http_url_and_headers_match_and_no_credentials_enter_report() {
        let url = "https://media.example/video.mkv?signature=secret-signature";
        let plain = StreamSource::Url(url.into());
        let options = StreamSource::UrlWithOptions {
            url: url.into(),
            headers: Vec::new(),
            subtitles: vec!["https://local/subtitle".into()],
        };
        assert_eq!(fingerprint(&plain), fingerprint(&options));
        let auth = StreamSource::UrlWithOptions {
            url: url.into(),
            headers: vec![("Authorization".into(), "Bearer private-token".into())],
            subtitles: Vec::new(),
        };
        assert_ne!(fingerprint(&plain), fingerprint(&auth));
        let different = StreamSource::Url(url.replace("secret-signature", "different-signature"));
        assert_ne!(fingerprint(&plain), fingerprint(&different));
        assert!(fingerprint(&StreamSource::Url("http://127.0.0.1:8080/video".into())).is_none());
        assert!(fingerprint(&StreamSource::Url("http://localhost:8080/video".into())).is_none());
        for relative in [
            "file:///video.mkv",
            "http://proxy.localhost/video",
            "http://[::1]:8080/video",
            "http://169.254.1.2/video",
            "http://[fe80::1]/video",
        ] {
            assert!(
                fingerprint(&StreamSource::Url(relative.into())).is_none(),
                "device-relative source {relative}"
            );
        }
        assert!(
            fingerprint(&StreamSource::Url(
                "http://192.168.1.20/shared/movie.mkv".into()
            ))
            .is_some()
        );
        assert!(
            fingerprint(&StreamSource::Url(
                "http://shared-nas.local/movie.mkv".into()
            ))
            .is_some()
        );
        let report = nova_party::LocalReport {
            source_fingerprint: fingerprint(&auth),
            media: Some(media()),
            ..Default::default()
        };
        let wire = serde_json::to_string(&report).unwrap();
        for private in [url, "signature", "private-token", "Authorization", "Bearer"] {
            assert!(!wire.contains(private));
        }
        assert!(
            fingerprint(&StreamSource::Url(
                "https://example.org/".to_owned() + &"x".repeat(MAX_URL_BYTES)
            ))
            .is_none()
        );
        assert!(
            fingerprint(&StreamSource::Resolver {
                provider_url: "owner".into(),
                token: "asset-id".into(),
                settings_generation: 1,
                addon_generation: 1
            })
            .is_none()
        );
    }
    #[test]
    fn only_another_connected_pick_for_this_media_can_recommend_an_existing_local_source() {
        let source = StreamSource::Url("https://example.org/exact-file".into());
        let mut room = room(fingerprint(&source).unwrap());
        assert!(recommended(Some(&room), &media(), &source));
        assert!(!recommended(
            Some(&room),
            &media(),
            &StreamSource::Url("https://example.org/other-file".into())
        ));
        let mut other = media();
        other.episode_id = "tt42:1:3".into();
        assert!(!recommended(Some(&room), &other, &source));
        room.participants[0].id = "self".into();
        assert!(!recommended(Some(&room), &media(), &source));
        room.participants[0].id = "host".into();
        room.participants[0].connected = false;
        assert!(!recommended(Some(&room), &media(), &source));
        room.participants[0].connected = true;
        room.connected = false;
        assert!(!recommended(Some(&room), &media(), &source));
        room.connected = true;
        room.ended = true;
        assert!(!recommended(Some(&room), &media(), &source));
        assert!(!recommended(None, &media(), &source));
    }
}
