use crate::*;
use mdns_sd::{ScopedIp, ServiceDaemon, ServiceEvent};
use reqwest::Url;
use std::{
    collections::{HashMap, HashSet},
    net::{Ipv4Addr, SocketAddrV6},
};
const SERVICE: &str = "_googlecast._tcp.local.";
const MAX_DEVICES: usize = 64;
struct Browse(ServiceDaemon);
impl Drop for Browse {
    fn drop(&mut self) {
        let _ = self.0.stop_browse(SERVICE);
        let _ = self.0.shutdown();
    }
}
/// Runs fresh mDNS and SSDP searches. Removal events within this window are honored;
/// there is no stale process-global discovery cache or background browser.
pub async fn discover(timeout: Duration, cancellation: Arc<AtomicBool>) -> Result<Vec<Device>> {
    check_cancel(&cancellation)?;
    let timeout = timeout.clamp(Duration::from_millis(100), Duration::from_secs(15));
    let (google, dlna) = tokio::join!(
        discover_google(timeout, cancellation.clone()),
        discover_dlna(timeout, cancellation.clone(), None)
    );
    check_cancel(&cancellation)?;
    let mut result = Vec::new();
    // One unavailable multicast protocol does not discard the other's devices.
    let both_failed = google.is_err() && dlna.is_err();
    if let Ok(mut devices) = google {
        result.append(&mut devices);
    }
    if let Ok(mut devices) = dlna {
        result.append(&mut devices);
    }
    if both_failed {
        return Err(error(CastErrorKind::Network));
    }
    result.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    result.dedup_by(|a, b| a.id == b.id);
    result.truncate(MAX_DEVICES);
    Ok(result)
}
async fn discover_google(timeout: Duration, cancel: Arc<AtomicBool>) -> Result<Vec<Device>> {
    let daemon = Browse(ServiceDaemon::new().map_err(|_| error(CastErrorKind::Network))?);
    let events = daemon
        .0
        .browse(SERVICE)
        .map_err(|_| error(CastErrorKind::Network))?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut devices: HashMap<String, Device> = HashMap::new();
    let mut names = HashMap::new();
    while tokio::time::Instant::now() < deadline {
        check_cancel(&cancel)?;
        // Limit each drain to avoid a hostile multicast flood starving cancellation.
        for _ in 0..128 {
            match events.try_recv() {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    let id = info
                        .get_property_val_str("id")
                        .filter(|id| !id.is_empty() && id.len() <= 128)
                        .map(cast_id);
                    let Some(id) = id else { continue };
                    let mut addresses: Vec<SocketAddr> = info
                        .get_addresses()
                        .iter()
                        .filter_map(|ip| scoped_socket(ip, info.get_port()))
                        .collect();
                    addresses.sort_by_key(|a| (!a.is_ipv4(), a.to_string()));
                    let Some(address) = addresses.first().copied() else {
                        continue;
                    };
                    let ca = info
                        .get_property_val_str("ca")
                        .and_then(|s| s.parse::<u32>().ok());
                    let dev = Device {
                        id: id.clone(),
                        name: info
                            .get_property_val_str("fn")
                            .unwrap_or("Google Cast")
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(128)
                            .collect(),
                        protocol: Protocol::GoogleCast,
                        address,
                        receiver_ip: address.ip(),
                        video: ca.is_none_or(|ca| ca & 1 != 0),
                        model: info
                            .get_property_val_str("md")
                            .map(|s| s.chars().take(128).collect()),
                        endpoint: None,
                    };
                    names.insert(info.get_fullname().to_owned(), id.clone());
                    if devices.len() < MAX_DEVICES || devices.contains_key(&id) {
                        devices.insert(id, dev);
                    }
                }
                Ok(ServiceEvent::ServiceRemoved(_, name)) => {
                    if let Some(id) = names.remove(&name) {
                        devices.remove(&id);
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(devices.into_values().collect())
}
fn scoped_socket(ip: &ScopedIp, port: u16) -> Option<SocketAddr> {
    if port == 0 {
        return None;
    }
    match ip {
        ScopedIp::V4(ip) => Some(SocketAddr::new(IpAddr::V4(*ip.addr()), port)),
        ScopedIp::V6(ip) => Some(SocketAddr::V6(SocketAddrV6::new(
            *ip.addr(),
            port,
            0,
            ip.scope_id().index,
        ))),
        _ => None,
    }
}
fn search_packet() -> &'static [u8] {
    b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n"
}
async fn discover_dlna(
    timeout: Duration,
    cancel: Arc<AtomicBool>,
    target: Option<IpAddr>,
) -> Result<Vec<Device>> {
    let mut interfaces: Vec<IpAddr> = if let Some(ip) = target {
        let socket = tokio::net::UdpSocket::bind(if ip.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })
            .await
            .map_err(|_| error(CastErrorKind::Network))?;
        socket
            .connect(SocketAddr::new(ip, 1900))
            .await
            .map_err(|_| error(CastErrorKind::Network))?;
        vec![
            socket
                .local_addr()
                .map_err(|_| error(CastErrorKind::Network))?
                .ip(),
        ]
    } else {
        if_addrs::get_if_addrs()
            .map_err(|_| error(CastErrorKind::Network))?
            .into_iter()
            .filter(|i| !i.is_loopback())
            .map(|i| i.ip())
            .filter(IpAddr::is_ipv4)
            .collect()
    };
    interfaces.sort();
    interfaces.dedup();
    interfaces.truncate(16);
    let final_deadline = tokio::time::Instant::now() + timeout;
    let receive_timeout = timeout / 2;
    let mut workers = tokio::task::JoinSet::new();
    for interface in interfaces {
        let cancel = cancel.clone();
        workers.spawn(async move {
            let socket = tokio::net::UdpSocket::bind(SocketAddr::new(interface, 0))
                .await
                .map_err(|_| error(CastErrorKind::Network))?;
            let destination = SocketAddr::new(
                target.unwrap_or(IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250))),
                1900,
            );
            socket
                .send_to(search_packet(), destination)
                .await
                .map_err(|_| error(CastErrorKind::Network))?;
            let deadline = tokio::time::Instant::now() + receive_timeout;
            let mut buffer = [0u8; 8192];
            let mut candidates = HashMap::new();
            let mut received = 0;
            loop {
                check_cancel(&cancel)?;
                if tokio::time::Instant::now() >= deadline || received >= 128 {
                    break;
                }
                match tokio::time::timeout(Duration::from_millis(50), socket.recv_from(&mut buffer))
                    .await
                {
                    Ok(Ok((n, source))) => {
                        received += 1;
                        if target.is_some_and(|ip| ip != source.ip()) {
                            continue;
                        }
                        if let Some(url) = ssdp_location(&buffer[..n], source.ip())
                            && candidates.len() < 16
                        {
                            candidates.insert(url.to_string(), (url, source.ip()));
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(_) => {}
                }
            }
            Ok::<_, CastError>(candidates.into_values().collect::<Vec<_>>())
        });
    }
    let mut candidates = Vec::new();
    while let Some(join) = workers.join_next().await {
        if let Ok(Ok(mut urls)) = join {
            candidates.append(&mut urls);
        }
    }
    check_cancel(&cancel)?;
    let mut seen = HashSet::new();
    candidates.retain(|(u, _)| seen.insert(u.to_string()));
    candidates.truncate(MAX_DEVICES);
    let mut devices = HashMap::new();
    // Bound simultaneous HTTP work and include it in the caller's discovery window.
    for batch in candidates.chunks(8) {
        if tokio::time::Instant::now() >= final_deadline {
            break;
        }
        let mut probes = tokio::task::JoinSet::new();
        for (url, source) in batch {
            let url = url.clone();
            let source = *source;
            let cancel = cancel.clone();
            probes.spawn(async move {
                let mut device = bounded(&cancel, dlna::description(url, source)).await?;
                if let Ok(session) = bounded(&cancel, dlna::Dlna::connect(device.clone())).await {
                    device.video = session.video_capable();
                }
                Ok::<_, CastError>(device)
            });
        }
        while !probes.is_empty() {
            check_cancel(&cancel)?;
            if tokio::time::Instant::now() >= final_deadline {
                break;
            }
            if let Ok(Some(Ok(Ok(device)))) =
                tokio::time::timeout(Duration::from_millis(50), probes.join_next()).await
            {
                devices.entry(device.id.clone()).or_insert(device);
            }
        }
    }
    Ok(devices.into_values().collect())
}
fn ssdp_location(packet: &[u8], source: IpAddr) -> Option<Url> {
    let text = std::str::from_utf8(packet).ok()?;
    if !text.starts_with("HTTP/1.1 200 ") {
        return None;
    }
    let mut location = None;
    let mut renderer = false;
    for line in text.lines().skip(1).take(64) {
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let val = val.trim();
        if key.eq_ignore_ascii_case("location") {
            if location.is_some() {
                return None;
            }
            location = Some(val);
        }
        if key.eq_ignore_ascii_case("st") {
            renderer = val.starts_with("urn:schemas-upnp-org:device:MediaRenderer:");
        }
    }
    if !renderer {
        return None;
    }
    let url = Url::parse(location?).ok()?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url
            .host_str()?
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .ok()
            != Some(source)
    {
        return None;
    }
    Some(url)
}
/// Accept a literal IP, IP:Cast-port, or an HTTP DLNA description URL. No DNS
/// resolution of untrusted discovery data is performed.
pub async fn discover_manual(address: &str, cancellation: Arc<AtomicBool>) -> Result<Vec<Device>> {
    check_cancel(&cancellation)?;
    let address = address.trim();
    if address.len() > 2048 {
        return Err(error(CastErrorKind::InvalidInput));
    }
    if address.starts_with("http://") {
        let url = Url::parse(address).map_err(|_| error(CastErrorKind::InvalidInput))?;
        let ip = url
            .host_str()
            .and_then(|h| h.trim_matches(['[', ']']).parse().ok())
            .ok_or_else(|| error(CastErrorKind::InvalidInput))?;
        let mut device = bounded(&cancellation, dlna::description(url, ip)).await?;
        if let Ok(session) = bounded(&cancellation, dlna::Dlna::connect(device.clone())).await {
            device.video = session.video_capable();
        }
        check_cancel(&cancellation)?;
        return Ok(vec![device]);
    }
    let socket = address
        .parse::<SocketAddr>()
        .or_else(|_| {
            address
                .parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, 8009))
        })
        .map_err(|_| error(CastErrorKind::InvalidInput))?;
    if socket.ip().is_unspecified() || socket.ip().is_multicast() || socket.port() == 0 {
        return Err(error(CastErrorKind::InvalidInput));
    }
    let mut probe = Device {
        id: format!("cast:manual:{}", socket),
        name: format!("Google Cast ({})", socket.ip()),
        protocol: Protocol::GoogleCast,
        address: socket,
        receiver_ip: socket.ip(),
        video: true,
        model: None,
        endpoint: None,
    };
    let (cast, dlna) = tokio::join!(
        bounded(&cancellation, google::Google::connect(&probe)),
        discover_dlna(
            Duration::from_secs(2),
            cancellation.clone(),
            Some(socket.ip())
        )
    );
    check_cancel(&cancellation)?;
    let mut devices = dlna.unwrap_or_default();
    if cast.is_ok() {
        let _ = bounded(&cancellation, async {
            dlna::manual_cast_identity(&mut probe).await;
            Ok(())
        })
        .await;
        devices.push(probe);
    }
    check_cancel(&cancellation)?;
    if devices.is_empty() {
        return Err(error(CastErrorKind::Network));
    }
    Ok(devices)
}
pub(crate) fn cast_id(raw: &str) -> String {
    format!(
        "cast:{}",
        raw.trim_start_matches("uuid:")
            .replace('-', "")
            .to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ssdp_rejects_redirected_credentialed_and_duplicate_locations() {
        let ip = "192.168.1.2".parse().unwrap();
        let packet = |location: &str| {
            format!(
                "HTTP/1.1 200 OK\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\nLOCATION: {location}\r\n\r\n"
            )
        };
        assert!(ssdp_location(packet("http://192.168.1.2/desc").as_bytes(), ip).is_some());
        for location in [
            "http://attacker.invalid/",
            "file:///etc/passwd",
            "http://user:pass@192.168.1.2/",
        ] {
            assert!(ssdp_location(packet(location).as_bytes(), ip).is_none());
        }
        let duplicate = packet("http://192.168.1.2/")
            .replace("\r\n\r\n", "\r\nLocation: http://192.168.1.2/b\r\n\r\n");
        assert!(ssdp_location(duplicate.as_bytes(), ip).is_none());
        assert!(ssdp_location(&[255, 0], ip).is_none());
    }
}
