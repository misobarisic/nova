//! A bounded, capability-addressed loopback relay for FlixCloud's wrapped HLS.
//! Only resolver-created resources are addressable; clients cannot supply URLs.
use crate::{ProviderHost, ProviderHostError, ScopedHttpHost};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use url::Url;

static SESSIONS: AtomicUsize = AtomicUsize::new(0);
const MAX_BODY: usize = 16 * 1024 * 1024;
const MAX_RESOURCES: usize = 4096;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct FlixcloudPlayback {
    pub mask: [u8; 16],
    pub w_payload: String,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Playlist,
    Segment,
    Key,
}
#[derive(Clone)]
struct Resource {
    url: Url,
    kind: Kind,
}
struct Session {
    root: String,
    resources: Mutex<Vec<Resource>>,
    domains: Vec<String>,
    spec: FlixcloudPlayback,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}
struct Slot;
impl Drop for Slot {
    fn drop(&mut self) {
        SESSIONS.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) fn start(
    url: &str,
    spec: FlixcloudPlayback,
    domains: Vec<String>,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<String, ProviderHostError> {
    let invalid = || ProviderHostError("invalid FlixCloud playback request".into());
    let url = Url::parse(url).map_err(|_| invalid())?;
    if spec.w_payload.is_empty()
        || spec.w_payload.len() > 8192
        || !domains.iter().any(|d| d == "enc-dec.app")
        || !domains.iter().any(|d| d == "flixcloud.cc")
    {
        return Err(invalid());
    }
    let policy = ScopedHttpHost::new(
        crate::host::session_state(),
        domains.clone(),
        Duration::from_secs(20),
        MAX_BODY,
        3,
        4,
    );
    policy.check_url(&url, Instant::now() + Duration::from_secs(10))?;
    SESSIONS
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            (n < 4).then_some(n + 1)
        })
        .map_err(|_| ProviderHostError("playback relay limit reached".into()))?;
    let slot = Slot;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|_| invalid())?;
    listener.set_nonblocking(true).map_err(|_| invalid())?;
    let mut entropy = [0u8; 24];
    aws_lc_rs::rand::fill(&mut entropy).map_err(|_| invalid())?;
    use base64::Engine;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(entropy);
    let root = format!(
        "http://127.0.0.1:{}/{token}/",
        listener.local_addr().map_err(|_| invalid())?.port()
    );
    let session = Arc::new(Session {
        root: root.clone(),
        resources: Mutex::new(vec![Resource {
            url,
            kind: Kind::Playlist,
        }]),
        domains,
        spec,
        cancelled,
    });
    std::thread::Builder::new().name("provider-hls".into()).spawn(move || {
        let _slot = slot;
        let created = Instant::now();
        let mut last = Instant::now();
        let readers = Arc::new(AtomicUsize::new(0));
        while created.elapsed() < Duration::from_secs(6 * 3600)
            && last.elapsed() < Duration::from_secs(600) && !(session.cancelled)() {
            match listener.accept() {
                Ok((mut socket, peer)) if peer.ip().is_loopback() => {
                    if readers.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| (n < 4).then_some(n + 1)).is_err() {
                        let _ = socket.write_all(b"HTTP/1.1 503 Busy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        continue;
                    }
                    last = Instant::now();
                    let session = session.clone();
                    let readers = readers.clone();
                    std::thread::spawn(move || {
                        let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
                        let _ = socket.set_write_timeout(Some(Duration::from_secs(20)));
                        if session.serve(&mut socket).is_err() {
                            let _ = socket.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        }
                        readers.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::park_timeout(Duration::from_millis(25)),
                _ => break,
            }
        }
    }).map_err(|_| invalid())?;
    Ok(format!("{root}0.m3u8"))
}

impl Session {
    fn resource(&self, url: Url, kind: Kind) -> Result<String, ProviderHostError> {
        // DNS/public-address validation is repeated and pinned by the HTTP host
        // at fetch time. Reject undeclared origins before publishing local URLs.
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some_and(|p| p != 443)
            || !self
                .domains
                .iter()
                .any(|d| crate::host::domain_matches(url.host_str().unwrap_or(""), d))
        {
            return Err(ProviderHostError("undeclared playlist origin".into()));
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| ProviderHostError("relay unavailable".into()))?;
        let index = if let Some(index) = resources
            .iter()
            .position(|r| r.url == url && r.kind == kind)
        {
            index
        } else {
            if resources.len() >= MAX_RESOURCES {
                return Err(ProviderHostError("playlist resource limit reached".into()));
            }
            resources.push(Resource { url, kind });
            resources.len() - 1
        };
        Ok(format!(
            "{}{index}{}",
            self.root,
            if kind == Kind::Playlist { ".m3u8" } else { "" }
        ))
    }
    fn child(&self, parent: &Url, raw: &str, kind: Kind) -> Result<String, ProviderHostError> {
        let mut url = parent
            .join(raw)
            .map_err(|_| ProviderHostError("invalid playlist URL".into()))?;
        if url.host_str() == parent.host_str()
            && !url.query_pairs().any(|(k, _)| k == "token")
            && let Some((_, token)) = parent.query_pairs().find(|(k, _)| k == "token")
        {
            url.query_pairs_mut().append_pair("token", &token);
        }
        self.resource(url, kind)
    }
    fn playlist(&self, parent: &Url, body: &str) -> Result<Vec<u8>, ProviderHostError> {
        if !body.trim_start().starts_with("#EXTM3U") {
            return Err(ProviderHostError("invalid HLS playlist".into()));
        }
        let uri = regex::Regex::new(r#"URI="([^"]+)""#).expect("static regex");
        let mut output = String::new();
        let mut variant = false;
        for line in body.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                let kind =
                    if line.starts_with("#EXT-X-KEY:") || line.starts_with("#EXT-X-SESSION-KEY:") {
                        Kind::Key
                    } else if line.starts_with("#EXT-X-MEDIA:")
                        || line.starts_with("#EXT-X-I-FRAME-STREAM-INF:")
                    {
                        Kind::Playlist
                    } else {
                        Kind::Segment
                    };
                let mut end = 0;
                for capture in uri.captures_iter(line) {
                    let all = capture.get(0).expect("whole match");
                    output.push_str(&line[end..all.start()]);
                    output.push_str("URI=\"");
                    output.push_str(&self.child(parent, &capture[1], kind)?);
                    output.push('"');
                    end = all.end();
                }
                output.push_str(&line[end..]);
                if line.starts_with("#EXT-X-STREAM-INF:") {
                    variant = true;
                }
            } else if !line.is_empty() {
                let kind = if variant || line.split('?').next().unwrap_or("").ends_with(".m3u8") {
                    Kind::Playlist
                } else {
                    Kind::Segment
                };
                output.push_str(&self.child(parent, line, kind)?);
                variant = false;
            }
            output.push('\n');
            if output.len() > 2 * 1024 * 1024 {
                return Err(ProviderHostError("playlist too large".into()));
            }
        }
        Ok(output.into_bytes())
    }
    fn serve(&self, socket: &mut TcpStream) -> Result<(), ProviderHostError> {
        let invalid = || ProviderHostError("relay request failed".into());
        let mut request = Vec::new();
        while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if socket.read(&mut byte).map_err(|_| invalid())? == 0 {
                return Err(invalid());
            }
            request.push(byte[0]);
        }
        let request = std::str::from_utf8(&request).map_err(|_| invalid())?;
        let mut parts = request.lines().next().unwrap_or("").split_whitespace();
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("");
        let prefix = Url::parse(&self.root)
            .map_err(|_| invalid())?
            .path()
            .to_owned();
        let index = target
            .strip_prefix(&prefix)
            .and_then(|s| s.strip_suffix(".m3u8").or(Some(s)))
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(invalid)?;
        if !matches!(method, "GET" | "HEAD") || (self.cancelled)() {
            return Err(invalid());
        }
        let resource = self
            .resources
            .lock()
            .map_err(|_| invalid())?
            .get(index)
            .cloned()
            .ok_or_else(invalid)?;
        let mut target = resource.url.clone();
        let is_master = resource.kind == Kind::Playlist
            && (target.path().contains("master.m3u8")
                || (target.host_str() == Some("flixcloud.cc")
                    && !target.path().contains("audio.m3u8")
                    && !target.path().contains("video.m3u8")));
        if is_master {
            target = Url::parse("https://enc-dec.app/api/parse-flixcloud").expect("static URL");
            target
                .query_pairs_mut()
                .append_pair("url", resource.url.as_str())
                .append_pair("w_payload", &self.spec.w_payload);
        }
        let origin = if target.host_str() == Some("enc-dec.app") {
            "https://enc-dec.app"
        } else {
            "https://flixcloud.cc"
        };
        let headers = BTreeMap::from([
            ("Referer".into(), format!("{origin}/")),
            ("Origin".into(), origin.into()),
        ]);
        let host = ScopedHttpHost::new(
            crate::host::session_state(),
            self.domains.clone(),
            Duration::from_secs(20),
            MAX_BODY,
            3,
            4,
        )
        .with_cancellation(self.cancelled.clone());
        let response = host.get(target.as_str(), &headers)?;
        if response.status != 200 {
            return Err(invalid());
        }
        let mut body = response.body;
        let content_type = match resource.kind {
            Kind::Playlist => {
                let mut parent = resource.url.clone();
                // The decoding service returns nested playlist URLs. Relative
                // media/key URLs still belong to the original CDN playlist.
                for _ in 0..3 {
                    if parent.host_str() != Some("enc-dec.app") {
                        break;
                    }
                    let nested = parent
                        .query_pairs()
                        .find(|(key, _)| key == "url")
                        .and_then(|(_, value)| Url::parse(&value).ok());
                    let Some(nested) = nested else {
                        break;
                    };
                    parent = nested;
                }
                body =
                    self.playlist(&parent, std::str::from_utf8(&body).map_err(|_| invalid())?)?;
                "application/vnd.apple.mpegurl"
            }
            Kind::Segment => {
                decode_segment(&mut body, &self.spec.mask);
                "application/octet-stream"
            }
            Kind::Key => "application/octet-stream",
        };
        // Decode the full segment before applying ranges so the XOR offset and
        // stripped header remain correct for byte-range HLS and player retries.
        let range = request.lines().find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("range"))
                .map(|(_, v)| v.trim())
        });
        let (status, extra, bytes) = if let Some(range) = range {
            let (start, end) = byte_range(range, body.len()).ok_or_else(invalid)?;
            (
                "206 Partial Content",
                format!("Content-Range: bytes {start}-{end}/{}\r\n", body.len()),
                &body[start..=end],
            )
        } else {
            ("200 OK", String::new(), body.as_slice())
        };
        write!(socket, "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\n{extra}Connection: close\r\n\r\n", bytes.len()).map_err(|_| invalid())?;
        if method == "GET" {
            socket.write_all(bytes).map_err(|_| invalid())?;
        }
        Ok(())
    }
}
fn byte_range(raw: &str, length: usize) -> Option<(usize, usize)> {
    let (start, end) = raw.strip_prefix("bytes=")?.split_once('-')?;
    if length == 0 {
        return None;
    }
    let (start, end) = if start.is_empty() {
        (
            length.saturating_sub(end.parse::<usize>().ok()?),
            length - 1,
        )
    } else {
        (
            start.parse().ok()?,
            if end.is_empty() {
                length - 1
            } else {
                end.parse::<usize>().ok()?.min(length - 1)
            },
        )
    };
    (start <= end && end < length).then_some((start, end))
}
fn decode_segment(body: &mut Vec<u8>, mask: &[u8; 16]) {
    let skip = if body.starts_with(b"\x89PNG\r\n\x1a\n") {
        8
    } else if body.len() >= 12 && &body[..4] == b"RIFF" && &body[8..12] == b"WEBP" {
        12
    } else {
        return;
    };
    body.drain(..skip);
    for (i, byte) in body.iter_mut().enumerate() {
        *byte ^= mask[i % 16];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrapped_and_plain_segments() {
        let mask = [37; 16];
        let plain: Vec<u8> = (0..200).collect();
        for header in [b"\x89PNG\r\n\x1a\n".as_slice(), b"RIFF1234WEBP".as_slice()] {
            let mut wrapped = header.to_vec();
            wrapped.extend(plain.iter().map(|b| b ^ 37));
            decode_segment(&mut wrapped, &mask);
            assert_eq!(wrapped, plain);
        }
        let mut body = plain.clone();
        decode_segment(&mut body, &mask);
        assert_eq!(body, plain);
    }
    #[test]
    fn playlist_rewrites_every_resource_and_scopes_tokens() {
        let session = Session {
            root: "http://127.0.0.1:1234/secret/".into(),
            resources: Mutex::new(vec![]),
            domains: vec!["flixcloud.cc".into()],
            spec: FlixcloudPlayback {
                mask: [0; 16],
                w_payload: "private".into(),
            },
            cancelled: Arc::new(|| false),
        };
        let parent = Url::parse("https://flixcloud.cc/hls/video.m3u8?token=signed").unwrap();
        let result = String::from_utf8(session.playlist(&parent, "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXT-X-MAP:URI=\"init.mp4\"\nsegment.ts\n").unwrap()).unwrap();
        assert!(!result.contains("signed"));
        assert!(result.contains("URI=\"http://127.0.0.1:1234/secret/0\""));
        let resources = session.resources.lock().unwrap();
        assert_eq!(resources.len(), 3);
        assert_eq!(resources[0].url.query(), Some("token=signed"));
        drop(resources);
        assert!(
            session
                .playlist(&parent, "#EXTM3U\nhttps://localhost/private\n")
                .is_err()
        );
        assert!(
            session
                .playlist(&parent, "#EXTM3U\nhttps://evil.example/file\n")
                .is_err()
        );
    }
    #[test]
    fn ranges_after_decoding() {
        assert_eq!(byte_range("bytes=2-4", 10), Some((2, 4)));
        assert_eq!(byte_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(byte_range("bytes=20-", 10), None);
        assert_eq!(byte_range("bytes=0-2,4-5", 10), None);
    }

    #[test]
    #[ignore = "requires NOVA_REANIME_LIVE_INPUT and NOVA_REANIME_LIVE_PACKAGE"]
    fn live_flixcloud_relay() {
        let path = std::env::var("NOVA_REANIME_LIVE_INPUT").expect("live input path");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let spec = serde_json::from_value(value["hlsProxy"].clone()).unwrap();
        let package = std::env::var("NOVA_REANIME_LIVE_PACKAGE").expect("live package path");
        let package = crate::AddonPackage::parse(&std::fs::read(package).unwrap()).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled = stop.clone();
        let local = start(
            value["url"].as_str().unwrap(),
            spec,
            package.plugin.permissions.domains.clone(),
            Arc::new(move || cancelled.load(Ordering::Relaxed)),
        )
        .unwrap();
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let mut current = local.clone();
        let mut encryption = None;
        let result = (|| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
            for _ in 0..4 {
                let body = client
                    .get(&current)
                    .send()?
                    .error_for_status()?
                    .bytes()?
                    .to_vec();
                if !body.starts_with(b"#EXTM3U") {
                    return Ok(body);
                }
                let text = std::str::from_utf8(&body)?;
                if let Some(line) = text
                    .lines()
                    .find(|l| l.starts_with("#EXT-X-KEY:METHOD=AES-128,"))
                {
                    let uri = regex::Regex::new(r#"URI="([^"]+)""#).unwrap();
                    let key_url = &uri.captures(line).ok_or("missing key URI")?[1];
                    let key = client
                        .get(key_url)
                        .send()?
                        .error_for_status()?
                        .bytes()?
                        .to_vec();
                    let iv = line
                        .split("IV=0x")
                        .nth(1)
                        .ok_or("missing IV")?
                        .split(',')
                        .next()
                        .unwrap();
                    if iv.len() != 32 {
                        return Err("invalid IV".into());
                    }
                    let iv: Vec<u8> = (0..32)
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&iv[i..i + 2], 16))
                        .collect::<Result<_, _>>()?;
                    encryption = Some((key, iv));
                }
                current = text
                    .lines()
                    .find(|l| !l.starts_with('#') && !l.trim().is_empty())
                    .ok_or("missing HLS resource")?
                    .into();
                assert!(current.starts_with("http://127.0.0.1:"));
            }
            Err("playlist nesting limit".into())
        })();
        let playback = std::env::var_os("NOVA_REANIME_MPV").map(|mpv| {
            std::process::Command::new(mpv)
                .args([
                    "--no-config",
                    "--vo=null",
                    "--ao=null",
                    "--audio=no",
                    "--frames=2",
                    "--ytdl=no",
                    "--network-timeout=20",
                    "--msg-level=all=warn",
                ])
                .arg(&local)
                .output()
                .expect("run mpv against the relay")
        });
        stop.store(true, Ordering::Relaxed);
        if let Some(playback) = playback {
            let urls = regex::Regex::new(r#"https?://[^\s]+"#).unwrap();
            let diagnostic = format!(
                "{}{}",
                String::from_utf8_lossy(&playback.stdout),
                String::from_utf8_lossy(&playback.stderr)
            );
            assert!(
                playback.status.success(),
                "mpv failed: {}",
                urls.replace_all(&diagnostic, "[URL]")
            );
        }
        let mut body = result.expect("relay serves a decoded segment");
        // mpv performs ordinary HLS AES-128 decryption after the relay removes
        // FlixCloud's wrapper. Verify that same final payload here.
        if let Some((key, iv)) = encryption {
            use aws_lc_rs::{
                cipher::{AES_128, DecryptionContext, PaddedBlockDecryptingKey, UnboundCipherKey},
                iv::FixedLength,
            };
            let key = UnboundCipherKey::new(&AES_128, &key).unwrap();
            let key = PaddedBlockDecryptingKey::cbc_pkcs7(key).unwrap();
            body = key
                .decrypt(
                    &mut body,
                    DecryptionContext::Iv128(FixedLength::try_from(iv.as_slice()).unwrap()),
                )
                .unwrap()
                .to_vec();
        }
        assert!(body.len() > 188 * 3);
        assert!(
            body[0] == 0x47 && body[188] == 0x47 && body[376] == 0x47,
            "decoded payload must contain MPEG-TS sync bytes"
        );
    }
}
