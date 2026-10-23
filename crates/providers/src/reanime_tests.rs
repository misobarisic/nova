//! Source-shaped Reanime fixtures run through QuickJS and the real Rust mapper.
use crate::{
    HttpResponse, MemoryProviderHost, PluginManifest, PluginRuntime, ProviderHost,
    ProviderHostError,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

struct Host {
    state: Arc<MemoryProviderHost>,
    rows: Vec<Value>,
    details: BTreeMap<String, (Value, Value)>,
    calls: Mutex<Vec<String>>,
}
impl ProviderHost for Host {
    fn get(
        &self,
        raw: &str,
        _: &BTreeMap<String, String>,
    ) -> Result<HttpResponse, ProviderHostError> {
        let url = url::Url::parse(raw).unwrap();
        self.calls.lock().unwrap().push(url.path().into());
        let parts: Vec<_> = url.path_segments().unwrap().collect();
        let body = if url.path() == "/api/v1/search" {
            json!({"results":self.rows})
        } else if let Some(slug) = parts.get(3).filter(|_| parts.get(2) == Some(&"anime")) {
            let (details, episodes) = self
                .details
                .get(*slug)
                .ok_or_else(|| ProviderHostError("unknown fixture".into()))?;
            if parts.get(4) == Some(&"episodes") {
                episodes.clone()
            } else {
                details.clone()
            }
        } else if url.path().starts_with("/api/flix/") {
            json!({"success":true,"servers":[{"serverName":"HD-1","dataType":"sub","dataLink":"https://flixcloud.cc/e/abc?v=1"}]})
        } else {
            return Err(ProviderHostError("unexpected fixture request".into()));
        };
        Ok(HttpResponse {
            status: 200,
            final_url: raw.into(),
            body: body.to_string().into_bytes(),
            content_type: None,
        })
    }
    fn storage_get(&self, key: &str) -> Option<String> {
        self.state.storage_get(key)
    }
    fn storage_set(&self, key: &str, value: &str) -> Result<(), ProviderHostError> {
        self.state.storage_set(key, value)
    }
    fn log(&self, _: &str) {}
}
fn row(slug: &str, title: &str, year: u32, count: u32, available: u32) -> (Value, (Value, Value)) {
    let dto = json!({"anime_id":slug,"title":{"english":title},"format":"TV","season_year":year,"episodes_total":count,"anilist_id":123,"imdb_id":if slug=="steins-gate" {"tt1910272"} else {"tt13293588"}});
    let episodes = json!({"data":(1..=available).map(|n|json!({"episode_number":n,"playable":true,"title":format!("Episode {n}"),"aired":format!("{year}-01-01")})).collect::<Vec<_>>()});
    (dto.clone(), (dto, episodes))
}
fn invoke(rows: Vec<(Value, (Value, Value))>, lookup: Value) -> (Value, Arc<Host>) {
    let host = Arc::new(Host {
        state: MemoryProviderHost::new(),
        rows: rows.iter().map(|(r, _)| r.clone()).collect(),
        details: rows
            .into_iter()
            .map(|(r, d)| (r["anime_id"].as_str().unwrap().to_owned(), d))
            .collect(),
        calls: Mutex::new(vec![]),
    });
    let manifest: PluginManifest = serde_json::from_str(include_str!(
        "../test-fixtures/addons/reanime/manifest.json"
    ))
    .unwrap();
    let result = PluginRuntime::new(
        manifest,
        include_str!("../test-fixtures/addons/reanime/index.js"),
    )
    .invoke(host.clone(), &json!({"op":"lookupStreams","lookup":lookup}))
    .unwrap();
    (result, host)
}
#[test]
fn steins_gate_regular_inventory_ignores_appended_ova_without_changing_native_ids() {
    let (result, _) = invoke(
        vec![row("steins-gate", "Steins;Gate", 2011, 24, 25)],
        json!({"mediaId":"tt1910272","mediaType":"series","title":"Steins;Gate","year":"2011","season":1,"episode":1,"absoluteEpisode":1,"seasonEpisodeCount":24,"seriesEpisodeCount":24}),
    );
    assert_eq!(result.as_array().unwrap().len(), 1);
}
#[test]
fn season_three_is_ranked_before_spinoffs_and_unrelated_popular_results() {
    let mut rows = vec![];
    for i in 0..8 {
        let mut r = row(
            &format!("special-{i}"),
            "Mushoku Tensei: Jobless Reincarnation",
            2021,
            1,
            1,
        );
        r.0["format"] = json!("SPECIAL");
        rows.push(r);
    }
    rows.push(row(
        "season-three",
        "Mushoku Tensei: Jobless Reincarnation Season 3",
        2026,
        14,
        14,
    ));
    let (result, host) = invoke(
        rows,
        json!({"mediaId":"tt13293588","mediaType":"series","title":"Mushoku Tensei: Jobless Reincarnation","year":"2021","season":3,"episode":2,"absoluteEpisode":49,"seasonEpisodeCount":14,"seriesEpisodeCount":61,"released":"2026-07-03","episodeTitle":"Howl, Mad Dog"}),
    );
    assert_eq!(result.as_array().unwrap().len(), 1);
    assert!(
        host.calls
            .lock()
            .unwrap()
            .iter()
            .any(|p| p == "/api/v1/anime/season-three")
    );
    assert!(
        !host
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("special-"))
    );
}
#[test]
fn shortened_title_uses_confirmed_show_identity_but_rejects_a_remake() {
    for (imdb, expected) in [("tt13293588", 1), ("tt9999999", 0)] {
        let (result, _) = invoke(
            vec![row(
                "season-three",
                "Mushoku Tensei: Jobless Reincarnation Season 3",
                2026,
                14,
                14,
            )],
            json!({"mediaId":imdb,"mediaType":"series","title":"Mushoku Tensei","year":"2021","season":3,"episode":1,"seasonEpisodeCount":14,"released":"2026-07-03"}),
        );
        assert_eq!(result.as_array().unwrap().len(), expected);
    }
}
