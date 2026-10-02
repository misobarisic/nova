use crate::{api::*, auth::*, *};
use serde_json::{Value, json};
use std::sync::Mutex;
struct Fixture {
    replies: Mutex<Vec<Response>>,
    requests: Mutex<Vec<Request>>,
}
impl Fixture {
    fn new(values: Vec<Value>) -> Self {
        Self {
            replies: Mutex::new(
                values
                    .into_iter()
                    .rev()
                    .map(|body| Response {
                        status: 200,
                        retry_after: None,
                        reset_at: None,
                        body,
                    })
                    .collect(),
            ),
            requests: Mutex::new(vec![]),
        }
    }
}
impl Transport for &Fixture {
    fn send(&self, request: Request) -> Result<Response, ApiError> {
        self.requests.lock().unwrap().push(request);
        self.replies.lock().unwrap().pop().ok_or(ApiError::Offline)
    }
}
fn viewer() -> Value {
    json!({"data":{"Viewer":{"id":7,"name":"test","mediaListOptions":{"scoreFormat":"POINT_10_DECIMAL"}}}})
}
fn entry(progress: u32) -> Value {
    json!({"id":99,"userId":7,"mediaId":12,"progress":progress,"status":"CURRENT","score":8.5,"startedAt":{"year":2026,"month":null,"day":null},"completedAt":{"year":null,"month":null,"day":null}})
}
fn client(f: &Fixture) -> Client<&Fixture> {
    Client::new(
        Service::AniList,
        Secret::new("test-token".into()).unwrap(),
        f,
    )
}
#[test]
fn anilist_progress_mutation_omits_unrelated_fields() {
    let f = Fixture::new(vec![
        viewer(),
        json!({"data":{"Media":{"id":12,"mediaListEntry":entry(4)}}}),
        json!({"data":{"SaveMediaListEntry":entry(5)}}),
    ]);
    let mut c = client(&f);
    c.verify(0).unwrap();
    let old = c.read(12.try_into().unwrap(), 0).unwrap().unwrap();
    let result = c
        .update(
            12.try_into().unwrap(),
            Some(&old),
            &EntryPatch {
                progress: Some(5),
                ..Default::default()
            },
            0,
        )
        .unwrap();
    assert_eq!(result.score_tenths, 85);
    let requests = f.requests.lock().unwrap();
    let Body::Json(body) = &requests[2].body else {
        panic!()
    };
    let query = body["query"].as_str().unwrap();
    assert!(!query.contains("score:$score"));
    assert!(!query.contains("status:$status"));
    assert!(!query.contains("startedAt:$startedAt"));
    assert_eq!(body["variables"]["id"], 99);
    assert_eq!(body["variables"]["mediaId"], 12);
}
#[test]
fn graphql_partial_mutation_errors_are_failures() {
    let f = Fixture::new(vec![
        viewer(),
        json!({"data":{"SaveMediaListEntry":entry(5)},"errors":[{"message":"failure"}]}),
    ]);
    let mut c = client(&f);
    c.verify(0).unwrap();
    assert_eq!(
        c.update(
            12.try_into().unwrap(),
            None,
            &EntryPatch {
                progress: Some(5),
                ..Default::default()
            },
            0
        ),
        Err(ApiError::Rejected)
    );
}
#[test]
fn wrong_remote_account_is_rejected() {
    let mut e = entry(4);
    e["userId"] = json!(8);
    let f = Fixture::new(vec![
        viewer(),
        json!({"data":{"Media":{"id":12,"mediaListEntry":e}}}),
    ]);
    let mut c = client(&f);
    c.verify(0).unwrap();
    assert_eq!(
        c.read(12.try_into().unwrap(), 0),
        Err(ApiError::WrongAccount)
    );
}
#[test]
fn absence_requires_a_successful_scoped_read() {
    let f = Fixture::new(vec![
        viewer(),
        json!({"data":{"Media":{"id":12,"mediaListEntry":null}}}),
        json!({"errors":[{"status":404}]}),
    ]);
    let mut c = client(&f);
    c.verify(0).unwrap();
    assert_eq!(c.read(12.try_into().unwrap(), 0), Ok(None));
    assert_eq!(c.read(12.try_into().unwrap(), 0), Err(ApiError::Rejected));
}
#[test]
fn mal_uses_patch_and_keeps_score_and_rewatching() {
    let value = json!({"status":"watching","score":9,"num_episodes_watched":5,"is_rewatching":true,"start_date":"2026-01-02","finish_date":null});
    let f = Fixture::new(vec![json!({"id":7,"name":"test"}), value]);
    let mut c = Client::new(
        Service::MyAnimeList,
        Secret::new("test".into()).unwrap(),
        &f,
    );
    c.verify(0).unwrap();
    let result = c
        .update(
            12.try_into().unwrap(),
            None,
            &EntryPatch {
                progress: Some(5),
                ..Default::default()
            },
            0,
        )
        .unwrap();
    assert!(result.repeating);
    assert_eq!(result.score_tenths, 90);
    let requests = f.requests.lock().unwrap();
    assert_eq!(requests[1].method, "PATCH");
    let Body::Form(form) = &requests[1].body else {
        panic!()
    };
    assert_eq!(form, &[("num_watched_episodes".into(), "5".into())]);
}
#[test]
fn rate_limit_is_preserved_on_non_json_response() {
    let response = Response {
        status: 429,
        retry_after: Some(120),
        reset_at: None,
        body: Value::Null,
    };
    assert_eq!(
        checked(response, 100),
        Err(ApiError::RateLimited { retry_at: 220 })
    );
}
#[test]
fn scores_and_capabilities_validate_before_requests() {
    assert!(ScoreFormat::Point10Decimal.valid(85));
    assert!(!ScoreFormat::Point10.valid(85));
    assert!(
        ListDate {
            year: Some(2024),
            month: Some(2),
            day: Some(29)
        }
        .valid()
    );
    assert!(
        !ListDate {
            year: Some(2025),
            month: Some(2),
            day: Some(29)
        }
        .valid()
    );
    assert_eq!(
        EntryPatch {
            started: Some(ListDate::default()),
            ..Default::default()
        }
        .validate(Service::MyAnimeList, ScoreFormat::Point10),
        Err(ApiError::UnsupportedField)
    );
}
#[test]
fn mal_pkce_checks_state_before_token_exchange() {
    let reg = ClientRegistration {
        client_id: "nova-test".into(),
        redirect_uri: "http://127.0.0.1:53926/callback".into(),
    };
    let auth = Authorization::begin(Service::MyAnimeList, reg).unwrap();
    let url = url::Url::parse(&auth.url().unwrap()).unwrap();
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs["code_challenge_method"], "plain");
    assert_eq!(pairs["code_challenge"].len(), 64);
    let f = Fixture::new(vec![]);
    assert!(matches!(
        auth.finish(
            "http://127.0.0.1:53926/callback?code=foo&state=wrong",
            &&f,
            0
        ),
        Err(ApiError::Authentication)
    ));
    assert!(f.requests.lock().unwrap().is_empty());
}
#[test]
fn mal_exchange_uses_public_client_and_expiry() {
    let reg = ClientRegistration {
        client_id: "nova-test".into(),
        redirect_uri: "http://127.0.0.1:53926/callback".into(),
    };
    let auth = Authorization::begin(Service::MyAnimeList, reg).unwrap();
    let url = url::Url::parse(&auth.url().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let f = Fixture::new(vec![
        json!({"access_token":"access","refresh_token":"refresh","token_type":"Bearer","expires_in":3600}),
    ]);
    let tokens = auth
        .finish(
            &format!("http://127.0.0.1:53926/callback?code=code&state={state}"),
            &&f,
            100,
        )
        .unwrap();
    assert_eq!(tokens.expires_at, Some(3700));
    let requests = f.requests.lock().unwrap();
    let Body::Form(form) = &requests[0].body else {
        panic!()
    };
    assert!(!form.iter().any(|(k, _)| k == "client_secret"));
    assert!(format!("{:?}", tokens.access).contains("redacted"));
}
#[test]
fn pin_fallback_requires_pin_registration() {
    let reg = ClientRegistration {
        client_id: "123".into(),
        redirect_uri: "https://anilist.co/api/v2/oauth/pin".into(),
    };
    assert!(
        Authorization::begin(Service::AniList, reg)
            .unwrap()
            .finish_pin("token".into())
            .is_ok()
    );
    let reg = ClientRegistration {
        client_id: "123".into(),
        redirect_uri: "http://127.0.0.1:53926/callback".into(),
    };
    assert!(
        Authorization::begin(Service::AniList, reg)
            .unwrap()
            .finish_pin("token".into())
            .is_err()
    );
}
