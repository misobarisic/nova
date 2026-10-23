use crate::*;
use reqwest::{Client, Url};
use std::collections::HashSet;
const LIMIT: usize = 512 * 1024;
#[derive(Clone)]
pub(crate) struct Service {
    kind: String,
    control: Url,
    scpd: Url,
}
#[derive(Clone)]
pub(crate) struct Description {
    pub av: Service,
    pub rendering: Option<Service>,
    pub connection: Option<Service>,
}
pub(crate) struct Dlna {
    client: Client,
    description: Description,
    caps: ReceiverCapabilities,
    pub occupied: bool,
    volume_range: Option<(u32, u32)>,
}
pub(crate) fn client() -> Result<Client> {
    let builder = Client::builder();
    // Android's platform verifier requires a JavaVM even when this client only
    // talks cleartext UPnP. Match Nova's explicit bundled-root HTTP transport.
    #[cfg(target_os = "android")]
    let builder = {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| error(CastErrorKind::Network))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        builder.tls_backend_preconfigured(config)
    };
    builder
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| error(CastErrorKind::Network))
}
async fn body(mut response: reqwest::Response) -> Result<String> {
    if response.content_length().is_some_and(|n| n > LIMIT as u64) {
        return Err(error(CastErrorKind::Protocol));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| error(CastErrorKind::Network))?
    {
        if bytes.len() + chunk.len() > LIMIT {
            return Err(error(CastErrorKind::Protocol));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| error(CastErrorKind::Protocol))
}
pub(crate) async fn description(url: Url, source: IpAddr) -> Result<Device> {
    validate_endpoint(&url, source)?;
    let client = client()?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|_| error(CastErrorKind::Network))?;
    if !response.status().is_success() {
        return Err(error(CastErrorKind::Protocol));
    }
    let xml = body(response).await?;
    parse_description(&xml, &url, source)
}
fn validate_endpoint(url: &Url, source: IpAddr) -> Result<()> {
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url
            .host_str()
            .and_then(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            != Some(source)
    {
        return Err(error(CastErrorKind::InvalidInput));
    }
    Ok(())
}
fn parse_description(xml: &str, location: &Url, source: IpAddr) -> Result<Device> {
    let doc = roxmltree::Document::parse(xml).map_err(|_| error(CastErrorKind::Protocol))?;
    let renderer = doc
        .descendants()
        .find(|n| {
            n.has_tag_name("device")
                && child(*n, "deviceType")
                    .is_some_and(|v| v.starts_with("urn:schemas-upnp-org:device:MediaRenderer:"))
        })
        .ok_or_else(|| error(CastErrorKind::Unsupported))?;
    let base = doc
        .descendants()
        .find(|n| n.has_tag_name("URLBase"))
        .and_then(|n| n.text())
        .and_then(|s| Url::parse(s.trim()).ok())
        .unwrap_or_else(|| location.clone());
    validate_endpoint(&base, source)?;
    let mut av = None;
    let mut rendering = None;
    let mut connection = None;
    for n in renderer
        .children()
        .filter(|n| n.has_tag_name("serviceList"))
        .flat_map(|n| n.children())
        .filter(|n| n.has_tag_name("service"))
    {
        let Some(kind) = child(n, "serviceType") else {
            continue;
        };
        if kind.len() > 256 {
            continue;
        }
        let Some(control) = child(n, "controlURL").and_then(|s| base.join(s).ok()) else {
            continue;
        };
        let Some(scpd) = child(n, "SCPDURL").and_then(|s| base.join(s).ok()) else {
            continue;
        };
        if validate_endpoint(&control, source).is_err() || validate_endpoint(&scpd, source).is_err()
        {
            continue;
        }
        let service = Service {
            kind: kind.to_owned(),
            control,
            scpd,
        };
        if kind.starts_with("urn:schemas-upnp-org:service:AVTransport:") {
            av = Some(service);
        } else if kind.starts_with("urn:schemas-upnp-org:service:RenderingControl:") {
            rendering = Some(service);
        } else if kind.starts_with("urn:schemas-upnp-org:service:ConnectionManager:") {
            connection = Some(service);
        }
    }
    let id = child(renderer, "UDN")
        .filter(|s| !s.is_empty() && s.len() < 512)
        .ok_or_else(|| error(CastErrorKind::Protocol))?;
    let port = location
        .port_or_known_default()
        .ok_or_else(|| error(CastErrorKind::Protocol))?;
    Ok(Device {
        id: format!("dlna:{id}"),
        name: child(renderer, "friendlyName")
            .unwrap_or("DLNA receiver")
            .chars()
            .filter(|c| !c.is_control())
            .take(128)
            .collect(),
        model: child(renderer, "modelName").map(|s| s.chars().take(128).collect()),
        protocol: Protocol::Dlna,
        address: SocketAddr::new(source, port),
        receiver_ip: source,
        video: true,
        endpoint: Some(Description {
            av: av.ok_or_else(|| error(CastErrorKind::Unsupported))?,
            rendering,
            connection,
        }),
    })
}
fn child<'a, 'input>(n: roxmltree::Node<'a, 'input>, name: &str) -> Option<&'a str> {
    n.children()
        .find(|n| n.has_tag_name(name))
        .and_then(|n| n.text())
        .map(str::trim)
}
fn value(xml: &str, name: &str) -> Option<String> {
    roxmltree::Document::parse(xml)
        .ok()?
        .descendants()
        .find(|n| n.has_tag_name(name))
        .and_then(|n| n.text())
        .map(str::to_owned)
}
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            c if c == '\t' || c == '\n' || c == '\r' || !c.is_control() => out.push(c),
            _ => {}
        }
    }
    out
}
async fn soap(
    client: &Client,
    service: &Service,
    action: &str,
    args: &[(&str, String)],
) -> Result<String> {
    let mut xml = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{}\">",
        escape(&service.kind)
    );
    for (key, val) in args {
        xml.push_str(&format!("<{key}>{}</{key}>", escape(val)));
    }
    xml.push_str(&format!("</u:{action}></s:Body></s:Envelope>"));
    let response = client
        .post(service.control.clone())
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("SOAPACTION", format!("\"{}#{action}\"", service.kind))
        .body(xml)
        .send()
        .await
        .map_err(|_| error(CastErrorKind::Network))?;
    let success = response.status().is_success();
    let xml = body(response).await?;
    let doc = roxmltree::Document::parse(&xml).map_err(|_| error(CastErrorKind::Protocol))?;
    if !success || doc.descendants().any(|n| n.has_tag_name("Fault")) {
        return Err(error(CastErrorKind::Protocol));
    }
    if !doc
        .descendants()
        .any(|n| n.tag_name().name() == format!("{action}Response"))
    {
        return Err(error(CastErrorKind::Protocol));
    }
    Ok(xml)
}
async fn scpd(
    client: &Client,
    service: &Service,
) -> Result<(HashSet<String>, Option<(u32, u32)>, bool)> {
    let response = client
        .get(service.scpd.clone())
        .send()
        .await
        .map_err(|_| error(CastErrorKind::Network))?;
    if !response.status().is_success() {
        return Err(error(CastErrorKind::Protocol));
    }
    let xml = body(response).await?;
    let doc = roxmltree::Document::parse(&xml).map_err(|_| error(CastErrorKind::Protocol))?;
    let actions = doc
        .descendants()
        .filter(|n| n.has_tag_name("action"))
        .filter_map(|n| child(n, "name").map(str::to_owned))
        .collect();
    let volume = doc
        .descendants()
        .find(|n| n.has_tag_name("stateVariable") && child(*n, "name") == Some("Volume"))
        .and_then(|n| n.children().find(|n| n.has_tag_name("allowedValueRange")))
        .and_then(|n| {
            Some((
                child(n, "minimum")?.parse().ok()?,
                child(n, "maximum")?.parse().ok()?,
            ))
        })
        .filter(|(min, max)| max > min);
    let rel_time = doc
        .descendants()
        .filter(|n| {
            n.has_tag_name("stateVariable") && child(*n, "name") == Some("A_ARG_TYPE_SeekMode")
        })
        .any(|n| {
            n.descendants()
                .any(|n| n.has_tag_name("allowedValue") && n.text() == Some("REL_TIME"))
        });
    Ok((actions, volume, rel_time))
}
impl Dlna {
    pub(crate) fn video_capable(&self) -> bool {
        self.caps.mime_types.is_empty()
            || self.caps.mime_types.iter().any(|m| {
                m.starts_with("video/")
                    || m == "application/vnd.apple.mpegurl"
                    || m == "application/x-mpegURL"
            })
    }
    pub async fn connect(device: Device) -> Result<Self> {
        let description = device
            .endpoint
            .ok_or_else(|| error(CastErrorKind::InvalidInput))?;
        let client = client()?;
        let (actions, _, relative) = scpd(&client, &description.av).await?;
        if ![
            "SetAVTransportURI",
            "Play",
            "Stop",
            "GetTransportInfo",
            "GetPositionInfo",
        ]
        .iter()
        .all(|a| actions.contains(*a))
        {
            return Err(error(CastErrorKind::Unsupported));
        }
        let mut caps = ReceiverCapabilities {
            pause: actions.contains("Pause"),
            seek: actions.contains("Seek") && relative,
            ..Default::default()
        };
        let mut volume_range = None;
        if let Some(render) = &description.rendering
            && let Ok((actions, range, _)) = scpd(&client, render).await
        {
            caps.volume =
                actions.contains("SetVolume") && actions.contains("GetVolume") && range.is_some();
            caps.mute = actions.contains("SetMute") && actions.contains("GetMute");
            volume_range = range;
        }
        if let Some(connection) = &description.connection
            && let Ok(xml) = soap(&client, connection, "GetProtocolInfo", &[]).await
        {
            caps.mime_types = protocol_mimes(value(&xml, "Sink").as_deref().unwrap_or(""));
        }
        let mut session = Self {
            client,
            description,
            caps,
            occupied: false,
            volume_range,
        };
        let status = session.snapshot().await?;
        session.occupied = status.content_id.is_some()
            || matches!(
                status.state,
                PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
            );
        Ok(session)
    }
    async fn av(&self, action: &str, mut args: Vec<(&str, String)>) -> Result<String> {
        args.insert(0, ("InstanceID", "0".into()));
        soap(&self.client, &self.description.av, action, &args).await
    }
    pub async fn snapshot(&mut self) -> Result<ReceiverSnapshot> {
        let transport = self.av("GetTransportInfo", vec![]).await?;
        let position = self.av("GetPositionInfo", vec![]).await?;
        let state = match value(&transport, "CurrentTransportState").as_deref() {
            Some("PLAYING") => PlaybackState::Playing,
            Some("PAUSED_PLAYBACK") => PlaybackState::Paused,
            Some("TRANSITIONING") => PlaybackState::Buffering,
            Some("STOPPED" | "NO_MEDIA_PRESENT") => PlaybackState::Idle,
            _ => PlaybackState::Unknown,
        };
        let content =
            value(&position, "TrackURI").filter(|s| !s.is_empty() && s != "NOT_IMPLEMENTED");
        let mut snapshot = ReceiverSnapshot {
            state,
            content_id: content,
            position: parse_time(value(&position, "RelTime").as_deref().unwrap_or(""))
                .unwrap_or(0.),
            duration: parse_time(value(&position, "TrackDuration").as_deref().unwrap_or("")),
            capabilities: self.caps.clone(),
            ..Default::default()
        };
        if value(&transport, "CurrentTransportStatus").as_deref() == Some("ERROR_OCCURRED") {
            snapshot.end_reason = Some(EndReason::Error);
        }
        if let Some(render) = &self.description.rendering {
            if self.caps.volume
                && let Ok(xml) = soap(
                    &self.client,
                    render,
                    "GetVolume",
                    &[("InstanceID", "0".into()), ("Channel", "Master".into())],
                )
                .await
                && let (Some(v), Some((min, max))) = (
                    value(&xml, "CurrentVolume").and_then(|v| v.parse::<u32>().ok()),
                    self.volume_range,
                )
            {
                snapshot.volume =
                    Some((v.saturating_sub(min) as f32 / (max - min) as f32).clamp(0., 1.));
            }
            if self.caps.mute
                && let Ok(xml) = soap(
                    &self.client,
                    render,
                    "GetMute",
                    &[("InstanceID", "0".into()), ("Channel", "Master".into())],
                )
                .await
            {
                snapshot.muted = value(&xml, "CurrentMute").and_then(|v| match v.as_str() {
                    "1" | "true" => Some(true),
                    "0" | "false" => Some(false),
                    _ => None,
                });
            }
        }
        Ok(snapshot)
    }
    pub(crate) async fn inspect_busy(&mut self) -> Result<()> {
        let snapshot = self.snapshot().await?;
        self.occupied = snapshot.content_id.is_some()
            || matches!(
                snapshot.state,
                PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
            );
        Ok(())
    }
    pub async fn load(&mut self, load: &MediaLoad) -> Result<ReceiverSnapshot> {
        if !load.text_tracks.is_empty() {
            return Err(error(CastErrorKind::Unsupported));
        }
        // Without pause support a paused handoff cannot be honored safely.
        if load.paused && !self.caps.pause || load.start_position > 0. && !self.caps.seek {
            return Err(error(CastErrorKind::Unsupported));
        }
        self.av(
            "SetAVTransportURI",
            vec![
                ("CurrentURI", load.url.clone()),
                ("CurrentURIMetaData", didl(load)),
            ],
        )
        .await?;
        // SetURI is stopped; Play is required before seeking on many renderers.
        self.av("Play", vec![("Speed", "1".into())]).await?;
        if load.start_position > 0. {
            self.av(
                "Seek",
                vec![
                    ("Unit", "REL_TIME".into()),
                    ("Target", format_time(load.start_position)),
                ],
            )
            .await?;
        }
        if load.paused {
            self.av("Pause", vec![]).await?;
        }
        self.occupied = true;
        self.snapshot().await
    }
    pub async fn command(&mut self, command: ReceiverCommand) -> Result<()> {
        match command {
            ReceiverCommand::SetPaused(true) => {
                self.av("Pause", vec![]).await?;
            }
            ReceiverCommand::SetPaused(false) => {
                self.av("Play", vec![("Speed", "1".into())]).await?;
            }
            ReceiverCommand::Seek(p) => {
                self.av(
                    "Seek",
                    vec![("Unit", "REL_TIME".into()), ("Target", format_time(p))],
                )
                .await?;
            }
            ReceiverCommand::SetVolume(v) => {
                let (min, max) = self
                    .volume_range
                    .ok_or_else(|| error(CastErrorKind::Unsupported))?;
                let raw = min + ((max - min) as f32 * v).round() as u32;
                let render = self
                    .description
                    .rendering
                    .as_ref()
                    .ok_or_else(|| error(CastErrorKind::Unsupported))?;
                soap(
                    &self.client,
                    render,
                    "SetVolume",
                    &[
                        ("InstanceID", "0".into()),
                        ("Channel", "Master".into()),
                        ("DesiredVolume", raw.to_string()),
                    ],
                )
                .await?;
            }
            ReceiverCommand::SetMuted(m) => {
                let render = self
                    .description
                    .rendering
                    .as_ref()
                    .ok_or_else(|| error(CastErrorKind::Unsupported))?;
                soap(
                    &self.client,
                    render,
                    "SetMute",
                    &[
                        ("InstanceID", "0".into()),
                        ("Channel", "Master".into()),
                        ("DesiredMute", if m { "1" } else { "0" }.into()),
                    ],
                )
                .await?;
            }
            ReceiverCommand::SetTextTrack(_) => return Err(error(CastErrorKind::Unsupported)),
        };
        Ok(())
    }
    pub async fn stop(&self) -> Result<()> {
        self.av("Stop", vec![]).await?;
        Ok(())
    }
}
fn protocol_mimes(s: &str) -> Vec<String> {
    let mut result: Vec<String> = s
        .split(',')
        .take(256)
        .filter_map(|p| {
            let parts: Vec<_> = p.splitn(4, ':').collect();
            if parts.len() == 4 && parts[0] == "http-get" && parts[2] != "*" {
                Some(parts[2].chars().take(128).collect())
            } else {
                None
            }
        })
        .collect();
    result.sort();
    result.dedup();
    result
}
fn didl(load: &MediaLoad) -> String {
    let duration = load
        .duration
        .map(|d| format!(" duration=\"{}\"", format_time(d)))
        .unwrap_or_default();
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\"><item id=\"0\" parentID=\"-1\" restricted=\"1\"><dc:title>{}</dc:title><upnp:class>object.item.videoItem</upnp:class><res protocolInfo=\"http-get:*:{}:*\"{}>{}</res></item></DIDL-Lite>",
        escape(&load.title),
        escape(&load.mime_type),
        duration,
        escape(&load.url)
    )
}
fn parse_time(s: &str) -> Option<f64> {
    let parts: Vec<_> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h = parts[0].parse::<u32>().ok()?;
    let m = parts[1].parse::<u32>().ok()?;
    let sec = parts[2].parse::<f64>().ok()?;
    if m >= 60 || !(0. ..60.).contains(&sec) || !sec.is_finite() {
        return None;
    }
    Some(h as f64 * 3600. + m as f64 * 60. + sec)
}
fn format_time(seconds: f64) -> String {
    let seconds = seconds.max(0.).min(u32::MAX as f64) as u32;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}
/// Cast's standard DIAL description supplies the same UUID used by mDNS, allowing
/// manual and discovered endpoints to retain one identity across address changes.
pub(crate) async fn manual_cast_identity(device: &mut Device) {
    let host = match device.receiver_ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    let Ok(url) = Url::parse(&format!("http://{host}:8008/ssdp/device-desc.xml")) else {
        return;
    };
    let Ok(client) = client() else { return };
    let Ok(response) = client.get(url).send().await else {
        return;
    };
    if !response.status().is_success() {
        return;
    }
    let Ok(xml) = body(response).await else {
        return;
    };
    let Ok(doc) = roxmltree::Document::parse(&xml) else {
        return;
    };
    let Some(node) = doc.descendants().find(|n| n.has_tag_name("device")) else {
        return;
    };
    if let Some(uuid) = child(node, "UDN").filter(|s| s.starts_with("uuid:") && s.len() < 128) {
        device.id = crate::discovery::cast_id(uuid);
    }
    if let Some(name) = child(node, "friendlyName") {
        device.name = name.chars().filter(|c| !c.is_control()).take(128).collect();
    }
    if let Some(model) = child(node, "modelName") {
        device.model = Some(model.chars().take(128).collect());
        device.video = !matches!(
            model,
            "Chromecast Audio"
                | "Google Home"
                | "Google Home Mini"
                | "Google Nest Mini"
                | "Nest Mini"
                | "Nest Audio"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_escapes_every_xml_context() {
        let load = MediaLoad {
            url: "http://host/video?a=1&b=<2>".into(),
            mime_type: "video/mp4\"".into(),
            title: "<tag>&\"'".into(),
            subtitle: None,
            duration: Some(90.),
            start_position: 0.,
            paused: false,
            text_tracks: vec![],
        };
        let xml = didl(&load);
        let doc = roxmltree::Document::parse(&xml).unwrap();
        assert_eq!(
            doc.descendants()
                .find(|n| n.has_tag_name(("http://purl.org/dc/elements/1.1/", "title")))
                .unwrap()
                .text(),
            Some(load.title.as_str())
        );
        assert!(xml.contains("&amp;"));
    }
    #[test]
    fn times_and_protocols_are_strict() {
        assert_eq!(parse_time("01:02:03.5"), Some(3723.5));
        assert_eq!(parse_time("00:90:00"), None);
        assert_eq!(parse_time("00:00:NaN"), None);
        assert_eq!(
            protocol_mimes("http-get:*:video/mp4:*,http-get:*:*:*,rtsp:*:video/x:*"),
            vec!["video/mp4"]
        );
    }
    #[test]
    fn relative_services_stay_on_discovered_host() {
        let xml = "<root><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>TV</friendlyName><UDN>uuid:tv</UDN><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>../control</controlURL><SCPDURL>/schema</SCPDURL></service></serviceList></device></root>";
        let location = Url::parse("http://192.168.1.2/device/root.xml").unwrap();
        let dev = parse_description(xml, &location, "192.168.1.2".parse().unwrap()).unwrap();
        assert_eq!(
            dev.endpoint.unwrap().av.control.as_str(),
            "http://192.168.1.2/control"
        );
        let bad = xml.replace("../control", "http://example.com/control");
        assert!(parse_description(&bad, &location, "192.168.1.2".parse().unwrap()).is_err());
    }
}
