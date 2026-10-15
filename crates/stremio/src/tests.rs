use crate::*;
use serde_json::{Value, json};
use std::sync::Mutex;
use zeroize::Zeroizing;

fn projection(saved: bool, watched: bool, position: u64) -> Projection {
    Projection {
        saved,
        watched: [("tt1:1:1".into(), watched)].into(),
        resume: (position > 0).then(|| Resume {
            video_id: "tt1:1:1".into(),
            position_ms: position,
            duration_ms: 1000,
        }),
    }
}
#[test]
fn initial_merge_unions_history_but_uses_latest_resume() {
    let local = Canonical::from_projection(&projection(true, false, 100), 20);
    let remote = Canonical::from_projection(&projection(false, true, 900), 10);
    let merged = reconcile(&local, &remote, true).projection();
    assert!(merged.saved);
    assert!(merged.watched["tt1:1:1"]);
    assert_eq!(merged.resume.unwrap().position_ms, 100);
}
#[test]
fn subsequent_remove_unwatch_and_rewind_are_events() {
    let local = Canonical::from_projection(&projection(true, true, 900), 20);
    let remote = Canonical::from_projection(&projection(false, false, 100), 21);
    assert_eq!(
        reconcile(&local, &remote, false).projection(),
        remote.projection()
    );
    assert_eq!(
        reconcile(
            &local,
            &Canonical::from_projection(&projection(false, false, 0), 20),
            false
        )
        .projection(),
        projection(false, false, 0)
    );
}
#[test]
fn unrelated_remote_changes_do_not_freshen_watched_clocks() {
    let mut remote = Canonical::from_projection(&projection(true, true, 100), 10);
    remote.observe(&projection(true, true, 200), 30);
    let local = Canonical::from_projection(&projection(true, false, 100), 20);
    let merged = reconcile(&local, &remote, false);
    assert!(!merged.watched["tt1:1:1"].value);
    assert_eq!(merged.resume.value.unwrap().position_ms, 200);
    remote.observe(&Projection::default(), 40);
    assert_eq!(remote.watched["tt1:1:1"].changed_at, 10);
}
fn videos(count: u32) -> Vec<Video> {
    (1..=count)
        .map(|n| Video {
            id: format!("tt2934286:1:{n}"),
            season: Some(1),
            episode: Some(n),
            released_ms: None,
        })
        .collect()
}
#[test]
fn official_watched_fixture_and_sorting_roundtrip() {
    let mut videos = videos(9);
    videos.reverse();
    let decoded = decode_watched("tt2934286:1:5:5:eJyTZwAAAEAAIA==", &videos).unwrap();
    for n in 1..=9 {
        assert_eq!(decoded[&format!("tt2934286:1:{n}")], n <= 5);
    }
    let encoded = encode_watched(&decoded, &videos).unwrap();
    assert_eq!(decode_watched(&encoded, &videos).unwrap(), decoded);
}
#[test]
fn watched_anchor_shifts_after_new_earlier_video() {
    let old = videos(9);
    let mut new = old.clone();
    new.push(Video {
        id: "special".into(),
        season: Some(0),
        episode: Some(0),
        released_ms: None,
    });
    let decoded = decode_watched("tt2934286:1:5:5:eJyTZwAAAEAAIA==", &new).unwrap();
    assert!(!decoded["special"]);
    assert!(decoded["tt2934286:1:5"]);
    assert!(!decoded["tt2934286:1:6"]);
}

#[test]
fn official_empty_watched_inventory_keeps_membership_and_resume_importable() {
    let videos = videos(9);
    let item: LibraryItem = serde_json::from_value(json!({
        "_id":"tt2934286", "type":"series", "removed":false, "temp":false,
        "state": {
            "watched":"undefined:1:eJwDAAAAAAE=",
            "video_id":"tt2934286:1:3", "timeOffset":12_000, "duration":60_000,
            // A series counter does not identify which episodes were watched.
            "timesWatched":1, "flaggedWatched":1
        }
    }))
    .unwrap();
    let projection = item.projection(&videos).unwrap();
    assert!(projection.saved);
    assert_eq!(projection.watched.len(), videos.len());
    assert!(projection.watched.values().all(|watched| !watched));
    assert_eq!(
        projection.resume,
        Some(Resume {
            video_id: "tt2934286:1:3".into(),
            position_ms: 12_000,
            duration_ms: 60_000,
        })
    );
}

#[test]
fn missing_watched_anchor_accepts_only_validated_empty_history() {
    let videos = videos(9);
    // A nonempty, zero-filled buffer is just as unambiguous as the official
    // zero-byte sentinel; it needs no anchor to align its false bits.
    let empty = encode_watched(&BTreeMap::new(), &videos).unwrap();
    let payload = empty.rsplit_once(':').unwrap().1;
    let encoded = format!("missing:1:{payload}");
    let decoded = decode_watched(&encoded, &videos).unwrap();
    assert_eq!(decoded.len(), videos.len());
    assert!(decoded.values().all(|watched| !watched));
    let mut invalid = videos.clone();
    invalid[0].id.clear();
    assert_eq!(
        decode_watched(&encoded, &invalid),
        Err(ApiError::InvalidInput)
    );

    let nonempty = encode_watched(&[(videos[0].id.clone(), true)].into(), &videos).unwrap();
    let payload = nonempty.rsplit_once(':').unwrap().1;
    let encoded = format!("missing:1:{payload}");
    assert_eq!(
        decode_watched(&encoded, &videos),
        Err(ApiError::InvalidResponse)
    );
    assert_eq!(
        decode_watched(&encoded, &[]),
        Err(ApiError::InvalidResponse)
    );
}

#[test]
fn malformed_and_expanding_bitmaps_are_bounded() {
    assert_eq!(
        decode_watched("a:999999999999999999999999:xxx", &videos(9)),
        Err(ApiError::InvalidResponse)
    );
    assert_eq!(
        decode_watched("wrong:5:eJyTZwAAAEAAIA==", &videos(9)),
        Err(ApiError::InvalidResponse)
    );
    use base64::Engine;
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(&vec![0; 100_000]).unwrap();
    let payload = base64::engine::general_purpose::STANDARD.encode(encoder.finish().unwrap());
    assert_eq!(
        decode_watched(&format!("tt2934286:1:1:1:{payload}"), &videos(9)),
        Err(ApiError::InvalidResponse)
    );
}
#[test]
fn edits_preserve_unknown_fields_and_metadata_unavailable_bitmap() {
    let mut item:LibraryItem=serde_json::from_value(json!({"_id":"tt1","type":"series","future":{"foo":1},"state":{"watched":"opaque","custom":123,"timeOffset":100,"video_id":"ep1"}})).unwrap();
    assert_eq!(item.projection(&[]), Err(ApiError::InvalidResponse));
    // Applying membership without metadata must never erase opaque history.
    item.apply_projection(
        &Projection {
            saved: true,
            ..Default::default()
        },
        &[],
        1000,
    )
    .unwrap();
    assert_eq!(item.0["state"]["watched"], "opaque");
    assert_eq!(item.0["state"]["custom"], 123);
    assert_eq!(item.0["future"]["foo"], 1);
}

struct Mock {
    responses: Mutex<Vec<Response>>,
    requests: Mutex<Vec<(String, Value)>>,
}
impl Mock {
    fn one(status: u16, value: Value) -> Self {
        Self {
            responses: Mutex::new(vec![Response {
                status,
                retry_after: Some(17),
                body: Zeroizing::new(serde_json::to_vec(&value).unwrap()),
            }]),
            requests: Mutex::new(Vec::new()),
        }
    }
}

#[test]
fn request_failures_keep_server_status_and_numeric_code_without_payloads() {
    let key = Secret::new("private");
    for status in [400, 404, 503] {
        let client = Client::new(Mock::one(status, json!({"message":"private server text"})));
        let error = client.library_meta(&key, 100).unwrap_err();
        assert_eq!(error, ApiError::Http { status });
        assert!(!format!("{error:?}").contains("private"));
    }
    let client = Client::new(Mock::one(
        200,
        json!({"error":{"code":18,"message":"private server text"}}),
    ));
    let error = client.library_meta(&key, 100).unwrap_err();
    assert_eq!(error, ApiError::RemoteRejected { code: Some(18) });
    assert!(!format!("{error:?}").contains("private"));
    let client = Client::new(Mock::one(200, json!({"result":{"success":false}})));
    assert_eq!(
        client.library_put(&key, &[], 100),
        Err(ApiError::RemoteRejected { code: None })
    );
}

#[test]
fn null_error_does_not_override_a_successful_result() {
    let client = Client::new(Mock::one(
        200,
        json!({"error":null,"result":{"success":true}}),
    ));
    client
        .library_put(&Secret::new("private"), &[], 100)
        .unwrap();
}
impl Transport for Mock {
    fn send(&self, request: Request) -> Result<Response, ApiError> {
        self.requests.lock().unwrap().push((
            request.url.clone(),
            serde_json::from_slice(&request.body).unwrap_or(Value::Null),
        ));
        Ok(self.responses.lock().unwrap().remove(0))
    }
}
#[test]
fn official_library_get_request_shape_and_unknown_fields() {
    let mock = Mock::one(
        200,
        json!({"result":[{"_id":"tt1","state":{},"unknown":true}]}),
    );
    let client = Client::new(mock);
    let result = client
        .library_get(&Secret::new("private"), &[], true, 0)
        .unwrap();
    assert_eq!(result[0].0["unknown"], true);
}
#[test]
fn safe_errors_and_rate_limit() {
    let client = Client::new(Mock::one(429, json!({"error":{"message":"private"}})));
    assert_eq!(
        client.get_user(&Secret::new("private"), 100).unwrap_err(),
        ApiError::RateLimited { retry_at: 117 }
    );
    let client = Client::new(Mock::one(
        200,
        json!({"error":{"code":1,"message":"private"}}),
    ));
    assert_eq!(
        client.get_user(&Secret::new("private"), 0).unwrap_err(),
        ApiError::Authentication
    );
    assert!(!format!("{:?}", Secret::new("private")).contains("private"));
}
#[test]
fn pending_link_and_approved_key() {
    let client = Client::new(Mock::one(200, json!({"result":null})));
    assert!(client.read_link(&Secret::new("CODE"), 0).unwrap().is_none());
    let client = Client::new(Mock::one(200, json!({"result":{"authKey":"private"}})));
    assert_eq!(
        client
            .read_link(&Secret::new("CODE"), 0)
            .unwrap()
            .unwrap()
            .expose(),
        "private"
    );
}
