//! Restricted transport for public addon directories and their untrusted logos.
//! General provider/media requests deliberately retain their existing policy.
use super::FetchError;
use reqwest::Url;
use std::{
    io::{Cursor, Read},
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;
const LOGO_BYTES: usize = 2 * 1024 * 1024;
const LOGO_SOURCE_SIDE: u32 = 1024;
const LOGO_DISPLAY_SIDE: u32 = 64;
const MAX_DNS_WORKERS: usize = 4;
const MAX_DNS_ADDRESSES: usize = 16;
static DNS_WORKERS: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192
                    && ((b == 0 && matches!(c, 0 | 2)) || (b == 88 && c == 99) || b == 168))
                || (a == 198 && (matches!(b, 18 | 19) || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            // Only ordinary global unicast: exclude local/mapped/NAT64 and
            // transition ranges, plus documentation and special assignments.
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && (segments[1] < 0x0200 || segments[1] == 0x0db8))
                && !matches!(segments[0], 0x2002 | 0x3fff)
        }
    }
}

/// Public directory URLs cannot name local resources or carry credentials.
/// This policy is also applied to every redirect before sending its request.
pub fn public_url(raw: &str) -> Option<Url> {
    if raw.len() > 8192 {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?.trim_end_matches('.').to_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return None;
    }
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        if !public_ip(ip) {
            return None;
        }
    } else if !host.contains('.') {
        return None;
    }
    Some(url)
}

fn redirect_target(current: &Url, location: &str) -> Result<Url, FetchError> {
    let target = current
        .join(location)
        .ok()
        .and_then(|target| public_url(target.as_str()))
        .filter(|target| current.scheme() != "https" || target.scheme() == "https");
    target.ok_or_else(|| FetchError::new(None, "directory redirect destination is not allowed"))
}

fn read_bounded(reader: impl Read, limit: usize) -> Result<Vec<u8>, FetchError> {
    let mut body = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| FetchError::new(None, error.to_string()))?;
    if body.len() > limit {
        return Err(FetchError::new(
            None,
            "directory response exceeded its byte limit",
        ));
    }
    Ok(body)
}

fn validate_addresses(addresses: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, FetchError> {
    if addresses.is_empty()
        || addresses.len() > MAX_DNS_ADDRESSES
        || addresses.iter().any(|address| !public_ip(address.ip()))
    {
        return Err(FetchError::new(
            None,
            "directory host resolved to a nonpublic address",
        ));
    }
    Ok(addresses)
}

struct DnsWorker;
impl Drop for DnsWorker {
    fn drop(&mut self) {
        let mut active = DNS_WORKERS.0.lock().unwrap();
        *active = active.saturating_sub(1);
        DNS_WORKERS.1.notify_one();
    }
}

fn dns_worker_slot(timeout: Duration) -> Result<DnsWorker, FetchError> {
    let started = Instant::now();
    let mut active = DNS_WORKERS.0.lock().unwrap();
    while *active >= MAX_DNS_WORKERS {
        let remaining = timeout
            .checked_sub(started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| FetchError::new(None, "directory DNS resolver timed out"))?;
        active = DNS_WORKERS.1.wait_timeout(active, remaining).unwrap().0;
    }
    *active += 1;
    Ok(DnsWorker)
}

fn resolve_public(url: &Url, timeout: Duration) -> Result<Vec<SocketAddr>, FetchError> {
    let started = Instant::now();
    let host = url
        .host_str()
        .ok_or_else(|| FetchError::new(None, "directory host is missing"))?
        .trim_matches(['[', ']'])
        .to_string();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| FetchError::new(None, "directory port is missing"))?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        return validate_addresses(vec![SocketAddr::new(ip, port)]);
    }
    // System lookup cannot be cancelled. Timed-out workers retain their slot
    // until the lookup returns, so repeated failures cannot spawn unbounded
    // resolver threads. Other logos wait for a slot within their remaining
    // request budget instead of recording an artificial busy failure.
    let permit = dns_worker_slot(timeout)?;
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("addon-directory-dns".into())
        .spawn(move || {
            let _permit = permit;
            let addresses = (host.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| addresses.take(MAX_DNS_ADDRESSES + 1).collect::<Vec<_>>());
            let _ = send.send(addresses);
        })
        .map_err(|error| FetchError::new(None, error.to_string()))?;
    let remaining = timeout
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| FetchError::new(None, "directory DNS lookup timed out"))?;
    let addresses = receive
        .recv_timeout(remaining)
        .map_err(|_| FetchError::new(None, "directory DNS lookup timed out"))?
        .map_err(|error| FetchError::new(None, error.to_string()))?;
    validate_addresses(addresses)
}

fn client(url: &Url, timeout: Duration) -> Result<reqwest::blocking::Client, FetchError> {
    let addresses = resolve_public(url, timeout)?;
    client_for_addresses(url, addresses)
}

#[derive(Debug)]
struct RejectUnpinnedDns;
impl reqwest::dns::Resolve for RejectUnpinnedDns {
    fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(std::future::ready(Err::<
            reqwest::dns::Addrs,
            Box<dyn std::error::Error + Send + Sync>,
        >(Box::new(
            std::io::Error::other("directory DNS destination was not pinned"),
        ))))
    }
}

fn client_for_addresses(
    url: &Url,
    addresses: Vec<SocketAddr>,
) -> Result<reqwest::blocking::Client, FetchError> {
    let addresses = validate_addresses(addresses)?;
    // Redirects are followed explicitly so no destination request precedes
    // the URL-policy check. The Android TLS roots match ordinary media fetches.
    let builder = reqwest::blocking::Client::builder()
        .no_proxy()
        .dns_resolver(std::sync::Arc::new(RejectUnpinnedDns))
        .resolve_to_addrs(url.host_str().unwrap_or_default(), &addresses)
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());
    #[cfg(target_os = "android")]
    let builder = builder.tls_backend_preconfigured(
        super::imp::tls_config().map_err(|error| FetchError::new(None, error))?,
    );
    builder
        .build()
        .map_err(|error| FetchError::new(None, error.to_string()))
}

fn get_with_connector(
    raw: &str,
    limit: usize,
    require_https: bool,
    mut connect: impl FnMut(&Url, Duration) -> Result<reqwest::blocking::Client, FetchError>,
) -> Result<Vec<u8>, FetchError> {
    let mut url = public_url(raw)
        .filter(|url| !require_https || url.scheme() == "https")
        .ok_or_else(|| FetchError::new(None, "directory URL is not allowed"))?;
    let started = Instant::now();
    for hop in 0..=MAX_REDIRECTS {
        let remaining = TIMEOUT
            .checked_sub(started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| FetchError::new(None, "directory request timed out"))?;
        let client = connect(&url, remaining)?;
        let remaining = TIMEOUT
            .checked_sub(started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| FetchError::new(None, "directory request timed out"))?;
        let response = client
            .get(url.clone())
            .timeout(remaining)
            .send()
            .map_err(|error| {
                FetchError::new(error.status().map(|s| s.as_u16()), error.to_string())
            })?;
        if response.status().is_redirection() {
            if hop == MAX_REDIRECTS {
                return Err(FetchError::new(None, "too many directory redirects"));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| FetchError::new(None, "directory redirect has no valid location"))?;
            url = redirect_target(&url, location)?;
            continue;
        }
        let response = response.error_for_status().map_err(|error| {
            FetchError::new(error.status().map(|s| s.as_u16()), error.to_string())
        })?;
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(FetchError::new(
                None,
                "directory response exceeded its byte limit",
            ));
        }
        return read_bounded(response, limit);
    }
    unreachable!("redirect limit returns from the loop")
}

/// Read a bounded HTTPS directory document on a worker, without provider dispatch.
pub fn fetch_document(
    url: String,
    response_limit: usize,
    then: impl FnOnce(Result<Vec<u8>, FetchError>) + Send + 'static,
) {
    std::thread::Builder::new()
        .name("addon-directory-fetch".into())
        .spawn(move || {
            then(get_with_connector(&url, response_limit, true, client));
        })
        .expect("spawn addon directory worker");
}

/// Validate dimensions and decoder allocation limits before raster allocation.
/// Only the bounded thumbnail is handed to the ordinary cache processing core.
fn thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() > LOGO_BYTES {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(LOGO_SOURCE_SIDE);
    limits.max_image_height = Some(LOGO_SOURCE_SIDE);
    limits.max_alloc = Some(8 * 1024 * 1024);
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let scaled = image.resize(
        LOGO_DISPLAY_SIDE,
        LOGO_DISPLAY_SIDE,
        image::imageops::FilterType::Triangle,
    );
    let mut encoded = Cursor::new(Vec::new());
    scaled
        .write_to(&mut encoded, image::ImageFormat::Png)
        .ok()?;
    Some(encoded.into_inner())
}

/// Directory logos share the processed/decoded cache, but neither network bytes
/// nor original raster dimensions can bypass the browser's separate limits.
pub fn fetch_logo(
    url: String,
    then: impl FnOnce(Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>) + Send + 'static,
) {
    std::thread::Builder::new()
        .name("addon-directory-logo".into())
        .spawn(move || {
            let pixels = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let settings = nova_config::active_cache_settings();
                crate::cache::cached_pixels_with(
                    &nova_config::poster_cache_dir(),
                    &url,
                    crate::cache::ImageUse::Fixed(Some(LOGO_DISPLAY_SIDE)),
                    &settings,
                    || {
                        let bytes = get_with_connector(&url, LOGO_BYTES, false, client).ok()?;
                        thumbnail(&bytes)
                    },
                )
            }))
            .ok()
            .flatten();
            then(pixels);
        })
        .expect("spawn addon directory logo worker");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener};

    // Loopback is an explicit transport-fixture exception, only compiled into
    // tests. Production client construction always validates resolved answers.
    fn get_with_fixture_client(
        client: &reqwest::blocking::Client,
        raw: &str,
        limit: usize,
        require_https: bool,
    ) -> Result<Vec<u8>, FetchError> {
        get_with_connector(raw, limit, require_https, |url, _| {
            assert_eq!(url.host_str(), Some("directory.fixture.test"));
            Ok(client.clone())
        })
    }

    #[test]
    fn public_hostname_cannot_pin_private_or_mixed_dns_answers() {
        let url = public_url("https://directory.fixture.test/catalog").unwrap();
        for address in [
            "127.0.0.1:443",
            "10.0.0.1:443",
            "192.168.1.1:443",
            "100.64.0.1:443",
            "[::1]:443",
            "[::ffff:127.0.0.1]:443",
            "[fd00::1]:443",
        ] {
            let error = client_for_addresses(&url, vec![address.parse().unwrap()]).unwrap_err();
            assert!(error.message.contains("nonpublic"), "{address}");
        }
        assert!(
            client_for_addresses(
                &url,
                vec![
                    "1.1.1.1:443".parse().unwrap(),
                    "127.0.0.1:443".parse().unwrap()
                ]
            )
            .is_err()
        );
        assert!(
            validate_addresses(vec![
                "1.1.1.1:443".parse().unwrap(),
                "[2606:4700:4700::1111]:443".parse().unwrap()
            ])
            .is_ok()
        );
        assert!(validate_addresses(Vec::new()).is_err());
    }

    #[test]
    fn resolver_slots_are_bounded_and_reused_and_unpinned_names_fail_closed() {
        let mut held = (0..MAX_DNS_WORKERS)
            .map(|_| dns_worker_slot(Duration::from_secs(1)).unwrap())
            .collect::<Vec<_>>();
        assert!(dns_worker_slot(Duration::from_millis(1)).is_err());
        let (ready_send, ready_receive) = mpsc::sync_channel(1);
        let waiter = std::thread::spawn(move || {
            let slot = dns_worker_slot(Duration::from_secs(1)).unwrap();
            ready_send.send(()).unwrap();
            drop(slot);
        });
        drop(held.pop());
        ready_receive.recv_timeout(Duration::from_secs(1)).unwrap();
        waiter.join().unwrap();
        drop(held);

        let mut lookup = reqwest::dns::Resolve::resolve(
            &RejectUnpinnedDns,
            "unlisted.fixture.test".parse().unwrap(),
        );
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            lookup.as_mut().poll(&mut context),
            std::task::Poll::Ready(Err(_))
        ));
    }

    #[test]
    fn redirects_revalidate_hosts_credentials_and_https_before_following() {
        let source = Url::parse("https://directory.example/catalog").unwrap();
        for location in [
            "http://public.example/logo.png",
            "http://127.0.0.1/private",
            "https://10.0.0.1/private",
            "https://localhost/private",
            "https://source.local/private",
            "https://user:secret@public.example/logo.png",
            "file:///etc/passwd",
            "https://[::1]/private",
        ] {
            assert!(redirect_target(&source, location).is_err(), "{location}");
        }
        assert_eq!(
            redirect_target(&source, "../safe").unwrap().as_str(),
            "https://directory.example/safe"
        );
        assert!(redirect_target(&source, "https://other.example/logo.png").is_ok());
    }

    fn fixture(
        response: Vec<u8>,
    ) -> (
        String,
        reqwest::blocking::Client,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut headers = Vec::new();
            let mut byte = [0];
            while !headers.ends_with(b"\r\n\r\n") && headers.len() < 8192 {
                if stream.read(&mut byte).unwrap_or_default() == 0 {
                    break;
                }
                headers.push(byte[0]);
            }
            let _ = stream.write_all(&response);
        });
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .resolve("directory.fixture.test", address)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        (
            format!("http://directory.fixture.test:{}", address.port()),
            client,
            worker,
        )
    }

    #[test]
    fn actual_transport_blocks_redirect_to_local_and_oversized_chunked_body() {
        let (url, client, worker) = fixture(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec());
        let error = get_with_fixture_client(&client, &url, 32, false).unwrap_err();
        assert!(error.message.contains("redirect destination"));
        worker.join().unwrap();
        let (url, client, worker) = fixture(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n8\r\n12345678\r\n0\r\n\r\n".to_vec());
        assert!(
            get_with_fixture_client(&client, &url, 4, false)
                .unwrap_err()
                .message
                .contains("byte limit")
        );
        worker.join().unwrap();
        assert_eq!(read_bounded(Cursor::new(b"1234"), 4).unwrap(), b"1234");
        assert!(read_bounded(Cursor::new(b"12345"), 4).is_err());
    }

    #[test]
    fn logo_dimension_and_compressed_limits_precede_raster_and_cache_processing() {
        let png = |width, height| {
            let image = image::DynamicImage::ImageRgba8(image::RgbaImage::new(width, height));
            let mut bytes = Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            bytes.into_inner()
        };
        assert!(thumbnail(&vec![0; LOGO_BYTES + 1]).is_none());
        assert!(thumbnail(&png(LOGO_SOURCE_SIDE + 1, 1)).is_none());
        assert!(thumbnail(&png(1, LOGO_SOURCE_SIDE + 1)).is_none());
        let valid = thumbnail(&png(256, 128)).unwrap();
        let decoded = image::load_from_memory(&valid).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (64, 32));
    }
}
