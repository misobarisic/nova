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
fn pin_login_uses_minimal_url_and_requires_pin_registration() {
    let reg = ClientRegistration {
        client_id: "123".into(),
        redirect_uri: "https://anilist.co/api/v2/oauth/pin".into(),
    };
    let auth = Authorization::begin(Service::AniList, reg).unwrap();
    assert_eq!(
        auth.url().unwrap(),
        "https://anilist.co/api/v2/oauth/authorize?client_id=123&response_type=token"
    );
    let transport = Fixture::new(vec![]);
    let tokens = auth
        .finish_return(AuthReturn::new(" token ".into()).unwrap(), &&transport, 100)
        .unwrap();
    assert_eq!(tokens.access.expose(), "token");
    assert!(transport.requests.lock().unwrap().is_empty());
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

#[test]
fn cross_reference_uses_public_metadata_and_never_an_unrelated_list() {
    let f = Fixture::new(vec![
        json!({"data":{"Page":{"media":[{"id":12,"idMal":42,"type":"ANIME","title":{"english":"Anime"},"format":"TV","episodes":12,"status":"FINISHED","seasonYear":2026}]}}}),
    ]);
    let c = Client::public_anilist(&f);
    let media = c.media_by_mal(42.try_into().unwrap(), 0).unwrap().unwrap();
    assert_eq!(media.id.get(), 12);
    assert_eq!(c.viewer(), Err(ApiError::Authentication));
    assert!(f.requests.lock().unwrap()[0].bearer.is_none());
}
#[test]
fn omitted_anilist_entry_field_is_not_proof_of_absence() {
    let f = Fixture::new(vec![viewer(), json!({"data":{"Media":{"id":12}}})]);
    let mut c = client(&f);
    c.verify(0).unwrap();
    assert_eq!(
        c.read(12.try_into().unwrap(), 0),
        Err(ApiError::InvalidResponse)
    );
}
#[test]
fn anilist_date_clear_omits_progress_and_other_fields() {
    let mut cleared = entry(4);
    cleared["startedAt"] = json!({"year":null,"month":null,"day":null});
    let f = Fixture::new(vec![
        viewer(),
        json!({"data":{"SaveMediaListEntry":cleared}}),
    ]);
    let mut c = client(&f);
    c.verify(0).unwrap();
    c.update(
        12.try_into().unwrap(),
        None,
        &EntryPatch {
            started: Some(ListDate::default()),
            ..Default::default()
        },
        0,
    )
    .unwrap();
    let req = f.requests.lock().unwrap();
    let Body::Json(body) = &req[1].body else {
        panic!()
    };
    let query = body["query"].as_str().unwrap();
    assert!(query.contains("startedAt:$startedAt"));
    assert!(!query.contains("progress:$progress"));
    assert!(!query.contains("completedAt:$completedAt"));
}
#[test]
fn anilist_callback_requires_registered_route_and_current_state() {
    let reg = ClientRegistration {
        client_id: "123".into(),
        redirect_uri: "http://127.0.0.1:53926/callback".into(),
    };
    let auth = Authorization::begin(Service::AniList, reg).unwrap();
    let url = url::Url::parse(&auth.url().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let f = Fixture::new(vec![]);
    let token = auth
        .finish(
            &format!(
                "http://127.0.0.1:53926/callback#access_token=token&state={state}&expires_in=60"
            ),
            &&f,
            100,
        )
        .unwrap();
    assert_eq!(token.expires_at, Some(160));
    assert!(f.requests.lock().unwrap().is_empty());
}

#[test]
fn mal_refresh_rotates_both_credentials_and_uses_the_returned_expiry() {
    let f = Fixture::new(vec![
        json!({"access_token":"new-access","refresh_token":"new-refresh","token_type":"Bearer","expires_in":123}),
    ]);
    let old = Tokens {
        access: Secret::new("old-access".into()).unwrap(),
        refresh: Some(Secret::new("old-refresh".into()).unwrap()),
        expires_at: Some(1),
    };
    let registration = ClientRegistration {
        client_id: "public-id".into(),
        redirect_uri: "http://127.0.0.1:53926/callback".into(),
    };
    let tokens = old.refresh(&registration, &&f, 100).unwrap();
    assert_eq!(tokens.expires_at, Some(223));
    assert_eq!(tokens.access.expose(), "new-access");
    assert_eq!(tokens.refresh.unwrap().expose(), "new-refresh");
    let requests = f.requests.lock().unwrap();
    let Body::Form(form) = &requests[0].body else {
        panic!("refresh form")
    };
    assert!(form.contains(&("refresh_token".into(), "old-refresh".into())));
    assert!(!form.iter().any(|(key, _)| key == "client_secret"));
}

#[test]
fn official_mal_release_details_keep_tv_movie_alternatives_and_partial_dates() {
    let f = Fixture::new(vec![
        json!({"id":49926,"title":"Kimetsu no Yaiba: Mugen Ressha-hen","media_type":"tv","num_episodes":7,"status":"finished_airing","start_date":"2021-10-10","end_date":"2021-11-28","alternative_titles":{"en":"Demon Slayer: Mugen Train Arc","synonyms":["Mugen Train"]},"related_anime":[{"node":{"id":40456},"relation_type":"alternative_version"},{"node":{"id":47778},"relation_type":"sequel"}]}),
    ]);
    let c = Client::new(
        Service::MyAnimeList,
        Secret::new("test-token".into()).unwrap(),
        &f,
    );
    let d = c.release_details(49926.try_into().unwrap(), 0).unwrap();
    assert_eq!(d.media.episodes.unwrap().get(), 7);
    assert_eq!(d.aliases[0], "Demon Slayer: Mugen Train Arc");
    assert_eq!(d.relations[0].relation, ReleaseRelation::Alternative);
    assert_eq!(d.relations[1].relation, ReleaseRelation::Sequel);
    let requests = f.requests.lock().unwrap();
    assert!(requests[0].url.contains("related_anime"));
}
#[test]
fn anilist_release_details_exclude_manga_relations_and_preserve_aliases() {
    let f = Fixture::new(vec![
        json!({"data":{"Media":{"id":12,"idMal":49926,"type":"ANIME","title":{"english":"Mugen Train","romaji":"Mugen Ressha-hen"},"format":"TV","episodes":7,"status":"FINISHED","seasonYear":2021,"startDate":{"year":2021,"month":10,"day":null},"endDate":null,"relations":{"edges":[{"relationType":"SEQUEL","node":{"id":13,"type":"ANIME"}},{"relationType":"ADAPTATION","node":{"id":14,"type":"MANGA"}}]}}}}),
    ]);
    let d = client(&f)
        .release_details(12.try_into().unwrap(), 0)
        .unwrap();
    assert_eq!(d.relations.len(), 1);
    assert_eq!(d.relations[0].id.get(), 13);
    assert_eq!(d.start.month, Some(10));
    assert!(d.aliases.contains(&"Mugen Ressha-hen".to_string()));
}

#[test]
fn release_dates_may_be_partial_and_wrong_release_ids_are_rejected() {
    let f = Fixture::new(vec![
        json!({"id":12,"title":"Partial dates","media_type":"tv","num_episodes":12,"status":"currently_airing","start_date":"2026-10","end_date":null}),
        json!({"id":13,"title":"Wrong id","media_type":"tv","num_episodes":12,"status":"currently_airing"}),
    ]);
    let c = Client::new(
        Service::MyAnimeList,
        Secret::new("test-token".into()).unwrap(),
        &f,
    );
    let details = c.release_details(12.try_into().unwrap(), 0).unwrap();
    assert_eq!(
        details.start,
        ListDate {
            year: Some(2026),
            month: Some(10),
            day: None
        }
    );
    assert_eq!(
        c.release_details(12.try_into().unwrap(), 0),
        Err(ApiError::InvalidResponse)
    );
}

#[test]
fn upcoming_mushoku_and_apothecary_releases_accept_year_and_month_dates() {
    let f = Fixture::new(vec![
        json!({"id":65077,"title":"Mushoku Tensei III Part 2","media_type":"tv","num_episodes":0,"status":"not_yet_aired","start_date":"2027","end_date":null}),
        json!({"id":62841,"title":"Kusuriya no Hitorigoto 3rd Season Part 2","media_type":"tv","num_episodes":0,"status":"not_yet_aired","start_date":"2027-04","end_date":null}),
    ]);
    let c = Client::new(
        Service::MyAnimeList,
        Secret::new("test-token".into()).unwrap(),
        &f,
    );
    let mushoku = c.release_details(65077.try_into().unwrap(), 0).unwrap();
    assert_eq!(
        mushoku.start,
        ListDate {
            year: Some(2027),
            month: None,
            day: None
        }
    );
    assert_eq!(mushoku.media.episodes, None);
    let kusuriya = c.release_details(62841.try_into().unwrap(), 0).unwrap();
    assert_eq!(
        kusuriya.start,
        ListDate {
            year: Some(2027),
            month: Some(4),
            day: None
        }
    );
    assert_eq!(kusuriya.media.episodes, None);
}
