//! Bounded process-memory stream snapshots; explicit selection owns playback.
use super::*;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(300);
const MAX_ENTRIES: usize = 32;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const MAX_LISTENERS: usize = 16;
type Progress = Arc<dyn Fn(Vec<u8>) + Send + Sync>;
type Complete = Box<dyn FnOnce(Result<StreamResponse, net::FetchError>) + Send>;

/// Cache origin is transport metadata, independent of provider labels or rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StreamResponse {
    pub bytes: Vec<u8>,
    pub cache_hit: bool,
}

impl StreamResponse {
    fn fetched(bytes: Vec<u8>, url: &str) -> Self {
        // Remote addons cannot claim our reserved provider response metadata.
        let cache_hit = url.starts_with("nova-provider://")
            && serde_json::from_slice::<serde_json::Value>(&bytes).is_ok_and(|value| {
                value
                    .get("novaCacheHit")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            });
        Self { bytes, cache_hit }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PlaybackKey {
    pub session: u64,
    pub series: String,
    pub episode: String,
    pub settings: u64,
    pub owners: Vec<(String, u64)>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct RequestKey {
    epoch: u64,
    url: String,
    owner: String,
    owner_generation: u64,
    settings_generation: u64,
}

struct Pending {
    listeners: Vec<(Progress, Complete)>,
    // A picker joining speculative work takes ownership, so closing the old
    // episode does not discard the answer it is actively waiting for.
    session: Option<u64>,
    target: Option<PlaybackKey>,
}

#[derive(Default)]
pub(super) struct Cache {
    epoch: u64,
    pending: HashMap<RequestKey, Pending>,
    ready: HashMap<RequestKey, (Instant, Vec<u8>)>,
}

impl Cache {
    pub(super) fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.ready.clear();
    }

    fn prune(&mut self) {
        self.ready.retain(|_, (at, _)| at.elapsed() < TTL);
        while self.ready.len() > MAX_ENTRIES
            || self
                .ready
                .values()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>()
                > MAX_BYTES
        {
            let Some(oldest) = self
                .ready
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.ready.remove(&oldest);
        }
    }
}

fn current_owner(state: &Shared, key: &RequestKey) -> bool {
    nova_providers::plugin_settings::settings_generation() == key.settings_generation
        && state.installed.iter().any(|addon| {
            addon.url == key.owner
                && addon.generation == key.owner_generation
                && addon.enabled
                && addon.available
        })
}

fn cacheable(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_RESPONSE
        && serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|value| {
            value
                .get("novaWarnings")
                .is_none_or(|warnings| warnings.as_array().is_some_and(Vec::is_empty))
        })
        && Addon::parse_streams(bytes).is_ok_and(|streams| !streams.is_empty())
}

impl Bridge {
    pub(super) fn prefetch_episode_streams(&self, request_id: String, session: u64) {
        let urls = {
            let state = self.shared.lock().unwrap();
            if !state.cache_settings.prefetch_next_episode_streams
                || !state
                    .playback
                    .as_ref()
                    .is_some_and(|p| p.session == session)
            {
                return;
            }
            detail::stream_search_urls_for(&state, &request_id)
        };
        for (_, url, owner, owner_generation) in urls {
            self.request_streams_cached(
                url,
                owner,
                owner_generation,
                nova_providers::plugin_settings::settings_generation(),
                Arc::new(|_| {}),
                Box::new(|_| {}),
                Some(session),
            );
        }
    }

    pub(super) fn request_streams_with_prefetch_cache(
        &self,
        url: String,
        owner: String,
        owner_generation: u64,
        settings_generation: u64,
        on_progress: Progress,
        on_complete: impl FnOnce(Result<StreamResponse, net::FetchError>) + Send + 'static,
    ) {
        self.request_streams_cached(
            url,
            owner,
            owner_generation,
            settings_generation,
            on_progress,
            Box::new(on_complete),
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn request_streams_cached(
        &self,
        url: String,
        owner: String,
        owner_generation: u64,
        settings_generation: u64,
        on_progress: Progress,
        on_complete: Complete,
        session: Option<u64>,
    ) {
        let mut state = self.shared.lock().unwrap();
        let key = RequestKey {
            epoch: state.stream_prefetch.epoch,
            url: url.clone(),
            owner,
            owner_generation,
            settings_generation,
        };
        if let Some(session) = session
            && (!state.cache_settings.prefetch_next_episode_streams
                || state.stream_prefetch_live_session != Some(session)
                || !self.player.can_prefetch_streams())
        {
            return;
        }
        let target = session.and_then(|_| state.playback.as_ref()?.next_episode_prefetched.clone());
        let valid = current_owner(&state, &key);
        let cache = &mut state.stream_prefetch;
        cache.prune();
        if valid && let Some((_, bytes)) = cache.ready.get(&key) {
            let bytes = bytes.clone();
            drop(state);
            if session.is_none() {
                // Matching the transport continuation keeps JSON decoding away
                // from the UI thread and never logs a potentially signed URL.
                crate::app::profiles::thread::Builder::new()
                    .name("stream-cache-hit".into())
                    .spawn(move || {
                        on_complete(Ok(StreamResponse {
                            bytes,
                            cache_hit: true,
                        }))
                    })
                    .expect("spawn cached stream delivery");
            }
            return;
        }
        if let Some(pending) = cache.pending.get_mut(&key) {
            if pending.listeners.len() >= MAX_LISTENERS {
                drop(state);
                if session.is_none() {
                    let response_url = url.clone();
                    net::fetch_bytes_with_progress(url, on_progress, move |result| {
                        on_complete(
                            result.map(|bytes| StreamResponse::fetched(bytes, &response_url)),
                        );
                    });
                }
                return;
            }
            if session.is_none() {
                pending.session = None;
                pending.target = None;
            }
            // Do not replay a progressive snapshot before registration: the
            // final completion could otherwise race ahead and strand a spinner.
            pending.listeners.push((on_progress, on_complete));
            return;
        }
        if cache.pending.len() >= MAX_ENTRIES {
            drop(state);
            // A saturated speculative queue must never delay an explicit picker.
            if session.is_none() {
                let response_url = url.clone();
                net::fetch_bytes_with_progress(url, on_progress, move |result| {
                    on_complete(result.map(|bytes| StreamResponse::fetched(bytes, &response_url)));
                });
            }
            return;
        }
        cache.pending.insert(
            key.clone(),
            Pending {
                listeners: vec![(on_progress, on_complete)],
                session,
                target,
            },
        );
        drop(state);
        let progress_bridge = self.clone();
        let progress_key = key.clone();
        let final_bridge = self.clone();
        let response_url = url.clone();
        net::fetch_bytes_with_progress(
            url,
            Arc::new(move |bytes| {
                let listeners = {
                    let mut state = progress_bridge.shared.lock().unwrap();
                    if !current_owner(&state, &progress_key) {
                        return;
                    }
                    let Some(pending) = state.stream_prefetch.pending.get_mut(&progress_key) else {
                        return;
                    };
                    pending
                        .listeners
                        .iter()
                        .map(|(progress, _)| progress.clone())
                        .collect::<Vec<_>>()
                };
                for listener in listeners {
                    listener(bytes.clone());
                }
            }),
            move |result| {
                // Parsing an addon snapshot can be substantial; do it before taking
                // the state lock that the UI thread also uses.
                let may_cache = result.as_ref().is_ok_and(|bytes| cacheable(bytes));
                let listeners = {
                    let mut state = final_bridge.shared.lock().unwrap();
                    let Some(pending) = state.stream_prefetch.pending.remove(&key) else {
                        return;
                    };
                    let playback_valid = pending.session.is_none_or(|session| {
                        state.cache_settings.prefetch_next_episode_streams
                            && state.stream_prefetch_live_session == Some(session)
                            && state.playback.as_ref().is_some_and(|p| {
                                p.session == session
                                    && !p.next_episode_dismissed
                                    && pending.target.as_ref().is_some_and(|target| {
                                        state
                                            .modal_item
                                            .as_ref()
                                            .filter(|m| m.id == target.series)
                                            .and_then(|m| {
                                                next_episode::successor(
                                                    &m.videos,
                                                    &p.episode_id,
                                                    today_days(),
                                                )
                                            })
                                            .is_some_and(|next| next.id == target.episode)
                                    })
                            })
                            && final_bridge.player.can_prefetch_streams()
                    });
                    if key.epoch == state.stream_prefetch.epoch
                        && current_owner(&state, &key)
                        && playback_valid
                        && may_cache
                        && let Ok(bytes) = &result
                    {
                        state
                            .stream_prefetch
                            .ready
                            .insert(key, (Instant::now(), bytes.clone()));
                        state.stream_prefetch.prune();
                    }
                    pending.listeners
                };
                for (_, complete) in listeners {
                    complete(match &result {
                        Ok(bytes) => Ok(StreamResponse::fetched(bytes.clone(), &response_url)),
                        Err(error) => Err(net::FetchError {
                            status: error.status,
                            message: error.message.clone(),
                        }),
                    });
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_internal_provider_transport_can_report_provider_cache_origin() {
        let bytes = br#"{"streams":[],"novaCacheHit":true}"#.to_vec();
        assert!(
            StreamResponse::fetched(
                bytes.clone(),
                "nova-provider://fixture/stream/movie/id.json"
            )
            .cache_hit
        );
        assert!(
            !StreamResponse::fetched(bytes, "https://example.test/stream/movie/id.json").cache_hit
        );
        for bytes in [
            b"not json".as_slice(),
            br#"{"novaCacheHit":"true"}"#,
            br#"{}"#,
        ] {
            assert!(!StreamResponse::fetched(bytes.to_vec(), "nova-provider://fixture").cache_hit);
        }
    }

    #[test]
    fn empty_invalid_and_oversized_snapshots_are_not_cached() {
        assert!(!cacheable(br#"{"streams":[]}"#));
        assert!(!cacheable(b"not json"));
        assert!(!cacheable(br#"{"streams":[{"url":"https://example.test/video"}],"novaWarnings":[{"message":"retry"}]}"#));
        assert!(cacheable(
            br#"{"streams":[{"url":"https://example.test/video"}]}"#
        ));
        assert!(!cacheable(&vec![b' '; MAX_RESPONSE + 1]));
    }
    #[test]
    fn cache_prunes_oldest_and_expired_entries() {
        let mut cache = Cache::default();
        for index in 0..=MAX_ENTRIES {
            cache.ready.insert(
                RequestKey {
                    epoch: 0,
                    url: index.to_string(),
                    owner: String::new(),
                    owner_generation: 0,
                    settings_generation: 0,
                },
                (Instant::now() - Duration::from_secs(index as u64), vec![1]),
            );
        }
        cache.prune();
        assert_eq!(cache.ready.len(), MAX_ENTRIES);
        assert!(
            !cache
                .ready
                .keys()
                .any(|key| key.url == MAX_ENTRIES.to_string())
        );
        for (at, _) in cache.ready.values_mut() {
            *at = Instant::now() - TTL;
        }
        cache.prune();
        assert!(cache.ready.is_empty());
    }
    #[test]
    #[cfg(feature = "desktop")]
    fn manual_refresh_bypasses_ready_and_pending_work_and_rejects_late_cache_writes() {
        use std::io::{Read, Write};
        const ROOT: &str = "NOVA_MANUAL_STREAM_REFRESH_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-manual-stream-refresh-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args([
                    "--exact",
                    "app::stream_prefetch::tests::manual_refresh_bypasses_ready_and_pending_work_and_rejects_late_cache_writes",
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
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        storage::init_at(&root);
        const OLD: &[u8] = br#"{"streams":[{"url":"https://example.test/old"}]}"#;
        const FRESH: &[u8] = br#"{"streams":[{"url":"https://example.test/fresh"}]}"#;
        fn respond(mut socket: std::net::TcpStream, body: &[u8]) {
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            socket.write_all(body).unwrap();
        }
        for preload_ready in [true, false] {
            let app = AppWindow::new().unwrap();
            let player = crate::player::Player::setup(&app);
            let (hi, _) = mpsc::channel();
            let (lo, _) = mpsc::channel();
            let bridge = Bridge::new(
                app.as_weak(),
                PosterTx { hi, lo },
                Arc::new(AtomicU64::new(0)),
                player,
                DownloadCoordinator::new(root.join(format!("downloads-{preload_ready}"))),
            );
            let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let owner = format!("http://{}", server.local_addr().unwrap());
            let url = format!("{owner}/stream/series/tt123:1:1.json");
            let (accepted_tx, accepted_rx) = mpsc::channel();
            let (old_release_tx, old_release_rx) = mpsc::channel();
            let (fresh_release_tx, fresh_release_rx) = mpsc::channel();
            let fixture = thread::spawn(move || {
                let (mut old, _) = server.accept().unwrap();
                let mut request = [0; 4096];
                assert!(old.read(&mut request).unwrap() > 0);
                accepted_tx.send(()).unwrap();
                let old_worker = thread::spawn(move || {
                    if !preload_ready {
                        old_release_rx
                            .recv_timeout(Duration::from_secs(10))
                            .unwrap();
                    }
                    respond(old, OLD);
                });
                let (mut fresh, _) = server.accept().unwrap();
                assert!(fresh.read(&mut request).unwrap() > 0);
                accepted_tx.send(()).unwrap();
                fresh_release_rx
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
                respond(fresh, FRESH);
                old_worker.join().unwrap();
            });
            {
                let mut state = bridge.shared.lock().unwrap();
                state.installed.push(Installed {
                    url: owner.clone(),
                    label: "Fixture".into(),
                    enabled: true,
                    available: true,
                    configure_ok: None,
                    generation: 1,
                    manifest: serde_json::from_value(serde_json::json!({
                        "id":"fixture", "name":"Fixture", "version":"1.0.0",
                        "types":["series"], "resources":["stream"], "catalogs":[]
                    }))
                    .unwrap(),
                });
                state.modal_item = Some(ModalItem {
                    details: Default::default(),
                    open_token: Arc::new(()),
                    pending_watch_now: None,
                    episodes_loading: false,
                    id: "tt123".into(),
                    type_: "series".into(),
                    request_id: "tt123:1:1".into(),
                    videos: vec![],
                    season_backdrops: HashMap::new(),
                    seasons: vec![],
                    season_index: 0,
                    episode_page: 0,
                    name: "Fixture".into(),
                    year: String::new(),
                    poster_url: String::new(),
                    background_url: String::new(),
                    logo_url: String::new(),
                    description: String::new(),
                    genres: vec![],
                });
                state.stream_generation = 40;
                state.stream_addons = vec!["Fixture".into()];
                state.stream_filter = 1;
                state.stream_page = 3;
            }
            app.set_modal_visible(true);
            app.set_stream_selector_open(true);
            let settings = nova_providers::plugin_settings::settings_generation();
            let (old_done_tx, old_done_rx) = mpsc::channel();
            bridge.request_streams_with_prefetch_cache(
                url.clone(),
                owner.clone(),
                1,
                settings,
                Arc::new(|_| {}),
                move |response| old_done_tx.send(response.unwrap()).unwrap(),
            );
            accepted_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            if preload_ready {
                assert_eq!(
                    old_done_rx
                        .recv_timeout(Duration::from_secs(10))
                        .unwrap()
                        .bytes,
                    OLD
                );
                assert_eq!(bridge.shared.lock().unwrap().stream_prefetch.ready.len(), 1);
            }

            // Exercise the actual user action, not just Cache::invalidate.
            bridge.refresh_stream_sources();
            accepted_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(!app.get_stream_refresh_enabled());
            assert!(app.get_streams_searching());
            {
                let state = bridge.shared.lock().unwrap();
                assert_eq!(state.stream_generation, 41);
                assert_eq!(state.stream_filter, 1, "refresh keeps the chosen addon");
                assert_eq!(state.stream_page, 0);
                assert_eq!(state.stream_prefetch.epoch, 1);
                assert!(state.stream_prefetch.ready.is_empty());
                assert_eq!(
                    state.stream_prefetch.pending.len(),
                    if preload_ready { 1 } else { 2 }
                );
            }
            for _ in 0..10 {
                bridge.refresh_stream_sources();
            }
            assert_eq!(bridge.shared.lock().unwrap().stream_generation, 41);
            assert_eq!(bridge.shared.lock().unwrap().stream_prefetch.epoch, 1);
            // Join only the new epoch so its callback also acknowledges that
            // the picker's completion has already been queued on the UI.
            let (fresh_done_tx, fresh_done_rx) = mpsc::channel();
            bridge.request_streams_with_prefetch_cache(
                url.clone(),
                owner.clone(),
                1,
                settings,
                Arc::new(|_| {}),
                move |response| fresh_done_tx.send(response.unwrap()).unwrap(),
            );
            fresh_release_tx.send(()).unwrap();
            let fresh = fresh_done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(!fresh.cache_hit);
            assert_eq!(fresh.bytes, FRESH);
            slint::invoke_from_event_loop(|| slint::quit_event_loop().unwrap()).unwrap();
            slint::run_event_loop().unwrap();
            assert!(!app.get_streams_searching());
            assert_eq!(bridge.shared.lock().unwrap().stream_all.len(), 1);
            assert!(matches!(
                &bridge.shared.lock().unwrap().stream_all[0].source,
                StreamSource::Url(url) if url == "https://example.test/fresh"
            ));
            if !preload_ready {
                // Deliver the old response after fresh cache publication.
                old_release_tx.send(()).unwrap();
                assert_eq!(
                    old_done_rx
                        .recv_timeout(Duration::from_secs(10))
                        .unwrap()
                        .bytes,
                    OLD
                );
            }
            fixture.join().unwrap();
            let (cached_tx, cached_rx) = mpsc::channel();
            bridge.request_streams_with_prefetch_cache(
                url,
                owner,
                1,
                settings,
                Arc::new(|_| {}),
                move |response| cached_tx.send(response.unwrap()).unwrap(),
            );
            let cached = cached_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(cached.cache_hit);
            assert_eq!(
                cached.bytes, FRESH,
                "a late old completion cannot overwrite fresh cache"
            );
            let state = bridge.shared.lock().unwrap();
            assert!(state.stream_prefetch.pending.is_empty());
            assert_eq!(state.stream_prefetch.ready.len(), 1);
            assert!(state.stream_prefetch.ready.keys().all(|key| key.epoch == 1));
        }
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn picker_joins_prefetch_and_reuses_memory_snapshot() {
        use std::io::{Read, Write};
        const ROOT: &str = "NOVA_STREAM_PREFETCH_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-stream-prefetch-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args([
                    "--exact",
                    "app::stream_prefetch::tests::picker_joins_prefetch_and_reuses_memory_snapshot",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            let _ = fs::remove_dir_all(&root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        storage::init_at(&root);
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let owner = format!("http://{}", server.local_addr().unwrap());
        let url = format!("{owner}/stream/series/episode.json");
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let fixture = crate::app::profiles::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let mut request = [0; 4096];
            // This fixture only needs to know the request started; a partial
            // header is enough before holding the response for cancellation.
            assert!(socket.read(&mut request).unwrap() > 0);
            accepted_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            let body = br#"{"streams":[{"url":"https://example.test/video"}]}"#;
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            socket.write_all(body).unwrap();
        });
        let session = next_episode::new_session();
        {
            let mut state = bridge.shared.lock().unwrap();
            state.installed.push(Installed {
                url: owner.clone(),
                label: "Fixture".into(),
                enabled: true,
                available: true,
                configure_ok: None,
                generation: 1,
                manifest: serde_json::from_value(serde_json::json!({
                    "id":"fixture", "name":"Fixture", "version":"1.0.0",
                    "types":["series"], "resources":["stream"], "catalogs":[]
                }))
                .unwrap(),
            });
            state.playback = Some(PlaybackTarget {
                session,
                ..Default::default()
            });
            state.stream_prefetch_live_session = Some(session);
        }
        let settings = nova_providers::plugin_settings::settings_generation();
        bridge.request_streams_cached(
            url.clone(),
            owner.clone(),
            1,
            settings,
            Arc::new(|_| {}),
            Box::new(|_| {}),
            Some(session),
        );
        accepted_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        bridge.request_streams_with_prefetch_cache(
            url.clone(),
            owner.clone(),
            1,
            settings,
            Arc::new(|_| {}),
            move |result| done_tx.send(result.unwrap()).unwrap(),
        );
        // The old player can close while the joined picker continues waiting.
        bridge.clear_next_episode_prompt();
        assert_eq!(
            bridge.shared.lock().unwrap().stream_prefetch.pending.len(),
            1
        );
        release_tx.send(()).unwrap();
        let answer = done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        fixture.join().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        bridge.request_streams_with_prefetch_cache(
            url,
            owner,
            1,
            settings,
            Arc::new(|_| {}),
            move |result| done_tx.send(result.unwrap()).unwrap(),
        );
        assert!(!answer.cache_hit, "joining pending work is a live response");
        let cached = done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(cached.cache_hit);
        assert_eq!(cached.bytes, answer.bytes);
        let state = bridge.shared.lock().unwrap();
        assert!(state.stream_prefetch.pending.is_empty());
        assert_eq!(state.stream_prefetch.ready.len(), 1);
    }
}
