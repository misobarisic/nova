//! Profile-owned TMDB recommendations with an installed-source fallback.
use super::*;
use std::time::Instant;
mod tmdb;

pub(super) fn settings_authority(settings: &CacheSettings) -> u64 {
    let config = tmdb::Settings::from_settings(settings);
    fnv1a(&serde_json::to_vec(&config.authority()).unwrap_or_default())
}

pub(super) fn supported_identity(type_: &str, id: &str) -> bool {
    nova_providers::ExternalId::parse(id).is_some()
        || (matches!(type_, "movie" | "series")
            && id.strip_prefix("tmdb:").is_some_and(|id| {
                !id.is_empty()
                    && id.bytes().all(|b| b.is_ascii_digit())
                    && id.parse::<u32>().is_ok_and(|id| id > 0)
            }))
}

const LIMIT: usize = 24;
const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(15 * 60);
const STALE_TTL_SECS: u64 = 7 * 24 * 60 * 60;

#[derive(Serialize, Deserialize)]
struct SavedTitles {
    authority: u64,
    fetched_at: u64,
    rows: Vec<MetaPreview>,
}

fn saved_key(type_: &str, id: &str) -> String {
    format!("similar_titles:v1:{type_}\u{1}{id}")
}

fn saved_authority(installed: &[Installed], tmdb: &tmdb::Settings) -> Option<u64> {
    // Runtime generations deliberately stay out of persistent identity: an
    // unchanged provider configuration can reuse this cache after a restart.
    let sources = installed
        .iter()
        .filter(|addon| addon.enabled && addon.available)
        .map(|addon| (&addon.url, &addon.manifest))
        .collect::<Vec<_>>();
    let settings = storage::try_get_str("plugin_settings").ok()?;
    let settings = settings
        .as_deref()
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()
        .ok()?
        .unwrap_or_else(|| serde_json::json!({}));
    let mut value = serde_json::to_value((sources, settings, tmdb.authority())).ok()?;
    // Flattened manifest maps have randomized iteration order. Canonicalize
    // them before hashing so a process restart does not invalidate the cache.
    value.sort_all_objects();
    serde_json::to_vec(&value).ok().map(|bytes| fnv1a(&bytes))
}

impl SavedTitles {
    fn usable(&self, authority: u64, now: u64) -> bool {
        self.authority == authority
            && self.fetched_at <= now
            && now - self.fetched_at < STALE_TTL_SECS
            && !self.rows.is_empty()
            && self.rows.len() <= LIMIT
    }
}

fn read_saved(
    installed: &[Installed],
    type_: &str,
    id: &str,
    tmdb: &tmdb::Settings,
) -> Option<SavedTitles> {
    let authority = saved_authority(installed, tmdb)?;
    read_json::<SavedTitles>(&saved_key(type_, id))
        .filter(|saved| saved.usable(authority, now_secs()))
}

#[derive(Default)]
pub(super) struct Cache {
    entries: HashMap<String, (Instant, Vec<MetaPreview>)>,
}

fn inventory_key(installed: &[Installed], tmdb: &tmdb::Settings) -> String {
    serde_json::to_string(&(
        &installed
            .iter()
            .filter(|addon| addon.enabled && addon.available)
            .map(|addon| (&addon.url, addon.generation, &addon.manifest))
            .collect::<Vec<_>>(),
        tmdb.authority(),
    ))
    .unwrap_or_default()
}

impl Bridge {
    /// Completion runs on a worker in the originating profile context. Callers
    /// publishing Slint models must use profiles::invoke_from_event_loop.
    pub(super) fn fetch_similar(
        &self,
        type_: &str,
        id: &str,
        then: impl FnOnce(Vec<MetaPreview>) + Send + 'static,
    ) {
        let type_ = type_.to_owned();
        let id = id.to_owned();
        let context = profiles::capture();
        let settings = context
            .session
            .as_ref()
            .and_then(|session| session.providers.get())
            .cloned()
            .unwrap_or_else(nova_providers::plugin_settings::capture_settings_capability);
        let settings_generation =
            settings.with(nova_providers::plugin_settings::settings_generation);
        let inventory = if profiles::enabled() {
            context
                .session
                .as_ref()
                .and_then(|session| session.provider_inventory().ok())
        } else {
            Some(nova_providers::OwnedProviderSnapshot::capture())
        };
        let (installed, cache, tmdb) = {
            let state = self.shared.lock().unwrap();
            (
                state.installed.clone(),
                state.similar_cache.clone(),
                tmdb::Settings::from_settings(&state.cache_settings),
            )
        };
        let authority = inventory_key(&installed, &tmdb);
        let key = serde_json::to_string(&(&authority, settings_generation, &type_, &id))
            .unwrap_or_default();
        let shared = self.shared.clone();
        profiles::thread::spawn(move || {
            profiles::with_context(context.clone(), || {
                let Some(inventory) = inventory else {
                    then(Vec::new());
                    return;
                };
                inventory.with(|| {
                    settings.with(|| {
                        let current = || {
                            profiles::is_current(&context)
                                && !context.session.as_ref().is_some_and(|session| {
                                    session.cancelled.load(Ordering::Acquire)
                                })
                                && settings.is_current()
                                && nova_providers::plugin_settings::settings_generation()
                                    == settings_generation
                                && {
                                    let state = shared.lock().unwrap();
                                    inventory_key(
                                        &state.installed,
                                        &tmdb::Settings::from_settings(&state.cache_settings),
                                    ) == authority
                                }
                        };
                        if !current() {
                            then(Vec::new());
                            return;
                        }
                        let cached = cache
                            .lock()
                            .unwrap()
                            .entries
                            .get(&key)
                            .filter(|(at, _)| at.elapsed() < TTL)
                            .map(|(_, rows)| rows.clone());
                        if let Some(rows) = cached {
                            then(rows);
                            return;
                        }
                        let save_authority = saved_authority(&installed, &tmdb);
                        let saved = read_saved(&installed, &type_, &id, &tmdb);
                        if let Some(saved) = &saved
                            && now_secs().saturating_sub(saved.fetched_at) < TTL.as_secs()
                        {
                            then(saved.rows.clone());
                            return;
                        }
                        let started = Instant::now();
                        let mut remaining_requests = if tmdb.configured() { 18 } else { 8 };
                        let budget = if tmdb.configured() { 20 } else { 12 };
                        let mut fetch = |url: &str| {
                            let remaining =
                                Duration::from_secs(budget).saturating_sub(started.elapsed());
                            if remaining_requests == 0 || remaining.is_zero() || !current() {
                                return None;
                            }
                            remaining_requests -= 1;
                            net::fetch_metadata_bytes(
                                url,
                                remaining.min(Duration::from_secs(4)),
                                RESPONSE_LIMIT,
                            )
                            .ok()
                        };
                        let mut rows = retrieve(&installed, &type_, &id, &tmdb, &mut fetch);
                        if !current() {
                            then(Vec::new());
                            return;
                        }
                        if rows.is_empty() {
                            if let Some(saved) = saved {
                                rows = saved.rows;
                            }
                        } else if let Some(authority) = save_authority {
                            write_json(
                                &saved_key(&type_, &id),
                                &SavedTitles {
                                    authority,
                                    fetched_at: now_secs(),
                                    rows: rows.clone(),
                                },
                            );
                        }
                        let mut cache = cache.lock().unwrap();
                        cache.entries.retain(|_, (at, _)| at.elapsed() < TTL);
                        if cache.entries.len() >= 64
                            && let Some(oldest) = cache
                                .entries
                                .iter()
                                .min_by_key(|(_, (at, _))| *at)
                                .map(|(key, _)| key.clone())
                        {
                            cache.entries.remove(&oldest);
                        }
                        if !rows.is_empty() {
                            cache.entries.insert(key, (Instant::now(), rows.clone()));
                        }
                        drop(cache);
                        then(rows);
                    });
                });
            });
        });
    }

    pub(super) fn load_detail_similar(&self) {
        if !self
            .app()
            .is_some_and(|app| app.get_detail_tab() == 0 && !app.get_stream_selector_open())
        {
            return;
        }
        let prefer_tmdb =
            tmdb::Settings::from_settings(&self.shared.lock().unwrap().cache_settings).configured();
        let Some((type_, id, token, original_rows)) = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .filter(|modal| {
                modal.details.related.is_empty()
                    || (prefer_tmdb
                        && !modal.details.related.iter().any(|row| {
                            row.extra
                                .get("novaRecommendationSource")
                                .and_then(serde_json::Value::as_str)
                                == Some("tmdb")
                        }))
            })
            .map(|m| {
                (
                    m.type_.clone(),
                    m.id.clone(),
                    m.open_token.clone(),
                    m.details.related.clone(),
                )
            })
        else {
            return;
        };
        let tmdb = tmdb::Settings::from_settings(&self.shared.lock().unwrap().cache_settings);
        let authority = inventory_key(&self.shared.lock().unwrap().installed, &tmdb);
        let cached_rows = {
            let installed = self.shared.lock().unwrap().installed.clone();
            read_saved(&installed, &type_, &id, &tmdb).map(|saved| saved.rows)
        };
        if let Some(rows) = &cached_rows {
            if let Some(modal) = self.shared.lock().unwrap().modal_item.as_mut()
                && Arc::ptr_eq(&modal.open_token, &token)
            {
                modal.details.related = rows.clone();
            }
            self.publish_detail_extras();
        }
        let settings = nova_providers::plugin_settings::capture_settings_capability();
        let generation = settings.with(nova_providers::plugin_settings::settings_generation);
        let bridge = self.clone();
        self.fetch_similar(&type_.clone(), &id.clone(), move |rows| {
            let _ = profiles::invoke_from_event_loop(move || {
                if !settings.is_current()
                    || settings.with(nova_providers::plugin_settings::settings_generation)
                        != generation
                {
                    return;
                }
                let changed = {
                    let mut state = bridge.shared.lock().unwrap();
                    if inventory_key(
                        &state.installed,
                        &tmdb::Settings::from_settings(&state.cache_settings),
                    ) != authority
                    {
                        return;
                    }
                    if let Some(modal) = state.modal_item.as_mut()
                        && modal.type_ == type_
                        && modal.id == id
                        && Arc::ptr_eq(&modal.open_token, &token)
                        && (modal.details.related.is_empty()
                            || modal.details.related == original_rows
                            || cached_rows.as_ref() == Some(&modal.details.related))
                        && !rows.is_empty()
                    {
                        modal.details.related = rows;
                        true
                    } else {
                        false
                    }
                };
                if changed {
                    bridge.publish_detail_extras();
                }
            });
        });
    }
}

fn retrieve(
    installed: &[Installed],
    type_: &str,
    id: &str,
    tmdb: &tmdb::Settings,
    fetch: &mut impl FnMut(&str) -> Option<Vec<u8>>,
) -> Vec<MetaPreview> {
    // Without the originating catalog URL an opaque ID cannot establish
    // identity across wildcard manifests. Explicit Detail extensions still
    // work through their original metadata response.
    if type_.is_empty()
        || id.is_empty()
        || id.len() > 512
        || id.chars().any(char::is_control)
        || !supported_identity(type_, id)
    {
        return Vec::new();
    }
    let enabled = installed.iter().filter(|a| a.enabled && a.available);
    let mut seed = None;
    let mut explicit_rows = Vec::new();
    for addon in enabled
        .clone()
        .filter(|a| a.manifest.accepts("meta", type_, id))
        .take(2)
    {
        let Ok(endpoint) = Addon::new(&addon.url) else {
            continue;
        };
        let Some(item) = fetch(&endpoint.meta_url(type_, id))
            .and_then(|bytes| Addon::parse_meta(&bytes).ok().flatten())
            .filter(|item| detail::meta_matches_request(item, type_, id))
        else {
            continue;
        };
        let mut explicit = detail_extras::DetailExtras::from_item(&item).related;
        if !explicit.is_empty() {
            for preview in &mut explicit {
                // Opaque provider IDs must keep the installed source that
                // actually supplied them, including related-title extensions.
                preview
                    .extra
                    .insert("novaSourceUrl".into(), addon.url.clone().into());
            }
            if !tmdb.configured() {
                return explicit;
            }
            if explicit_rows.is_empty() {
                explicit_rows = explicit;
            }
        }
        if seed.is_none()
            || seed
                .as_ref()
                .is_some_and(|old: &MetaItem| old.preview.genres.is_empty())
        {
            seed = Some(item);
        }
    }
    let mut tmdb_seed = seed
        .as_ref()
        .map(|item| item.preview.clone())
        .unwrap_or_else(|| MetaPreview {
            id: id.to_owned(),
            type_: type_.to_owned(),
            ..Default::default()
        });
    if let Some(seed) = &seed {
        tmdb_seed.extra.extend(seed.extra.clone());
    }
    let recommendations = tmdb::recommendations(&tmdb_seed, installed, tmdb, fetch);
    if !recommendations.is_empty() {
        return recommendations;
    }
    if !explicit_rows.is_empty() {
        return explicit_rows;
    }
    let Some(mut seed) = seed else {
        return Vec::new();
    };
    seed.preview.extra.extend(seed.extra.clone());
    let animated = animated(&seed.preview);
    let anime_seed = anime(&seed.preview);
    let mut urls = Vec::new();
    for addon in enabled {
        let Ok(endpoint) = Addon::new(&addon.url) else {
            continue;
        };
        for catalog in catalog::browsable_catalogs(&addon.manifest).filter(|c| c.type_ == type_) {
            if catalog
                .extra
                .iter()
                .any(|extra| extra.is_required && extra.name != "genre")
            {
                continue;
            }
            let Some(genre_extra) = catalog.extra.iter().find(|e| e.name == "genre") else {
                continue;
            };
            // Medium is a constraint, not just another shared genre. An
            // animation seed must not spend its budget on a Drama catalog.
            let mut genres = seed
                .preview
                .genres
                .iter()
                .take(12)
                .map(String::as_str)
                .collect::<Vec<_>>();
            if anime_seed {
                genres.insert(0, "Anime");
            }
            if animated {
                genres.push("Animation");
            }
            genres.sort_by_key(|genre| match genre.trim().to_lowercase().as_str() {
                "anime" => 0,
                "animation" => 1,
                _ => 2,
            });
            for genre in genres {
                if animated
                    && !matches!(genre.trim().to_lowercase().as_str(), "animation" | "anime")
                {
                    continue;
                }
                let genre = if genre_extra.options.is_empty() && !genre_extra.options_required {
                    Some(genre)
                } else {
                    genre_extra
                        .options
                        .iter()
                        .find(|g| g.eq_ignore_ascii_case(genre))
                        .map(String::as_str)
                };
                if let Some(genre) = genre {
                    let url = endpoint.catalog_url(type_, &catalog.id, &[("genre", genre)]);
                    let priority = if anime_seed && genre.eq_ignore_ascii_case("anime") {
                        0
                    } else {
                        1
                    };
                    if urls.len() < 128 && !urls.iter().any(|(existing, _, _)| existing == &url) {
                        urls.push((url, addon.url.clone(), priority));
                    }
                }
            }
        }
    }
    urls.sort_by_key(|(_, _, priority)| *priority);
    let candidates = urls
        .iter()
        .take(2)
        .filter_map(|(url, owner, _)| {
            let bytes = fetch(url)?;
            let rows = Addon::parse_catalog(&bytes).ok()?;
            Some(
                rows.into_iter()
                    .take(100)
                    .map(|mut preview| {
                        preview
                            .extra
                            .insert("novaSourceUrl".into(), owner.clone().into());
                        preview
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect();
    // Catalog previews often omit country. Verify a small, already-ranked
    // shortlist through its actual installed owner rather than treating all
    // animation (or missing metadata) as evidence of anime.
    let mut candidates = rank_candidates(&seed, candidates, false);
    if anime_seed {
        let mut lookups = 0;
        for candidate in &mut candidates {
            if anime(candidate) || lookups >= 4 {
                continue;
            }
            let Some(owner) = candidate
                .extra
                .get("novaSourceUrl")
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let Some(addon) = installed.iter().find(|addon| {
                addon.enabled
                    && addon.available
                    && addon.url == owner
                    && addon
                        .manifest
                        .accepts("meta", &candidate.type_, &candidate.id)
            }) else {
                continue;
            };
            let Ok(endpoint) = Addon::new(&addon.url) else {
                continue;
            };
            lookups += 1;
            if let Some(mut item) = fetch(&endpoint.meta_url(&candidate.type_, &candidate.id))
                .and_then(|bytes| Addon::parse_meta(&bytes).ok().flatten())
                .filter(|item| detail::meta_matches_request(item, &candidate.type_, &candidate.id))
            {
                item.preview.extra.extend(item.extra);
                candidate.extra.extend(item.preview.extra);
                candidate
                    .extra
                    .insert("novaSourceUrl".into(), addon.url.clone().into());
                if !item.preview.genres.is_empty() {
                    candidate.genres = item.preview.genres;
                }
                if item.preview.description.is_some() {
                    candidate.description = item.preview.description;
                }
                for (key, names) in [
                    ("cast", item.cast),
                    ("director", item.director),
                    ("writer", item.writer),
                ] {
                    if !names.is_empty() {
                        candidate.extra.insert(key.into(), serde_json::json!(names));
                    }
                }
            }
        }
    }
    rank(&seed, candidates)
}

fn anime_identity(preview: &MetaPreview) -> bool {
    let ids = nova_providers::normalize_external_ids(&preview.extra, &preview.id, &preview.type_);
    [
        nova_providers::IdNamespace::MalAnime,
        nova_providers::IdNamespace::AnilistAnime,
    ]
    .into_iter()
    .any(|namespace| {
        matches!(
            ids.resolve_id(namespace),
            nova_providers::IdResolution::Unique(_)
        )
    })
}

fn anime(preview: &MetaPreview) -> bool {
    if preview.type_ == "anime"
        || preview
            .genres
            .iter()
            .any(|genre| genre.trim().eq_ignore_ascii_case("anime"))
        || anime_identity(preview)
    {
        return true;
    }
    // Country is supplied by metadata, never inferred from a title or cast.
    animated(preview)
        && ["country", "countries"].into_iter().any(|key| {
            preview.extra.get(key).is_some_and(|value| {
                let japanese = |value: &str| {
                    value.split([',', '/', ';']).any(|part| {
                        matches!(part.trim().to_lowercase().as_str(), "japan" | "jp" | "jpn")
                    })
                };
                value.as_str().is_some_and(japanese)
                    || value.as_array().is_some_and(|values| {
                        values
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .any(japanese)
                    })
            })
        })
}

fn animated(preview: &MetaPreview) -> bool {
    preview.type_ == "anime"
        || preview
            .genres
            .iter()
            .any(|genre| matches!(genre.trim().to_lowercase().as_str(), "animation" | "anime"))
        || anime_identity(preview)
}

fn story_words(description: Option<&str>) -> std::collections::BTreeSet<String> {
    // Boilerplate and generic synopsis vocabulary must not create a match.
    const COMMON: &str = concat!(
        "about after again against also another around available back become becomes before being between both ",
        "city could description during each family find finds first friends from group have having home into just ",
        "life lives more most must only other over people provided school series show some story student students ",
        "synopsis take than that their them then there these they this through title together unknown when where ",
        "which while will with world would years young"
    );
    description
        .unwrap_or_default()
        .chars()
        .take(4096)
        .collect::<String>()
        .split(|c: char| !c.is_alphabetic())
        .map(str::to_lowercase)
        .filter(|word| {
            word.chars().count() >= 4
                && !COMMON.split_ascii_whitespace().any(|common| common == word)
        })
        .collect()
}

fn labels(values: &[String]) -> std::collections::BTreeSet<String> {
    values
        .iter()
        .take(100)
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && !matches!(s.as_str(), "unknown" | "n/a" | "none" | "various"))
        .collect()
}

fn genre_labels(values: &[String]) -> std::collections::BTreeSet<String> {
    labels(values)
        .into_iter()
        .map(|genre| match genre.as_str() {
            "science fiction" | "science-fiction" | "sci fi" => "sci-fi".into(),
            _ => genre,
        })
        .collect()
}

fn rank(seed: &MetaItem, candidates: Vec<MetaPreview>) -> Vec<MetaPreview> {
    rank_candidates(seed, candidates, true)
}

fn story_phrases(description: Option<&str>) -> std::collections::BTreeSet<String> {
    let useful = story_words(description);
    let words = description
        .unwrap_or_default()
        .chars()
        .take(4096)
        .collect::<String>()
        .split(|c: char| !c.is_alphabetic())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    words
        .windows(2)
        .filter(|pair| useful.contains(&pair[0]) && useful.contains(&pair[1]))
        .map(|pair| format!("{} {}", pair[0], pair[1]))
        .collect()
}

fn rank_candidates(
    seed: &MetaItem,
    candidates: Vec<MetaPreview>,
    verify_anime: bool,
) -> Vec<MetaPreview> {
    let genres = genre_labels(&seed.preview.genres);
    let specific =
        |genre: &&String| !matches!(genre.as_str(), "animation" | "anime" | "comedy" | "drama");
    let seed_words = story_words(seed.preview.description.as_deref());
    let seed_phrases = story_phrases(seed.preview.description.as_deref());
    let creators = labels(
        &seed
            .director
            .iter()
            .chain(&seed.writer)
            .cloned()
            .collect::<Vec<_>>(),
    );
    let people = labels(&seed.cast);
    let mut seen = std::collections::HashSet::new();
    let mut scored = candidates
        .into_iter()
        .filter_map(|candidate| {
            if candidate.type_ != seed.preview.type_
                || (animated(&seed.preview) != animated(&candidate))
                || (verify_anime && anime(&seed.preview) && !anime(&candidate))
                || candidate.id == seed.preview.id
                || candidate.id.is_empty()
                || candidate.id.len() > 512
                || candidate.id.chars().any(char::is_control)
                || candidate.title().trim().is_empty()
            {
                return None;
            }
            let candidate_genres = genre_labels(&candidate.genres);
            let shared_genres = genres.intersection(&candidate_genres).count();
            let shared_specific = genres
                .intersection(&candidate_genres)
                .filter(specific)
                .count();
            let names = |keys: &[&str]| {
                keys.iter()
                    .filter_map(|key| {
                        candidate
                            .extra
                            .get(*key)
                            .and_then(serde_json::Value::as_array)
                    })
                    .flatten()
                    .take(100)
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            };
            let shared_creators = creators
                .intersection(&labels(&names(&["director", "writer"])))
                .count();
            let shared_people = people.intersection(&labels(&names(&["cast"]))).count();
            let words = story_words(candidate.description.as_deref());
            let shared_words = seed_words.intersection(&words).count();
            let story_match = shared_words >= 3
                && seed_phrases
                    .intersection(&story_phrases(candidate.description.as_deref()))
                    .next()
                    .is_some()
                && shared_words * 5 >= seed_words.len().min(words.len()).max(1);
            // Genres help rank a match, but cannot establish a relationship
            // on their own. Require plot, creator or repeated cast evidence.
            (shared_genres > 0 && (shared_creators > 0 || shared_people >= 2 || story_match))
                .then_some((
                    shared_specific * 3
                        + shared_creators * 6
                        + shared_people * 2
                        + shared_words.min(6),
                    candidate,
                ))
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|row| std::cmp::Reverse(row.0));
    scored
        .into_iter()
        .map(|(_, row)| row)
        .filter(|row| seen.insert((row.type_.clone(), row.id.clone())))
        .take(LIMIT)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anime_requires_origin_and_real_similarity_not_broad_genres() {
        let seed: MetaItem = serde_json::from_value(json!({
            "id":"tt1910272", "type":"series", "name":"Steins;Gate",
            "genres":["Animation","Comedy","Drama"], "country":"Japan",
            "description":"A university student discovers time travel and battles an evil organization.",
            "cast":["Actor"]
        })).unwrap();
        let rows = json!([
            {"id":"broad", "type":"series", "name":"Generic anime", "genres":["Animation","Comedy","Drama"], "country":"Japan"},
            {"id":"live", "type":"series", "name":"Live action", "genres":["Comedy","Drama"], "description":"A university student discovers time travel."},
            {"id":"western", "type":"series", "name":"Western animation", "genres":["Animation","Drama"], "country":"US", "description":"A university student discovers time travel."},
            {"id":"unknown", "type":"series", "name":"Unknown origin", "genres":["Animation","Drama"], "description":"A university student discovers time travel."},
            {"id":"cast", "type":"series", "name":"Same actor", "genres":["Animation","Drama"], "country":"Japan", "cast":["Actor"]},
            {"id":"topic", "type":"series", "name":"Related anime", "genres":["Animation","Drama"], "country":["JP"], "description":"Time travel puts a university student against an evil organization."},
            {"id":"mal:123", "type":"series", "name":"Explicit anime identity", "genres":["Drama"], "description":"A university student discovers time travel."}
        ]);
        let rows = rank(&seed, serde_json::from_value(rows).unwrap());
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["topic", "mal:123"]
        );
    }

    #[test]
    fn generic_synopses_and_one_specific_genre_are_insufficient() {
        let seed: MetaItem = serde_json::from_value(json!({"id":"seed", "type":"series", "name":"Seed", "genres":["Drama","Comedy","Crime","Mystery"], "description":"A young group of friends must find their place in the world."})).unwrap();
        let candidates = json!([
            {"id":"broad", "type":"series", "name":"Broad", "genres":["Drama","Comedy"]},
            {"id":"one-specific", "type":"series", "name":"Still broad", "genres":["Drama","Crime"]},
            {"id":"two-specific", "type":"series", "name":"Still only genres", "genres":["Mystery","Crime"]},
            {"id":"generic", "type":"series", "name":"Generic", "genres":["Drama"], "description":"A young group of friends must find their place in the world."}
        ]);
        assert!(rank(&seed, serde_json::from_value(candidates).unwrap()).is_empty());
    }

    #[test]
    fn creator_matches_require_a_shared_genre_and_ignore_placeholder_names() {
        let seed: MetaItem = serde_json::from_value(json!({"id":"seed", "type":"series", "name":"Seed", "genres":["Science Fiction","Drama"], "writer":["Author"], "director":["Unknown"]})).unwrap();
        let candidates = json!([
            {"id":"creator", "type":"series", "name":"Creator match", "genres":["Sci-Fi"], "writer":["author"]},
            {"id":"placeholder", "type":"series", "name":"Placeholder", "genres":["Drama"], "director":["unknown"]},
            {"id":"wrong-genre", "type":"series", "name":"Different genre", "genres":["Comedy"], "writer":["Author"]}
        ]);
        let rows = rank(&seed, serde_json::from_value(candidates).unwrap());
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["creator"]
        );
    }

    #[test]
    fn anime_identity_extensions_work_without_animation_genre_or_country() {
        let preview: MetaPreview = serde_json::from_value(json!({"id":"tt1910272", "type":"series", "name":"Steins;Gate", "genres":["Drama"], "_malId":9253})).unwrap();
        assert!(anime(&preview));
        assert!(animated(&preview));
        let manga: MetaPreview = serde_json::from_value(json!({"id":"private", "type":"series", "name":"Manga", "ids":{"typed":[{"namespace":"mal_manga","value":9253}]}})).unwrap();
        assert!(!anime(&manga));
    }

    #[test]
    fn anime_catalogs_are_prioritized_and_missing_origin_is_verified_by_owner() {
        let addon = Installed {
            url: "https://installed.example".into(), label: "Installed".into(), enabled: true,
            configure_ok: None, available: true, generation: 1,
            manifest: serde_json::from_value(json!({"id":"test", "version":"1.0.0", "name":"Test", "resources":["meta","catalog"], "types":["series"], "catalogs":[{"id":"top", "type":"series", "name":"Top", "extra":[{"name":"genre","options":["Drama","Comedy","Animation","Anime"]}]}]})).unwrap(),
        };
        let mut requests = Vec::new();
        let rows = retrieve(
            &[addon],
            "series",
            "tt100",
            &tmdb::Settings::default(),
            &mut |url| {
                requests.push(url.to_owned());
                let value = if url.contains("/meta/series/tt100.") {
                    json!({"meta":{"id":"tt100", "type":"series", "name":"Seed", "genres":["Drama","Comedy","Animation"], "country":"Japan", "description":"A university student discovers time travel."}})
                } else if url.contains("/meta/series/tt200.") {
                    json!({"meta":{"id":"tt200", "type":"series", "name":"Candidate", "genres":["Animation","Drama"], "country":"Japan", "novaSourceUrl":"https://forged.example"}})
                } else {
                    assert!(
                        url.contains("genre=Anime.json") || url.contains("genre=Animation.json")
                    );
                    json!({"metas":[{"id":"tt200", "type":"series", "name":"Candidate", "genres":["Animation","Drama"], "description":"A university student discovers time travel."}, {"id":"tt300", "type":"series", "name":"Live action", "genres":["Comedy","Drama"], "description":"A university student discovers time travel."}]})
                };
                Some(serde_json::to_vec(&value).unwrap())
            },
        );
        assert_eq!(requests.len(), 4);
        assert!(requests[1].contains("genre=Anime.json"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "tt200");
        assert_eq!(rows[0].extra["novaSourceUrl"], "https://installed.example");
    }

    #[test]
    fn saved_titles_preserve_rows_and_reject_changed_expired_or_future_authority() {
        let saved = SavedTitles {
            authority: 42,
            fetched_at: 100,
            rows: vec![MetaPreview {
                id: "tt1".into(),
                type_: "series".into(),
                name: "Example".into(),
                ..Default::default()
            }],
        };
        let loaded: SavedTitles =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert!(loaded.usable(42, 101));
        assert_eq!(loaded.rows, saved.rows);
        assert!(!loaded.usable(43, 101));
        assert!(!loaded.usable(42, 99));
        assert!(!loaded.usable(42, 100 + STALE_TTL_SECS));
        assert_ne!(saved_key("series", "tt1"), saved_key("movie", "tt1"));
    }

    #[test]
    fn saved_titles_roundtrip_through_local_metadata_storage() {
        const CHILD: &str = "NOVA_SIMILAR_CACHE_TEST_ROOT";
        let Some(root) = std::env::var_os(CHILD) else {
            let root = std::env::temp_dir().join(format!(
                "nova-similar-cache-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, &root)
                .args([
                    "--exact",
                    "app::similar::tests::saved_titles_roundtrip_through_local_metadata_storage",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        storage::init_at(&PathBuf::from(root));
        let addon = Installed {
            url: "https://installed.example".into(), label: "Installed".into(), enabled: true,
            configure_ok: None, available: true, generation: 1,
            manifest: serde_json::from_value(json!({"id":"test", "version":"1.0.0", "name":"Test", "resources":["meta"], "types":["series"], "catalogs":[], "z":1, "a":2})).unwrap(),
        };
        let mut reloaded = addon.clone();
        reloaded.generation = 99;
        reloaded.manifest =
            serde_json::from_value(serde_json::to_value(&addon.manifest).unwrap()).unwrap();
        assert_eq!(
            saved_authority(&[addon], &tmdb::Settings::default()),
            saved_authority(&[reloaded], &tmdb::Settings::default())
        );
        let saved = SavedTitles {
            authority: saved_authority(&[], &tmdb::Settings::default()).unwrap(),
            fetched_at: now_secs(),
            rows: vec![MetaPreview {
                id: "tt1".into(),
                type_: "series".into(),
                name: "Example".into(),
                ..Default::default()
            }],
        };
        write_json(&saved_key("series", "tt100"), &saved);
        assert_eq!(
            read_saved(&[], "series", "tt100", &tmdb::Settings::default())
                .unwrap()
                .rows,
            saved.rows
        );
        assert!(read_saved(&[], "movie", "tt100", &tmdb::Settings::default()).is_none());
        storage::try_set_str("plugin_settings", r#"{"source":{"filter":"different"}}"#).unwrap();
        assert!(read_saved(&[], "series", "tt100", &tmdb::Settings::default()).is_none());
    }

    #[test]
    fn ranks_only_metadata_evidence_and_excludes_self_duplicates_and_other_types() {
        let seed: MetaItem = serde_json::from_value(json!({"id":"seed","type":"series","name":"Example", "genres":["Drama","Crime","Mystery"], "cast":["Actor","Another actor"], "description":"An investigator uncovers a criminal conspiracy behind mysterious disappearances."})).unwrap();
        let rows = json!([
            {"id":"word","type":"series","name":"Example sequel","genres":["Drama"]},
            {"id":"good","type":"series","name":"Different","genres":["crime","Mystery"], "description":"An investigator investigates mysterious disappearances and a criminal conspiracy."},
            {"id":"good","type":"series","name":"Duplicate","genres":["Crime","Drama"]},
            {"id":"person","type":"series","name":"Cast match","genres":["Drama"],"cast":["actor","another actor"]},
            {"id":"seed","type":"series","name":"Self","genres":["Crime","Drama"]},
            {"id":"movie","type":"movie","name":"Wrong type","genres":["Crime","Drama"]}
        ]);
        let ranked = rank(&seed, serde_json::from_value(rows).unwrap());
        assert_eq!(
            ranked.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["good", "person"]
        );
    }

    #[test]
    fn installed_authority_exact_identity_and_explicit_related_win() {
        let mut addon = Installed {
            url: "https://installed.example".into(), label: "Installed".into(), enabled: true,
            configure_ok: None, available: true, generation: 1,
            manifest: serde_json::from_value(json!({"id":"test", "version":"1.0.0", "name":"Test", "resources":["meta","catalog"], "types":["series"], "catalogs":[{"id":"top", "type":"series", "name":"Top", "extra":[{"name":"genre","options":["Crime"]}]}]})).unwrap(),
        };
        let mut calls = 0;
        assert!(
            retrieve(
                &[addon.clone()],
                "series",
                "private:123",
                &tmdb::Settings::default(),
                &mut |_| { panic!("opaque seed must not query wildcard sources") }
            )
            .is_empty()
        );
        let explicit = retrieve(
            &[addon.clone()],
            "series",
            "tt100",
            &tmdb::Settings::default(),
            &mut |url| {
                calls += 1;
                assert!(url.starts_with("https://installed.example/meta/series/tt100.json"));
                Some(serde_json::to_vec(&json!({"meta":{"id":"tt100", "type":"series", "name":"Seed", "related":[{"id":"related","type":"series","name":"Actual relation", "novaSourceUrl":"https://forged.example"}]}})).unwrap())
            },
        );
        assert_eq!(calls, 1);
        assert_eq!(explicit[0].id, "related");
        assert_eq!(
            explicit[0].extra["novaSourceUrl"],
            "https://installed.example"
        );
        addon.enabled = false;
        assert!(
            retrieve(
                &[addon.clone()],
                "series",
                "tt100",
                &tmdb::Settings::default(),
                &mut |_| panic!("disabled source requested")
            )
            .is_empty()
        );
        addon.enabled = true;
        assert!(retrieve(&[addon], "series", "tt100", &tmdb::Settings::default(), &mut |_| Some(br#"{"meta":{"id":"wrong","type":"series","name":"Wrong","genres":["Drama","Crime"]}}"#.to_vec())).is_empty());
    }

    #[test]
    fn fallback_uses_only_advertised_encoded_genres_and_bounded_catalogs() {
        let manifest = serde_json::from_value(json!({
            "id":"genres", "version":"1.0.0", "name":"Genres", "types":["series"],
            "resources":["meta", "catalog"], "catalogs":[
                {"id":"search-only", "type":"series", "name":"Search", "extra":[{"name":"search", "isRequired":true}, {"name":"genre"}]},
                {"id":"no-filter", "type":"series", "name":"No filter"},
                {"id":"popular", "type":"series", "name":"Popular", "extra":[{"name":"genre", "options":["Action & Adventure", "Crime", "Drama"]}]},
                {"id":"other", "type":"series", "name":"Other", "extra":[{"name":"genre"}]}
            ]
        })).unwrap();
        let addon = Installed {
            url: "https://installed.example".into(),
            label: "Installed".into(),
            enabled: true,
            configure_ok: None,
            available: true,
            generation: 1,
            manifest,
        };
        let mut requests = Vec::new();
        let rows = retrieve(
            &[addon],
            "series",
            "tt100",
            &tmdb::Settings::default(),
            &mut |url| {
                requests.push(url.to_owned());
                Some(serde_json::to_vec(&if url.contains("/meta/") {
                json!({"meta":{"id":"tt100", "type":"series", "name":"Seed", "genres":["Not advertised", "Action & Adventure", "Crime", "Mystery", "Drama"], "description":"An investigator uncovers a criminal conspiracy behind mysterious disappearances."}})
            } else {
                json!({"metas":[
                    {"id":"related", "type":"series", "name":"Related", "genres":["Crime", "Mystery"], "description":"An investigator investigates mysterious disappearances and a criminal conspiracy.", "novaSourceUrl":"https://forged.example"},
                    {"id":"broad", "type":"series", "name":"Unrelated", "genres":["Drama"]}
                ]})
            }).unwrap())
            },
        );
        assert_eq!(requests.len(), 3);
        assert!(
            requests[1].contains("/catalog/series/popular/genre=Action%20%26%20Adventure.json")
        );
        assert!(requests[2].contains("/catalog/series/popular/genre=Crime.json"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "related");
        assert_eq!(rows[0].extra["novaSourceUrl"], "https://installed.example");
    }
}
