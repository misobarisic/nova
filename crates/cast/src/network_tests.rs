//! Local protocol fixtures exercise real sockets. The TLS key is disposable test data.
use super::*;
use serde_json::{Value, json};
use std::sync::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
fn load() -> MediaLoad {
    MediaLoad {
        url: "http://192.0.2.1/token/video.mp4".into(),
        mime_type: "video/mp4".into(),
        title: "Movie <&>".into(),
        subtitle: None,
        duration: Some(120.),
        start_position: 12.,
        paused: true,
        text_tracks: vec![],
    }
}
fn cast_frame(namespace: &str, payload: &Value) -> Vec<u8> {
    fn varint(mut value: usize, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 127) as u8;
            value >>= 7;
            out.push(byte | if value > 0 { 128 } else { 0 });
            if value == 0 {
                break;
            }
        }
    }
    let mut bytes = vec![8, 0];
    for (field, s) in [
        (2, "receiver-0".to_owned()),
        (3, "sender-0".to_owned()),
        (4, namespace.to_owned()),
    ] {
        bytes.push((field << 3) | 2);
        varint(s.len(), &mut bytes);
        bytes.extend_from_slice(s.as_bytes());
    }
    bytes.extend_from_slice(&[40, 0]);
    let s = payload.to_string();
    bytes.push(50);
    varint(s.len(), &mut bytes);
    bytes.extend_from_slice(s.as_bytes());
    let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
    frame.extend(bytes);
    frame
}
async fn cast_read(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> Option<(String, Value)> {
    fn varint(bytes: &[u8], offset: &mut usize) -> Option<usize> {
        let mut value = 0;
        for shift in (0..35).step_by(7) {
            let byte = *bytes.get(*offset)?;
            *offset += 1;
            value |= ((byte & 127) as usize) << shift;
            if byte & 128 == 0 {
                return Some(value);
            }
        }
        None
    }
    let len = stream.read_u32().await.ok()? as usize;
    if len > 1024 * 1024 {
        return None;
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await.ok()?;
    let mut offset = 0;
    let mut namespace = None;
    let mut payload = None;
    while offset < bytes.len() {
        let key = varint(&bytes, &mut offset)?;
        match key & 7 {
            0 => {
                varint(&bytes, &mut offset)?;
            }
            2 => {
                let n = varint(&bytes, &mut offset)?;
                let text = std::str::from_utf8(bytes.get(offset..offset + n)?).ok()?;
                if key >> 3 == 4 {
                    namespace = Some(text.to_owned());
                }
                if key >> 3 == 6 {
                    payload = Some(serde_json::from_str(text).ok()?);
                }
                offset += n;
            }
            _ => return None,
        }
    }
    Some((namespace?, payload?))
}

struct CastMock {
    address: SocketAddr,
    commands: Arc<Mutex<Vec<String>>>,
    takeover: Arc<AtomicBool>,
    closed: tokio::sync::oneshot::Receiver<()>,
}
async fn cast_mock(occupied: bool) -> CastMock {
    cast_mock_on(occupied, "127.0.0.1:0").await
}
async fn cast_mock_on(occupied: bool, bind: &str) -> CastMock {
    let listener = TcpListener::bind(bind).await.unwrap();
    let address = listener.local_addr().unwrap();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let server_commands = commands.clone();
    let takeover = Arc::new(AtomicBool::new(false));
    let server_takeover = takeover.clone();
    let (closed_tx, closed) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let cert = rustls::pki_types::CertificateDer::from(
            include_bytes!("../tests/fixtures/cert.der").to_vec(),
        );
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(
            include_bytes!("../tests/fixtures/key.der").to_vec(),
        );
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key.into())
        .unwrap();
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = tokio_rustls::TlsAcceptor::from(Arc::new(config))
            .accept(tcp)
            .await
            .unwrap();
        let mut active = occupied;
        let mut media: Option<Value> = None;
        let mut paused = false;
        let mut position = 0.;
        while let Some((request_namespace, request)) = cast_read(&mut stream).await {
            let kind = request["type"].as_str().unwrap_or("");
            server_commands.lock().unwrap().push(kind.to_owned());
            if kind == "CONNECT" {
                continue;
            }
            if kind == "PING" {
                let pong = cast_frame(
                    "urn:x-cast:com.google.cast.tp.heartbeat",
                    &json!({"type":"PONG"}),
                );
                if stream.write_all(&pong).await.is_err() {
                    break;
                }
                continue;
            }
            let id = request["requestId"].clone();
            let response;
            let namespace;
            if request_namespace == "urn:x-cast:com.google.cast.receiver" {
                if kind == "LAUNCH" {
                    active = true;
                }
                namespace = "urn:x-cast:com.google.cast.receiver";
                response = json!({"requestId":id,"type":"RECEIVER_STATUS","status":{"volume":{"level":0.5,"muted":false},"applications":if active {json!([{"appId":"CC1AD845","sessionId":"app-1","transportId":"transport-1","displayName":"Player","namespaces":[{"name":"urn:x-cast:com.google.cast.media"}]}])}else{json!([])}}});
            } else if kind == "GET_STATUS" {
                namespace = "urn:x-cast:com.google.cast.media";
                response = json!({"requestId":id,"type":"MEDIA_STATUS","status":[{"mediaSessionId":if server_takeover.load(Ordering::Acquire){9}else{7},"playerState":if paused{"PAUSED"}else{"PLAYING"},"currentTime":position,"supportedMediaCommands":3,"media":media}]});
            } else {
                namespace = "urn:x-cast:com.google.cast.media";
                if kind == "LOAD" {
                    media = Some(request["media"].clone());
                    paused = !request["autoplay"].as_bool().unwrap();
                    position = request["currentTime"].as_f64().unwrap();
                }
                if kind == "PAUSE" {
                    paused = true;
                }
                if kind == "PLAY" {
                    paused = false;
                }
                if kind == "SEEK" {
                    position = request["currentTime"].as_f64().unwrap();
                }
                response = json!({"requestId":id,"type":"MEDIA_STATUS","status":[{"mediaSessionId":7,"playerState":if paused{"PAUSED"}else{"PLAYING"},"currentTime":position,"supportedMediaCommands":3,"media":media}]});
            }
            if stream
                .write_all(&cast_frame(namespace, &response))
                .await
                .is_err()
            {
                break;
            }
        }
        let _ = closed_tx.send(());
    });
    CastMock {
        address,
        commands,
        takeover,
        closed,
    }
}
fn cast_device(address: SocketAddr) -> Device {
    Device {
        id: "cast:test".into(),
        name: "Test TV".into(),
        protocol: Protocol::GoogleCast,
        address,
        receiver_ip: address.ip(),
        video: true,
        model: None,
        endpoint: None,
    }
}
#[tokio::test]
async fn cast_close_transport_never_sends_stop_or_launch() {
    let mock = cast_mock(true).await;
    let mut session =
        ReceiverSession::connect(cast_device(mock.address), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
    assert!(session.needs_replacement());
    assert_eq!(
        session.load(load()).await.err().unwrap().kind,
        CastErrorKind::ReplacementRequired
    );
    session.close_transport().await;
    tokio::time::timeout(Duration::from_secs(2), mock.closed)
        .await
        .unwrap()
        .unwrap();
    let commands = mock.commands.lock().unwrap();
    assert!(
        !commands
            .iter()
            .any(|s| matches!(s.as_str(), "STOP" | "LAUNCH" | "LOAD"))
    );
}
#[test]
fn finished_status_keeps_ownership_but_takeover_revokes_it_permanently() {
    let mut session = ReceiverSession {
        transport: Transport::Closed,
        cancellation: Arc::new(AtomicBool::new(false)),
        replacement_authorized: false,
        owned: Some(Identity {
            app: Some("app".into()),
            media: Some("7".into()),
            content: "http://private/token".into(),
        }),
        takeover: false,
    };
    let finished = ReceiverSnapshot {
        app_session: Some("app".into()),
        media_session: Some("7".into()),
        state: PlaybackState::Idle,
        end_reason: Some(EndReason::Finished),
        ..Default::default()
    };
    assert!(session.classify(finished).unwrap().owned);
    let changed = ReceiverSnapshot {
        app_session: Some("app".into()),
        media_session: Some("8".into()),
        content_id: Some("http://other".into()),
        state: PlaybackState::Playing,
        ..Default::default()
    };
    let status = session.classify(changed).unwrap();
    assert!(!status.owned);
    assert_eq!(status.end_reason, Some(EndReason::Takeover));
    let original = ReceiverSnapshot {
        app_session: Some("app".into()),
        media_session: Some("7".into()),
        content_id: Some("http://private/token".into()),
        state: PlaybackState::Playing,
        ..Default::default()
    };
    assert!(!session.classify(original).unwrap().owned);
}
#[tokio::test]
async fn cancellation_aborts_waiting_operation() {
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        c.store(true, Ordering::Release);
    });
    let result: Result<()> = bounded(&cancel, std::future::pending()).await;
    assert_eq!(result.unwrap_err().kind, CastErrorKind::Cancelled);
}

#[tokio::test]
async fn cast_load_control_takeover_and_close_use_real_tls() {
    let mock = cast_mock(false).await;
    let mut session =
        ReceiverSession::connect(cast_device(mock.address), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
    assert!(!session.needs_replacement());
    let snapshot = session.load(load()).await.unwrap();
    assert!(snapshot.owned);
    assert_eq!(snapshot.state, PlaybackState::Paused);
    assert_eq!(snapshot.position, 12.);
    let snapshot = session.command(ReceiverCommand::Seek(30.)).await.unwrap();
    assert_eq!(snapshot.position, 30.);
    let snapshot = session
        .command(ReceiverCommand::SetPaused(false))
        .await
        .unwrap();
    assert_eq!(snapshot.state, PlaybackState::Playing);
    mock.takeover.store(true, Ordering::Release);
    assert_eq!(
        session
            .command(ReceiverCommand::SetPaused(true))
            .await
            .err()
            .unwrap()
            .kind,
        CastErrorKind::Takeover
    );
    assert_eq!(
        session.stop_owned_media().await.unwrap_err().kind,
        CastErrorKind::Takeover
    );
    assert_eq!(
        session.load(load()).await.err().unwrap().kind,
        CastErrorKind::Takeover
    );
    assert_eq!(
        mock.commands
            .lock()
            .unwrap()
            .iter()
            .filter(|s| *s == "LOAD")
            .count(),
        1
    );
    session.close_transport().await;
    tokio::time::timeout(Duration::from_secs(2), mock.closed)
        .await
        .unwrap()
        .unwrap();
    assert!(!mock.commands.lock().unwrap().iter().any(|s| s == "STOP"));
}

#[derive(Default)]
struct DlnaState {
    uri: String,
    position: String,
    playing: bool,
    paused: bool,
    stops: usize,
    metadata: String,
    fail_pause: bool,
    fail_after_play: bool,
    loads: usize,
}
async fn http_request(stream: &mut tokio::net::TcpStream) -> Option<(String, String)> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 2048];
    let header_end;
    loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 || bytes.len() + n > 65536 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = end + 4;
            break;
        }
    }
    let headers = std::str::from_utf8(&bytes[..header_end]).ok()?.to_owned();
    let len = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            if key.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    if len > 65536 {
        return None;
    }
    while bytes.len() < header_end + len {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    Some((
        headers,
        String::from_utf8(bytes[header_end..header_end + len].to_vec()).ok()?,
    ))
}
async fn dlna_mock() -> (
    reqwest::Url,
    Arc<Mutex<DlnaState>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = reqwest::Url::parse(&format!(
        "http://{}/device/description.xml",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let state = Arc::new(Mutex::new(DlnaState {
        position: "00:00:00".into(),
        ..Default::default()
    }));
    let shared = state.clone();
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let shared = shared.clone();
            tokio::spawn(async move {
                let Some((headers, body)) = http_request(&mut stream).await else {
                    return;
                };
                let mut status = "200 OK";
                let response = if headers.starts_with("GET /device/description.xml ") {
                    "<root><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:2</deviceType><friendlyName>Mock TV</friendlyName><UDN>uuid:mock</UDN><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:2</serviceType><controlURL>../control</controlURL><SCPDURL>../schema</SCPDURL></service></serviceList></device></root>".to_owned()
                } else if headers.starts_with("GET /schema ") {
                    let actions = [
                        "SetAVTransportURI",
                        "Play",
                        "Pause",
                        "Stop",
                        "Seek",
                        "GetTransportInfo",
                        "GetPositionInfo",
                    ]
                    .iter()
                    .map(|a| format!("<action><name>{a}</name></action>"))
                    .collect::<String>();
                    format!(
                        "<scpd><actionList>{actions}</actionList><serviceStateTable><stateVariable><name>A_ARG_TYPE_SeekMode</name><allowedValueList><allowedValue>REL_TIME</allowedValue></allowedValueList></stateVariable></serviceStateTable></scpd>"
                    )
                } else {
                    let doc = roxmltree::Document::parse(&body).unwrap();
                    let action = doc
                        .descendants()
                        .find(|n| n.has_tag_name("Body"))
                        .unwrap()
                        .children()
                        .find(|n| n.is_element())
                        .unwrap();
                    let kind = action.tag_name().name();
                    let val = |key: &str| {
                        action
                            .children()
                            .find(|n| n.has_tag_name(key))
                            .and_then(|n| n.text())
                            .unwrap_or("")
                            .to_owned()
                    };
                    let mut state = shared.lock().unwrap();
                    let content = match kind {
                        "SetAVTransportURI" => {
                            state.loads += 1;
                            state.uri = val("CurrentURI");
                            state.metadata = val("CurrentURIMetaData");
                            String::new()
                        }
                        "Play" => {
                            state.playing = true;
                            state.paused = false;
                            String::new()
                        }
                        "Pause" => {
                            if state.fail_pause {
                                status = "500 Internal Server Error";
                                "<Fault><faultcode>s:Client</faultcode><detail><UPnPError><errorCode>701</errorCode><errorDescription>secret URL must not escape</errorDescription></UPnPError></detail></Fault>".into()
                            } else {
                                state.paused = true;
                                String::new()
                            }
                        }
                        "Stop" => {
                            state.playing = false;
                            state.paused = false;
                            state.stops += 1;
                            String::new()
                        }
                        "Seek" => {
                            state.position = val("Target");
                            String::new()
                        }
                        "GetTransportInfo" => format!(
                            "<CurrentTransportState>{}</CurrentTransportState><CurrentTransportStatus>OK</CurrentTransportStatus>",
                            if state.paused {
                                "PAUSED_PLAYBACK"
                            } else if state.playing {
                                "PLAYING"
                            } else {
                                "STOPPED"
                            }
                        ),
                        "GetPositionInfo" => {
                            if state.fail_after_play && state.playing {
                                state.fail_after_play = false;
                                status = "500 Internal Server Error";
                                "<Fault><faultcode>s:Server</faultcode></Fault>".into()
                            } else {
                                format!(
                                    "<Track>1</Track><TrackDuration>00:02:00</TrackDuration><TrackURI>{}</TrackURI><RelTime>{}</RelTime>",
                                    crate::dlna::escape(&state.uri),
                                    state.position
                                )
                            }
                        }
                        _ => panic!("unexpected SOAP action"),
                    };
                    if status.starts_with("500") {
                        format!(
                            "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>{content}</s:Body></s:Envelope>"
                        )
                    } else {
                        format!(
                            "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:{kind}Response xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:2\">{content}</u:{kind}Response></s:Body></s:Envelope>"
                        )
                    }
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (url, state, task)
}
#[tokio::test]
async fn dlna_load_preserves_pause_seek_and_escaped_metadata_and_rejects_takeover() {
    let (url, state, task) = dlna_mock().await;
    let device = dlna::description(url, "127.0.0.1".parse().unwrap())
        .await
        .unwrap();
    let mut session = ReceiverSession::connect(device, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert!(!session.needs_replacement());
    let snapshot = session.load(load()).await.unwrap();
    assert!(snapshot.owned);
    assert_eq!(snapshot.state, PlaybackState::Paused);
    assert_eq!(snapshot.position, 12.);
    assert!(snapshot.capabilities.pause);
    assert!(snapshot.capabilities.seek);
    assert!(!snapshot.capabilities.volume);
    let metadata = state.lock().unwrap().metadata.clone();
    let doc = roxmltree::Document::parse(&metadata).unwrap();
    assert_eq!(
        doc.descendants()
            .find(|n| n.tag_name().name() == "title")
            .unwrap()
            .text(),
        Some("Movie <&>")
    );
    let snapshot = session.command(ReceiverCommand::Seek(42.)).await.unwrap();
    assert_eq!(snapshot.position, 42.);
    state.lock().unwrap().uri = "http://other-sender/media".into();
    assert_eq!(
        session.stop_owned_media().await.unwrap_err().kind,
        CastErrorKind::Takeover
    );
    assert_eq!(state.lock().unwrap().stops, 0);
    session.close_transport().await;
    assert_eq!(state.lock().unwrap().stops, 0);
    task.abort();
}
#[tokio::test]
async fn dlna_fault_is_sanitized_and_mutation_is_not_retried() {
    let (url, state, task) = dlna_mock().await;
    let device = dlna::description(url, "127.0.0.1".parse().unwrap())
        .await
        .unwrap();
    let mut session = ReceiverSession::connect(device, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    let mut media = load();
    media.paused = false;
    session.load(media).await.unwrap();
    state.lock().unwrap().fail_pause = true;
    let error = session
        .command(ReceiverCommand::SetPaused(true))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, CastErrorKind::Protocol);
    assert!(!error.to_string().contains("secret"));
    assert_eq!(
        session.snapshot().await.unwrap().state,
        PlaybackState::Playing
    );
    session.stop_owned_media().await.unwrap();
    assert_eq!(state.lock().unwrap().stops, 1);
    task.abort();
}

#[tokio::test]
async fn cancelled_setup_can_still_stop_its_exact_owned_media() {
    let (url, state, task) = dlna_mock().await;
    let device = dlna::description(url, "127.0.0.1".parse().unwrap())
        .await
        .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut session = ReceiverSession::connect(device, cancel.clone())
        .await
        .unwrap();
    session.load(load()).await.unwrap();
    cancel.store(true, Ordering::Release);
    assert_eq!(
        session.snapshot().await.err().unwrap().kind,
        CastErrorKind::Cancelled
    );
    session.stop_owned_media().await.unwrap();
    assert_eq!(state.lock().unwrap().stops, 1);
    task.abort();
}
#[tokio::test]
async fn playback_started_after_selection_requires_fresh_confirmation() {
    let (url, state, task) = dlna_mock().await;
    let device = dlna::description(url, "127.0.0.1".parse().unwrap())
        .await
        .unwrap();
    let mut session = ReceiverSession::connect(device, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    state.lock().unwrap().uri = "http://another-sender/current".into();
    assert_eq!(
        session.load(load()).await.err().unwrap().kind,
        CastErrorKind::ReplacementRequired
    );
    assert_eq!(state.lock().unwrap().uri, "http://another-sender/current");
    assert_eq!(state.lock().unwrap().stops, 0);
    session.authorize_replacement();
    assert!(session.load(load()).await.unwrap().owned);
    task.abort();
}

#[tokio::test]
async fn cast_ipv6_literal_connects_and_closes_without_stop() {
    let mock = cast_mock_on(false, "[::1]:0").await;
    let mut session =
        ReceiverSession::connect(cast_device(mock.address), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
    assert!(!session.needs_replacement());
    session.close_transport().await;
    tokio::time::timeout(Duration::from_secs(2), mock.closed)
        .await
        .unwrap()
        .unwrap();
    assert!(!mock.commands.lock().unwrap().iter().any(|s| s == "STOP"));
}

#[tokio::test]
async fn uncertain_load_is_observed_without_replaying_or_losing_cleanup_ownership() {
    let (url, state, task) = dlna_mock().await;
    let device = dlna::description(url, "127.0.0.1".parse().unwrap())
        .await
        .unwrap();
    let mut session = ReceiverSession::connect(device, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    state.lock().unwrap().fail_after_play = true;
    let snapshot = session.load(load()).await.unwrap();
    assert!(snapshot.owned);
    assert_eq!(snapshot.state, PlaybackState::Paused);
    assert_eq!(state.lock().unwrap().loads, 1);
    session.stop_owned_media().await.unwrap();
    assert_eq!(state.lock().unwrap().stops, 1);
    task.abort();
}
