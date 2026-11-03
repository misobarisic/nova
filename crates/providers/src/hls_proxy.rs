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
const ORPHAN_LIFETIME: Duration = Duration::from_secs(600);
const HARD_LIFETIME: Duration = Duration::from_secs(6 * 3600);

type RelayClock = Arc<dyn Fn() -> Duration + Send + Sync>;

struct LifecycleState {
    owned: bool,
    orphaned_at: Duration,
    ended: bool,
}

struct RelayLifecycle {
    clock: RelayClock,
    state: Mutex<LifecycleState>,
}

impl RelayLifecycle {
    fn new(clock: RelayClock) -> (Arc<Self>, PlaybackRelayLease) {
        let lifecycle = Arc::new(Self {
            clock,
            state: Mutex::new(LifecycleState {
                owned: true,
                orphaned_at: Duration::ZERO,
                ended: false,
            }),
        });
        let lease = PlaybackRelayLease(Arc::new(RelayOwner(lifecycle.clone())));
        (lifecycle, lease)
    }

    fn allows_serving(&self, cancelled: bool) -> bool {
        let now = (self.clock)();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.ended |= cancelled
            || now >= HARD_LIFETIME
            || (!state.owned && now.saturating_sub(state.orphaned_at) >= ORPHAN_LIFETIME);
        !state.ended
    }

    fn finish(&self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .ended = true;
    }
}

struct RelayOwner(Arc<RelayLifecycle>);
impl Drop for RelayOwner {
    fn drop(&mut self) {
        let now = (self.0.clock)();
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.owned = false;
        // Only final owner release starts the orphan deadline. Network activity
        // cannot keep a resolver-created orphan alive indefinitely.
        state.orphaned_at = now;
    }
}

/// Keeps a resolved provider relay available while its playback or download
/// owns it, including a paused player. Clones share one final-drop owner.
/// Cancellation and the six-hour hard limit remain authoritative.
#[derive(Clone)]
pub struct PlaybackRelayLease(Arc<RelayOwner>);

impl PlaybackRelayLease {
    /// A terminal relay must be resolved again; it cannot be revived by traffic.
    pub fn has_ended(&self) -> bool {
        !self.0.0.allows_serving(false)
    }
}

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
    lifecycle: Arc<RelayLifecycle>,
    #[cfg(test)]
    transport: Option<crate::host::TestTransport>,
}
struct ReaderSlot(Arc<AtomicUsize>);
impl Drop for ReaderSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
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
) -> Result<(String, PlaybackRelayLease), ProviderHostError> {
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
    let created = Instant::now();
    let (lifecycle, lease) = RelayLifecycle::new(Arc::new(move || created.elapsed()));
    let serving = lifecycle.clone();
    let cancelled: Arc<dyn Fn() -> bool + Send + Sync> =
        Arc::new(move || !serving.allows_serving(cancelled()));
    let session = Arc::new(Session {
        root: root.clone(),
        resources: Mutex::new(vec![Resource {
            url,
            kind: Kind::Playlist,
        }]),
        domains,
        spec,
        cancelled,
        lifecycle,
        #[cfg(test)]
        transport: None,
    });
    std::thread::Builder::new()
        .name("provider-hls".into())
        .spawn(move || {
            let _slot = slot;
            run_listener(session, listener);
        })
        .map_err(|_| invalid())?;
    Ok((format!("{root}0.m3u8"), lease))
}

fn run_listener(session: Arc<Session>, listener: TcpListener) {
    let readers = Arc::new(AtomicUsize::new(0));
    while !(session.cancelled)() {
        match listener.accept() {
            Ok((mut socket, peer)) if peer.ip().is_loopback() => {
                if readers
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                        (n < 4).then_some(n + 1)
                    })
                    .is_err()
                {
                    let _ = socket.write_all(
                        b"HTTP/1.1 503 Busy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                }
                let session = session.clone();
                let reader_slot = ReaderSlot(readers.clone());
                std::thread::spawn(move || {
                    let _reader_slot = reader_slot;
                    let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = socket.set_write_timeout(Some(Duration::from_secs(20)));
                    if session.serve(&mut socket).is_err() {
                        let _ = socket.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::park_timeout(Duration::from_millis(25))
            }
            _ => break,
        }
    }
    session.lifecycle.finish();
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
        const LIMIT: usize = 2 * 1024 * 1024;
        fn append(output: &mut String, value: &str) -> Result<(), ProviderHostError> {
            if value.len() > LIMIT - output.len() {
                return Err(ProviderHostError("playlist too large".into()));
            }
            output.push_str(value);
            Ok(())
        }
        // Fixed capacity prevents geometric growth and rejects before copying.
        let mut output = String::with_capacity(LIMIT);
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
                    append(&mut output, &line[end..all.start()])?;
                    append(&mut output, "URI=\"")?;
                    append(&mut output, &self.child(parent, &capture[1], kind)?)?;
                    append(&mut output, "\"")?;
                    end = all.end();
                }
                append(&mut output, &line[end..])?;
                if line.starts_with("#EXT-X-STREAM-INF:") {
                    variant = true;
                }
            } else if !line.is_empty() {
                let kind = if variant || line.split('?').next().unwrap_or("").ends_with(".m3u8") {
                    Kind::Playlist
                } else {
                    Kind::Segment
                };
                append(&mut output, &self.child(parent, line, kind)?)?;
                variant = false;
            }
            append(&mut output, "\n")?;
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
        let deadline = Instant::now() + Duration::from_secs(20);
        let limit = match resource.kind {
            Kind::Playlist => 2 * 1024 * 1024,
            Kind::Segment => MAX_BODY,
            Kind::Key => 64 * 1024,
        };
        // Resource lookup has released its lock. Admission precedes transport
        // body allocation and stays alive through decoding and downstream writes.
        let _body_permit = crate::host::PlaybackBodyPermit::acquire(
            limit,
            resource.kind == Kind::Playlist,
            deadline,
            &*self.cancelled,
        )?;
        #[allow(unused_mut)]
        let mut host = ScopedHttpHost::playback(
            self.domains.clone(),
            deadline,
            &_body_permit,
            self.cancelled.clone(),
        );
        #[cfg(test)]
        {
            host.test_transport = self.transport.clone();
        }
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
        let check_delivery = |socket: &TcpStream| -> Result<(), ProviderHostError> {
            if (self.cancelled)() {
                return Err(invalid());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(invalid());
            }
            socket
                .set_write_timeout(Some(remaining))
                .map_err(|_| invalid())
        };
        check_delivery(socket)?;
        write!(socket, "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\n{extra}Connection: close\r\n\r\n", bytes.len()).map_err(|_| invalid())?;
        if method == "GET" {
            for chunk in bytes.chunks(64 * 1024) {
                check_delivery(socket)?;
                socket.write_all(chunk).map_err(|_| invalid())?;
            }
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
    fn fixture_session(size: usize, known: bool, gets: Arc<AtomicUsize>) -> Arc<Session> {
        Arc::new(Session {
            root: "http://127.0.0.1:1234/fixture/".into(),
            resources: Mutex::new(vec![Resource {
                url: Url::parse("https://flixcloud.cc/segment.ts").unwrap(),
                kind: Kind::Segment,
            }]),
            domains: vec!["flixcloud.cc".into()],
            spec: FlixcloudPlayback {
                mask: [37; 16],
                w_payload: "fixture".into(),
            },
            cancelled: Arc::new(|| false),
            lifecycle: RelayLifecycle::new(Arc::new(|| Duration::ZERO)).0,
            transport: Some(Arc::new(move |_| {
                gets.fetch_add(1, Ordering::Relaxed);
                // A wrapped segment exercises the entire decode/range path.
                (
                    Box::new(
                        std::io::Cursor::new(b"\x89PNG\r\n\x1a\n")
                            .chain(std::io::repeat(71 ^ 37).take((size - 8) as u64)),
                    ),
                    known.then_some(size as u64),
                )
            })),
        })
    }

    fn relay_request(
        session: Arc<Session>,
        method: &str,
        range: Option<&str>,
    ) -> (Vec<u8>, Duration) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            session.serve(&mut socket)
        });
        let started = Instant::now();
        write!(
            client,
            "{method} /fixture/0 HTTP/1.1\r\nHost: localhost\r\n{}\r\n",
            range
                .map(|range| format!("Range: {range}\r\n"))
                .unwrap_or_default()
        )
        .unwrap();
        let mut response = Vec::new();
        while !response.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            assert_eq!(client.read(&mut byte).unwrap(), 1);
            response.push(byte[0]);
        }
        let mut first = [0];
        let count = client.read(&mut first).unwrap();
        let first_payload = started.elapsed();
        response.extend_from_slice(&first[..count]);
        client.read_to_end(&mut response).unwrap();
        server.join().unwrap().unwrap();
        (response, first_payload)
    }

    #[test]
    fn native_relay_large_wrapped_segments_ranges_and_heads() {
        for known in [false, true] {
            let size = 3 * 1024 * 1024;
            let gets = Arc::new(AtomicUsize::new(0));
            let session = fixture_session(size, known, gets.clone());
            let (response, _) = relay_request(session.clone(), "GET", None);
            let offset = response
                .windows(4)
                .position(|bytes| bytes == b"\r\n\r\n")
                .unwrap()
                + 4;
            assert_eq!(response.len() - offset, size - 8);
            assert!(response[offset..].iter().all(|byte| *byte == 71));
            let (response, _) = relay_request(session.clone(), "GET", Some("bytes=2-4"));
            assert!(response.starts_with(b"HTTP/1.1 206"));
            assert!(response.ends_with(b"GGG"));
            let (response, _) = relay_request(session, "HEAD", None);
            assert!(response.ends_with(b"\r\n\r\n"));
            // MED-12 reuse is deliberately separate: these are three upstream GETs.
            assert_eq!(gets.load(Ordering::Relaxed), 3);
        }
    }

    #[test]
    fn native_relay_reader_workload_measurements() {
        for readers in [1, 4, 16] {
            let gets = Arc::new(AtomicUsize::new(0));
            let session = fixture_session(3 * 1024 * 1024, false, gets.clone());
            let barrier = Arc::new(std::sync::Barrier::new(readers));
            let handles: Vec<_> = (0..readers)
                .map(|_| {
                    let session = session.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        let (response, first_payload) = relay_request(session, "GET", None);
                        let offset = response
                            .windows(4)
                            .position(|bytes| bytes == b"\r\n\r\n")
                            .unwrap()
                            + 4;
                        let digest = aws_lc_rs::digest::digest(
                            &aws_lc_rs::digest::SHA256,
                            &response[offset..],
                        );
                        let hash: String = digest
                            .as_ref()
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect();
                        assert_eq!(response.len() - offset, 3 * 1024 * 1024 - 8);
                        (first_payload.as_micros(), hash)
                    })
                })
                .collect();
            let samples: Vec<_> = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect();
            assert_eq!(gets.load(Ordering::Relaxed), readers);
            assert!(samples.iter().all(|sample| sample.1 == samples[0].1));
            let rss = std::fs::read_to_string("/proc/self/status")
                .ok()
                .map(|status| {
                    status
                        .lines()
                        .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
                        .collect::<Vec<_>>()
                        .join(" ")
                });
            eprintln!(
                "MED01 fixture=3MiB-PNG readers={readers} GETs={} first_payload_us={:?} decoded_sha256={} rss={rss:?}",
                gets.load(Ordering::Relaxed),
                samples.iter().map(|sample| sample.0).collect::<Vec<_>>(),
                samples[0].1
            );
        }
    }

    #[test]
    fn relay_lease_clock_boundaries_and_final_owner_drop() {
        use std::sync::atomic::AtomicU64;
        let clock = Arc::new(AtomicU64::new(0));
        let now = clock.clone();
        let (lifecycle, lease) = RelayLifecycle::new(Arc::new(move || {
            Duration::from_secs(now.load(Ordering::Acquire))
        }));
        for time in [599, 600, 601, 21_599] {
            clock.store(time, Ordering::Release);
            assert!(lifecycle.allows_serving(false), "owned at {time}");
            assert!(!lease.has_ended());
        }
        clock.store(21_600, Ordering::Release);
        assert!(!lifecycle.allows_serving(false));
        assert!(lease.has_ended());
        clock.store(21_601, Ordering::Release);
        assert!(lease.has_ended());
        // A terminal result cannot be made live by clock/activity recreation.
        clock.store(1, Ordering::Release);
        assert!(lease.has_ended());

        clock.store(1000, Ordering::Release);
        let now = clock.clone();
        let (lifecycle, lease) = RelayLifecycle::new(Arc::new(move || {
            Duration::from_secs(now.load(Ordering::Acquire))
        }));
        let retained = lease.clone();
        drop(lease);
        clock.store(1601, Ordering::Release);
        assert!(
            !retained.has_ended(),
            "a live clone retains one shared owner"
        );
        drop(retained);
        for time in [2200, 2201, 2202] {
            clock.store(time, Ordering::Release);
            assert_eq!(lifecycle.allows_serving(false), time < 2201);
        }

        let (lifecycle, _lease) = RelayLifecycle::new(Arc::new(|| Duration::ZERO));
        assert!(
            !lifecycle.allows_serving(true),
            "cancellation wins over ownership"
        );
        assert!(!lifecycle.allows_serving(false), "cancellation is terminal");
    }

    #[test]
    fn relay_listener_owned_handoff_orphan_cancellation_and_hard_limit() {
        use std::sync::atomic::{AtomicBool, AtomicU64};
        fn fixture(
            clock: Arc<AtomicU64>,
            cancel: Arc<AtomicBool>,
        ) -> (String, PlaybackRelayLease, std::thread::JoinHandle<()>) {
            SESSIONS.fetch_add(1, Ordering::Relaxed);
            let slot = Slot;
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let root = format!(
                "http://127.0.0.1:{}/fixture/",
                listener.local_addr().unwrap().port()
            );
            let (lifecycle, lease) = RelayLifecycle::new(Arc::new(move || {
                Duration::from_secs(clock.load(Ordering::Acquire))
            }));
            let serving = lifecycle.clone();
            let mut session = fixture_session(4096, true, Arc::new(AtomicUsize::new(0)));
            let inner = Arc::get_mut(&mut session).unwrap();
            inner.root = root.clone();
            inner.lifecycle = lifecycle;
            inner.cancelled =
                Arc::new(move || !serving.allows_serving(cancel.load(Ordering::Acquire)));
            let listener = std::thread::spawn(move || {
                let _slot = slot;
                run_listener(session, listener);
            });
            (format!("{root}0"), lease, listener)
        }
        fn finish(thread: std::thread::JoinHandle<()>) {
            let started = Instant::now();
            while !thread.is_finished() {
                assert!(
                    started.elapsed() < Duration::from_secs(3),
                    "listener did not retire"
                );
                std::thread::park_timeout(Duration::from_millis(1));
            }
            thread.join().unwrap();
        }
        let initial = SESSIONS.load(Ordering::Relaxed);
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let get = |url: &str| {
            let response = client.get(url).send().unwrap().error_for_status().unwrap();
            let bytes = response.bytes().unwrap();
            assert_eq!(bytes.len(), 4088);
            assert!(bytes.iter().all(|byte| *byte == 71));
        };
        let clock = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (url, lease, listener) = fixture(clock.clone(), cancel.clone());
        assert_eq!(SESSIONS.load(Ordering::Relaxed), initial + 1);
        // A resolver result crosses the worker/UI handoff with ownership, then
        // remains owned through a paused or blocked transfer at >600s.
        let (tx, rx) = std::sync::mpsc::channel();
        let handoff = std::thread::spawn(move || {
            tx.send(crate::ResolvedProviderStream {
                stream: addons::Stream::default(),
                relay: Some(lease),
            })
            .unwrap_or_else(|_| panic!("handoff receiver disappeared"));
        });
        let owned = rx.recv().unwrap();
        handoff.join().unwrap();
        for time in [599, 600, 601] {
            clock.store(time, Ordering::Release);
            assert!(!owned.relay.as_ref().unwrap().has_ended());
        }
        get(&url); // first network activity after a >600s paused/blocked interval
        drop(owned); // bounded orphan starts at 601, not at last network request
        clock.store(1200, Ordering::Release);
        get(&url); // activity at 599s orphan age cannot renew the deadline
        clock.store(1201, Ordering::Release);
        finish(listener);
        assert!(client.get(&url).send().is_err());
        assert_eq!(SESSIONS.load(Ordering::Relaxed), initial);

        clock.store(0, Ordering::Release);
        let (url, lease, listener) = fixture(clock.clone(), cancel.clone());
        get(&url);
        cancel.store(true, Ordering::Release);
        finish(listener);
        assert!(lease.has_ended());
        assert!(client.get(&url).send().is_err());
        assert_eq!(SESSIONS.load(Ordering::Relaxed), initial);
        drop(lease);

        clock.store(0, Ordering::Release);
        cancel.store(false, Ordering::Release);
        let (url, lease, listener) = fixture(clock.clone(), cancel);
        clock.store(21_599, Ordering::Release);
        get(&url);
        clock.store(21_600, Ordering::Release);
        finish(listener);
        assert!(lease.has_ended());
        assert!(client.get(&url).send().is_err());
        assert_eq!(SESSIONS.load(Ordering::Relaxed), initial);
    }

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
            lifecycle: RelayLifecycle::new(Arc::new(|| Duration::ZERO)).0,
            transport: None,
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
        let (local, _lease) = start(
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
