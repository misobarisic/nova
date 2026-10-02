//! Addon install / remove / refresh and the addon picker rows.
use super::*;
static ADDON_GENERATION: AtomicU64 = AtomicU64::new(1);

impl Bridge {
    #[cfg(test)]
    pub(super) fn assert_desired_addon_regression(&self) {
        let a = "http://127.0.0.1:1/a";
        let b = "http://127.0.0.1:1/b";
        let generation = self.ensure_desired_addon(a, false, Some(false), Some("A".into()));
        self.ensure_desired_addon(b, true, Some(false), Some("B".into()));
        assert_eq!(
            read_persisted_addons()
                .iter()
                .map(|a| a.url.as_str())
                .collect::<Vec<_>>(),
            vec![a, b]
        );
        assert!(
            self.shared
                .lock()
                .unwrap()
                .installed
                .iter()
                .all(|a| !a.available)
        );
        let manifest = |name: &str| {
            serde_json::from_value::<Manifest>(
                serde_json::json!({"id":name,"version":"1","name":name}),
            )
            .unwrap()
        };
        // Opposite completion order and stale enabled arguments must not alter
        // desired order/flags captured after the fetch started.
        self.install_ok(b.into(), manifest("B"), false, Some(false), None);
        self.install_ok(a.into(), manifest("A"), true, Some(false), None);
        let persisted = read_persisted_addons();
        assert_eq!(
            persisted.iter().map(|a| a.url.as_str()).collect::<Vec<_>>(),
            vec![a, b]
        );
        assert!(!persisted[0].enabled);
        assert!(persisted[1].enabled);
        self.remove_addon_at(0);
        assert!(!self.addon_generation_matches(a, generation));
        self.install_ok(a.into(), manifest("stale"), true, Some(false), None);
        assert_eq!(read_persisted_addons().len(), 1);
        self.ensure_desired_addon(a, true, Some(false), Some("new".into()));
        assert!(!self.addon_generation_matches(a, generation));
    }

    /// Validate + normalise the URL on the main thread, then fetch the
    /// manifest on a worker thread. New UI addons are installed enabled.
    pub(super) fn add_addon(&self, raw: &str) {
        self.install_new(raw, true, None);
    }

    pub(super) fn install_new(&self, raw: &str, enabled: bool, label: Option<String>) {
        let addon = match Addon::new(raw) {
            Ok(a) => a,
            Err(_) => {
                return;
            }
        };
        let base = addon.base_url().to_string();
        {
            let mut state = self.shared.lock().unwrap();
            let dup = state.installed.iter().position(|a| a.url == base);
            if let Some(i) = dup.filter(|&i| state.installed[i].available) {
                // Pasting a URL that is installed but disabled re-enables it.
                if !state.installed[i].enabled {
                    state.installed[i].enabled = true;
                    drop(state);
                    self.persist_installed();
                    self.refresh_all(true);
                } else {
                    drop(state);
                }
                return;
            }
        }

        let generation = self.ensure_desired_addon(&base, enabled, None, label.clone());
        if !self.shared.lock().unwrap().refreshing.insert(base.clone()) {
            return;
        }
        let remote_origin = applying() || self.shared.lock().unwrap().loading_addons;
        let bridge = self.clone();
        let manifest_url = addon.manifest_url();
        crate::web_log("nova: fetching addon manifest");
        net::fetch_bytes(manifest_url, move |result| {
            let bridge2 = bridge.clone();
            // Panic-guarded: a panic inside this UI-thread closure would
            // otherwise kill the event loop (app crash with no message).
            // Log it with the cause.
            let _ = slint::invoke_from_event_loop(move || {
                if !bridge2.addon_generation_matches(&base, generation) {
                    return;
                }
                bridge2.shared.lock().unwrap().refreshing.remove(&base);
                let _origin = remote_origin.then(ApplyingGuard::new);
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match result {
                        Ok(bytes) => match Addon::parse_manifest(&bytes) {
                            Ok(manifest) => {
                                crate::web_log("nova: addon manifest OK — installing");
                                bridge2.install_ok(base, manifest, enabled, None, label.clone());
                                None
                            }
                            Err(e) => Some(text::invalid_manifest(&e.to_string())),
                        },
                        Err(e) => Some(text::addon_unreachable(&e.to_message())),
                    }));
                let _msg = match outcome {
                    Ok(None) => return,
                    Ok(Some(msg)) => msg,
                    Err(payload) => {
                        let detail = if let Some(s) = payload.downcast_ref::<String>() {
                            s.clone()
                        } else if let Some(s) = payload.downcast_ref::<&str>() {
                            s.to_string()
                        } else {
                            "unknown panic payload".to_string()
                        };
                        text::install_failed(&detail)
                    }
                };
                crate::web_log("nova: addon installation failed");
            });
        });
    }

    fn ensure_desired_addon(
        &self,
        base: &str,
        enabled: bool,
        configure: Option<bool>,
        label: Option<String>,
    ) -> u64 {
        let generation = {
            let mut state = self.shared.lock().unwrap();
            if let Some(a) = state.installed.iter().find(|a| a.url == base) {
                return a.generation;
            }
            let generation = ADDON_GENERATION.fetch_add(1, Ordering::Relaxed);
            let label = label
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| base.to_string());
            let manifest: Manifest = serde_json::from_value(
                serde_json::json!({"id":"pending","version":"0","name":label}),
            )
            .unwrap();
            state.installed.push(Installed {
                url: base.to_string(),
                label,
                enabled,
                configure_ok: configure,
                manifest,
                available: false,
                generation,
            });
            generation
        };
        self.persist_installed();
        self.apply_addon_rows();
        generation
    }

    fn addon_generation_matches(&self, base: &str, generation: u64) -> bool {
        self.shared
            .lock()
            .unwrap()
            .installed
            .iter()
            .any(|a| a.url == base && a.generation == generation)
    }

    /// Main thread: record a successfully fetched addon, refresh pickers and
    /// persist it (config entry + manifest cache) so the next launch can load
    /// it without touching the network. `configure` is the stored
    /// configure-page verdict (`None` for fresh inserts and legacy entries,
    /// which are probed now and written down).
    pub(super) fn install_ok(
        &self,
        url: String,
        manifest: Manifest,
        _enabled: bool,
        _configure: Option<bool>,
        _label_override: Option<String>,
    ) {
        let label = {
            let state = self.shared.lock().unwrap();
            let Some(desired) = state.installed.iter().find(|a| a.url == url) else {
                return;
            };
            if desired.label != url && !desired.label.trim().is_empty() {
                desired.label.clone()
            } else {
                let base = manifest.name.clone();
                if state.installed.iter().any(|a| a.label == base) {
                    format!("{base} ({url})")
                } else {
                    base
                }
            }
        };
        {
            let mut state = self.shared.lock().unwrap();
            let Some(entry) = state.installed.iter_mut().find(|a| a.url == url) else {
                return;
            };
            entry.label = label;
            entry.manifest = manifest.clone();
            entry.available = true;
            // Stay on "All addons" so the new addon's catalogs are visible.
            state.chosen_addon = usize::MAX;
        }
        write_cached_manifest_for(&url, &manifest);
        self.persist_installed();
        // Resolve any label collisions deterministically (identical across
        // devices), then refresh.
        self.normalize_addon_labels();
        self.refresh_all(true);
        // Verify the addon's configure page in the background unless a
        // verdict is already written down; the Configure button appears
        // only once a probe answers 2xx.
        if self
            .shared
            .lock()
            .unwrap()
            .installed
            .iter()
            .find(|a| a.url == url)
            .is_some_and(|a| a.configure_ok.is_none())
        {
            self.probe_configure_page(url);
        }
    }

    /// Give duplicate addon names a stable `"$NAME ($URL)"` label. Among
    /// addons sharing a manifest name, the lexicographically smallest URL
    /// keeps the plain name and the rest get the URL suffix, so every device
    /// that has the same set shows identical labels. Returns true when any
    /// label changed.
    pub(super) fn normalize_addon_labels(&self) -> bool {
        let changed = {
            let mut state = self.shared.lock().unwrap();
            let pairs: Vec<(String, String)> = state
                .installed
                .iter()
                .filter(|a| a.available)
                .map(|a| (a.manifest.name.clone(), a.url.clone()))
                .collect();
            let labels = unique_labels(&pairs);
            let mut changed = false;
            for (addon, label) in state
                .installed
                .iter_mut()
                .filter(|a| a.available)
                .zip(labels)
            {
                if addon.label != label {
                    addon.label = label;
                    changed = true;
                }
            }
            changed
        };
        if changed {
            self.persist_installed();
            self.apply_addon_rows();
        }
        changed
    }

    /// Background: GET `<base>/configure` and remember whether it serves a
    /// page. A 404 (or any failure) simply leaves the Configure button
    /// hidden; the body is discarded.
    pub(super) fn probe_configure_page(&self, base: String) {
        if base == nova_providers::ANIKOTO_PROVIDER_URL {
            self.set_configure_state(&base, false);
            return;
        }
        let Some(generation) = self
            .shared
            .lock()
            .unwrap()
            .installed
            .iter()
            .find(|a| a.url == base)
            .map(|a| a.generation)
        else {
            return;
        };
        let url = format!("{}/configure", base.trim_end_matches('/'));
        let bridge = self.clone();
        net::fetch_bytes(url, move |result| {
            let ok = result.is_ok();
            let _ = slint::invoke_from_event_loop(move || {
                if !bridge.addon_generation_matches(&base, generation) {
                    return;
                }
                let _guard = ApplyingGuard::new(); // reachability is device-local derived state
                bridge.set_configure_state(&base, ok);
            });
        });
    }

    /// Main thread: record a configure-page probe result, write it down,
    /// and refresh the Settings list if the Configure button's visibility
    /// changed.
    pub(super) fn set_configure_state(&self, base: &str, ok: bool) {
        let changed = {
            let mut state = self.shared.lock().unwrap();
            match state.installed.iter_mut().find(|a| a.url == base) {
                Some(a) if a.configure_ok != Some(ok) => {
                    a.configure_ok = Some(ok);
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.persist_installed();
            self.apply_addon_rows();
        }
    }

    /// Main thread: install an addon that is already persisted, preferring
    /// its cached manifest. Only when the cache misses is the manifest
    /// re-fetched from the addon server.
    pub(super) fn install_persisted(
        &self,
        url: &str,
        enabled: bool,
        configure: Option<bool>,
        label: Option<String>,
    ) {
        let base = match Addon::new(url) {
            Ok(a) => a.base_url().to_string(),
            Err(_) => return,
        };
        {
            let state = self.shared.lock().unwrap();
            if state.installed.iter().any(|a| a.url == base && a.available) {
                return; // already installed (duplicate entry or UI add)
            }
        }
        self.ensure_desired_addon(&base, enabled, configure, label.clone());
        match read_cached_manifest_for(&base) {
            Some(manifest) => self.install_ok(base, manifest, enabled, configure, label),
            None => self.install_new(url, enabled, label),
        }
    }

    /// Snapshot the installed addons (URLs + enabled flags + configure
    /// verdicts) into the KV store.
    pub(super) fn persist_installed(&self) {
        if self.shared.lock().unwrap().loading_addons {
            return;
        }
        let addons: Vec<AddonStore> = {
            let state = self.shared.lock().unwrap();
            state
                .installed
                .iter()
                .map(|a| AddonStore {
                    url: a.url.clone(),
                    enabled: a.enabled,
                    configure_ok: a.configure_ok,
                    label: a.label.clone(),
                })
                .collect()
        };
        write_persisted_addons(&addons);
    }

    /// Settings → Addons: flip the enabled flag of the addon at `idx` and
    /// refresh. Disabling the addon currently browsed in Discover drops back
    /// to the merged "All addons" view.
    pub(super) fn toggle_addon(&self, idx: usize) {
        let reset_chosen = {
            let mut state = self.shared.lock().unwrap();
            let chosen = state.chosen_addon;
            let Some(entry) = state.installed.get_mut(idx) else {
                return;
            };
            entry.enabled = !entry.enabled;
            let now_enabled = entry.enabled;
            !now_enabled && chosen == idx
        };
        if reset_chosen {
            let mut state = self.shared.lock().unwrap();
            state.chosen_addon = usize::MAX;
        }
        self.persist_installed();
        self.refresh_all(true);
    }

    /// Settings → Addons: re-fetch the manifest of the addon at `idx` from
    /// its already-known manifest URL and refresh all stored metadata
    /// derived from it (label, manifest + manifest cache, merged catalogs,
    /// configure-page verdict). Keeps the old data on failure.
    pub(super) fn refresh_addon(&self, idx: usize) {
        let (base, generation) = {
            let mut state = self.shared.lock().unwrap();
            let Some(entry) = state.installed.get(idx) else {
                return;
            };
            let base = entry.url.clone();
            let generation = entry.generation;
            if !state.refreshing.insert(base.clone()) {
                return; // a refresh for this addon is already in flight
            }
            (base, generation)
        };
        let manifest_url = match Addon::new(&base) {
            Ok(a) => a.manifest_url(),
            Err(_) => {
                self.shared.lock().unwrap().refreshing.remove(&base);
                crate::web_log("nova: cannot refresh invalid addon URL");
                return;
            }
        };
        let bridge = self.clone();
        crate::web_log("nova: refreshing addon manifest");
        net::fetch_bytes(manifest_url, move |result| {
            let _ = slint::invoke_from_event_loop(move || {
                if !bridge.addon_generation_matches(&base, generation) {
                    return;
                }
                match result {
                    Ok(bytes) => match Addon::parse_manifest(&bytes) {
                        Ok(manifest) => bridge.refresh_ok(base, manifest),
                        Err(_) => {
                            bridge.shared.lock().unwrap().refreshing.remove(&base);
                            crate::web_log("nova: invalid refreshed addon manifest");
                        }
                    },
                    Err(_) => {
                        bridge.shared.lock().unwrap().refreshing.remove(&base);
                        crate::web_log("nova: addon refresh request failed");
                    }
                }
            });
        });
    }

    /// Main thread: apply a re-fetched manifest to the installed addon at
    /// `base`, refreshing everything stored from it.
    pub(super) fn refresh_ok(&self, base: String, manifest: Manifest) {
        {
            let mut state = self.shared.lock().unwrap();
            state.refreshing.remove(&base);
            let Some(pos) = state.installed.iter().position(|a| a.url == base) else {
                return;
            };
            // Only re-derive the label when the manifest name actually
            // changed; otherwise keep the current (possibly synced) label.
            let name_changed = state.installed[pos].manifest.name != manifest.name;
            let wanted = manifest.name.clone();
            let taken: Vec<String> = state
                .installed
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != pos)
                .map(|(_, a)| a.label.clone())
                .collect();
            let entry = &mut state.installed[pos];
            if name_changed {
                entry.label = if taken.contains(&wanted) {
                    format!("{wanted} ({base})")
                } else {
                    wanted
                };
            }
            entry.manifest = manifest.clone();
            entry.available = true;
        }
        write_cached_manifest_for(&base, &manifest);
        self.persist_installed();
        self.normalize_addon_labels();
        // The configure page may have appeared or gone; re-verify (writes
        // the verdict down and refreshes the list on change).
        self.probe_configure_page(base);
        self.refresh_all(true);
    }

    /// Settings → Addons: remove (uninstall) the addon at `idx`.
    pub(super) fn remove_addon_at(&self, idx: usize) {
        let removed_url = {
            let mut state = self.shared.lock().unwrap();
            if state.installed.is_empty() {
                return;
            }
            let idx = idx.min(state.installed.len() - 1);
            let removed = state.installed.remove(idx);
            state.refreshing.remove(&removed.url);
            if state.chosen_addon != usize::MAX {
                if state.chosen_addon == idx {
                    // The removed addon was the one being browsed.
                    state.chosen_addon = usize::MAX;
                } else if state.chosen_addon > idx {
                    state.chosen_addon -= 1; // later addons shifted down
                }
            }
            removed.url
        };
        delete_cached_manifest_for(&removed_url);
        self.persist_installed();
        // A removed duplicate may free the plain name for a remaining addon.
        self.normalize_addon_labels();
        self.refresh_all(true);
    }

    /// Settings → Addons: move the addon at `idx` by `delta` slots (-1 = up,
    /// +1 = down). Order drives picker/filter/stream precedence and syncs
    /// across devices via the `order` record. The browsed addon is tracked
    /// by URL so `chosen_addon` survives the shift.
    pub(super) fn move_addon(&self, idx: usize, delta: i32) {
        {
            let mut state = self.shared.lock().unwrap();
            let chosen_url = (state.chosen_addon != usize::MAX)
                .then(|| {
                    state
                        .installed
                        .get(state.chosen_addon)
                        .map(|a| a.url.clone())
                })
                .flatten();
            let Some(_) = Self::move_item(&mut state.installed, idx, delta) else {
                return;
            };
            if let Some(url) = chosen_url {
                state.chosen_addon = state
                    .installed
                    .iter()
                    .position(|a| a.url == url)
                    .unwrap_or(usize::MAX);
            }
        }
        self.persist_installed();
        self.apply_addon_rows();
        self.refresh_all(true);
    }

    /// Rebuild picker lists from the chosen addon's manifest and reload.
    pub(super) fn refresh_all(&self, load: bool) {
        let app = match self.app() {
            Some(a) => a,
            None => return,
        };

        let mut state = self.shared.lock().unwrap();

        // Only enabled addons feed Discover; a selection pointing at a
        // disabled or removed addon falls back to the merged "All addons".
        let addon_count = state.installed.len();
        if state.chosen_addon != usize::MAX
            && (state.chosen_addon >= addon_count || !state.installed[state.chosen_addon].enabled)
        {
            state.chosen_addon = usize::MAX;
        }
        let enabled_idxs: Vec<usize> = state
            .installed
            .iter()
            .enumerate()
            .filter(|(_, a)| a.enabled)
            .map(|(i, _)| i)
            .collect();

        // Rebuild type/catalog definitions from the active addon.
        state.type_defs.clear();
        if state.chosen_addon == usize::MAX && !enabled_idxs.is_empty() {
            // "All addons" — merge catalogs from every enabled addon.
            state.type_defs = build_merged_type_defs(&state.installed);
        } else if state.chosen_addon < addon_count {
            let manifest = &state.installed[state.chosen_addon].manifest;
            let label = state.installed[state.chosen_addon].label.clone();
            state.type_defs = build_type_defs(manifest, &label);
        }
        state.chosen_type = 0;
        state.chosen_catalog = 0;
        state.chosen_genre.clear();
        let has_grid_source = !state.type_defs.is_empty();

        // Collect the model contents while holding the lock.
        let mut addon_names: Vec<SharedString> = Vec::new();
        if !enabled_idxs.is_empty() {
            addon_names.push(SharedString::from(text::tr("All addons")));
        }
        for i in &enabled_idxs {
            addon_names.push(SharedString::from(&state.installed[*i].label));
        }
        let type_names: Vec<SharedString> = state
            .type_defs
            .iter()
            .map(|t| SharedString::from(&t.label))
            .collect();
        let catalog_names: Vec<SharedString> = state
            .type_defs
            .first()
            .map(|t| {
                t.catalogs
                    .iter()
                    .map(|c| SharedString::from(&c.label))
                    .collect()
            })
            .unwrap_or_default();

        let hint = if enabled_idxs.is_empty() {
            if addon_count == 0 && !state.loading_addons {
                text::tr("No addons installed yet — add one in Settings → Addons.").to_string()
            } else if addon_count > 0 && !state.loading_addons {
                text::tr("All addons are disabled — enable one in Settings → Addons.").to_string()
            } else {
                text::tr("Loading configured add-ons…").to_string()
            }
        } else if !has_grid_source {
            text::no_browsable_catalogs(&state.installed[enabled_idxs[0]].label)
        } else {
            String::new()
        };

        let chosen_addon = state.chosen_addon;
        if !load {
            state.previews.clear();
        }
        let addon_rows: Vec<AddonRow> = state
            .installed
            .iter()
            .map(|a| AddonRow {
                label: SharedString::from(&a.label),
                url: SharedString::from(&a.url),
                enabled: a.enabled,
                config_url: SharedString::from(Self::addon_config_url(a)),
            })
            .collect();
        drop(state);

        app.set_addon_names(Rc::new(VecModel::from(addon_names)).into());
        app.set_type_names(Rc::new(VecModel::from(type_names)).into());
        app.set_catalog_names(Rc::new(VecModel::from(catalog_names)).into());
        app.set_addon_rows(Rc::new(VecModel::from(addon_rows)).into());
        // Map chosen_addon to the combo index over the *enabled* addons only:
        // "All addons" = 0, others = position in the enabled list + 1.
        let addon_combo_idx = if chosen_addon == usize::MAX {
            0
        } else {
            enabled_idxs
                .iter()
                .position(|&i| i == chosen_addon)
                .map(|p| p + 1)
                .unwrap_or(0)
        };
        app.set_addon_combo_idx(if !enabled_idxs.is_empty() {
            addon_combo_idx as i32
        } else {
            -1
        });
        app.set_type_combo_idx(0);
        app.set_catalog_combo_idx(0);
        self.apply_catalog_labels_to_ui();
        self.apply_genre_selection_to_ui();
        self.apply_search_support_to_ui();
        self.apply_home_catalog_rows();
        self.invalidate_home_showcase();
        if app.get_show_home() {
            self.ensure_home_showcase_loaded();
        }

        if load && has_grid_source {
            self.load_catalog();
        } else {
            let mut state = self.shared.lock().unwrap();
            state.previews.clear();
            drop(state);
            app.set_catalog(Rc::new(VecModel::<MediaCard>::from(vec![])).into());
            app.set_empty_hint(SharedString::from(hint));
        }
    }

    /// Refresh only localized Discover picker text. `refresh_all` also resets
    /// selection and loads a catalog, so it is not appropriate for a language
    /// change while the user is already browsing.
    pub(super) fn refresh_addon_picker_language_text(&self) {
        let Some(app) = self.app() else { return };
        let (names, empty_hint) = {
            let state = self.shared.lock().unwrap();
            let enabled: Vec<&Installed> = state.installed.iter().filter(|a| a.enabled).collect();
            let mut names =
                Vec::with_capacity(enabled.len() + if enabled.is_empty() { 0 } else { 1 });
            if !enabled.is_empty() {
                names.push(SharedString::from(text::tr("All addons")));
            }
            names.extend(enabled.iter().map(|addon| SharedString::from(&addon.label)));

            let hint = if enabled.is_empty() {
                if state.installed.is_empty() && !state.loading_addons {
                    text::tr("No addons installed yet — add one in Settings → Addons.").to_string()
                } else if !state.installed.is_empty() && !state.loading_addons {
                    text::tr("All addons are disabled — enable one in Settings → Addons.")
                        .to_string()
                } else {
                    text::tr("Loading configured add-ons…").to_string()
                }
            } else if state.type_defs.is_empty() {
                text::no_browsable_catalogs(&enabled[0].label)
            } else {
                text::tr("No results for this selection.").to_string()
            };
            (names, hint)
        };
        app.set_addon_names(Rc::new(VecModel::from(names)).into());
        app.set_empty_hint(SharedString::from(empty_hint));
    }

    /// The addon's web configuration page (`<addon base>/configure`, e.g.
    /// https://torrentio.strem.fun/configure). Only returned once a probe
    /// of that URL has answered 2xx (see `probe_configure_page`) — addons
    /// without a configure page get no Configure button instead of a dead
    /// browser tab. Empty otherwise.
    pub(super) fn addon_config_url(installed: &Installed) -> String {
        if installed.configure_ok == Some(true) {
            let base = installed.url.trim_end_matches('/');
            if base.starts_with("http://") || base.starts_with("https://") {
                return format!("{base}/configure");
            }
        }
        String::new()
    }

    /// Move the item at `idx` by `delta` slots, clamping at the ends.
    /// Returns the new index, or `None` when nothing moved (short list,
    /// out of range, or clamped to a standstill).
    pub(crate) fn move_item<T>(items: &mut Vec<T>, idx: usize, delta: i32) -> Option<usize> {
        let len = items.len();
        if len < 2 || idx >= len {
            return None;
        }
        let new_idx = (idx as i32 + delta).clamp(0, len as i32 - 1) as usize;
        if new_idx == idx {
            return None;
        }
        let item = items.remove(idx);
        items.insert(new_idx, item);
        Some(new_idx)
    }

    /// Mirror the installed addons (all, including disabled ones) to the
    /// Settings → Addons list.
    pub(super) fn apply_addon_rows(&self) {
        let rows: Vec<AddonRow> = {
            let state = self.shared.lock().unwrap();
            state
                .installed
                .iter()
                .map(|a| AddonRow {
                    label: SharedString::from(&a.label),
                    url: SharedString::from(&a.url),
                    enabled: a.enabled,
                    config_url: SharedString::from(Self::addon_config_url(a)),
                })
                .collect()
        };
        if let Some(app) = self.app() {
            app.set_addon_rows(Rc::new(VecModel::from(rows)).into());
        }
    }

    /// Settings → Addons: copy the addon's install URL. The row prints only the
    /// addon's name, so this is where the URL (still carried by the row model)
    /// stays reachable.
    pub(super) fn addon_copy_link(&self, url: &str) {
        if !url.is_empty() {
            copy_to_clipboard(url);
        }
    }
}

#[allow(dead_code)]
#[cfg(feature = "desktop")]
pub(crate) fn manifest_cache_dir(dir: &Path) -> PathBuf {
    dir.join("manifests")
}
#[cfg(feature = "desktop")]
#[allow(dead_code)]
pub(crate) fn manifest_cache_path(dir: &Path, url: &str) -> PathBuf {
    manifest_cache_dir(dir).join(format!("{:016x}.json", fnv1a(url.as_bytes())))
}
#[cfg(feature = "desktop")]
#[allow(dead_code)]
pub(crate) fn read_cached_manifest(dir: &Path, url: &str) -> Option<Manifest> {
    let text = fs::read_to_string(manifest_cache_path(dir, url)).ok()?;
    serde_json::from_str(&text).ok()
}
#[cfg(feature = "desktop")]
#[allow(dead_code)]
pub(crate) fn write_cached_manifest(dir: &Path, url: &str, manifest: &Manifest) {
    if fs::create_dir_all(manifest_cache_dir(dir)).is_err() {
        eprintln!("nova: could not create manifest cache dir");
        return;
    }
    let contents = match serde_json::to_string_pretty(manifest) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nova: could not serialise manifest: {e}");
            return;
        }
    };
    if let Err(e) = atomic_write(&manifest_cache_path(dir, url), &contents) {
        eprintln!("nova: could not write manifest cache: {e}");
    }
}
#[cfg(feature = "desktop")]
#[allow(dead_code)]
pub(crate) fn delete_cached_manifest(dir: &Path, url: &str) {
    let _ = fs::remove_file(manifest_cache_path(dir, url));
}
/// Persisted addons (URL + enabled flag + configure verdict + label) from the
/// KV store (empty when absent/unreadable).
pub(crate) fn read_persisted_addons() -> Vec<AddonStore> {
    read_json::<Vec<AddonStore>>("addons").unwrap_or_default()
}
/// Snapshot the installed addons into the KV store and mirror them into the
/// sync store.
pub(crate) fn write_persisted_addons(addons: &[AddonStore]) {
    write_json("addons", addons);
    if !applying() {
        notify_addons(addons);
    }
}
pub(crate) fn manifest_key(url: &str) -> String {
    format!("manifest:{url}")
}
pub(crate) fn read_cached_manifest_for(url: &str) -> Option<Manifest> {
    read_json::<Manifest>(&manifest_key(url))
}
pub(crate) fn write_cached_manifest_for(url: &str, manifest: &Manifest) {
    write_json(&manifest_key(url), manifest);
}
pub(crate) fn delete_cached_manifest_for(url: &str) {
    storage::remove(&manifest_key(url));
}
pub(crate) fn configured_addon_urls() -> Vec<String> {
    {
        let mut urls: Vec<String> = Vec::new();

        if let Ok(env) = std::env::var("NOVA_ADDONS") {
            for part in env.split(',') {
                let part = part.trim();
                if !part.is_empty() {
                    urls.push(part.to_string());
                }
            }
        }

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--addon" {
                if let Some(url) = args.next() {
                    urls.push(url);
                }
            } else if let Some(url) = arg.strip_prefix("--addon=") {
                urls.push(url.to_string());
            }
        }

        urls
    }
}

/// Compute unique display labels for `(manifest_name, url)` pairs: within a
/// group of addons sharing a manifest name, the lexicographically smallest
/// URL keeps the plain name and the rest get `"$NAME ($URL)"`. Deterministic,
/// so every device with the same set derives identical labels.
pub(crate) fn unique_labels(pairs: &[(String, String)]) -> Vec<String> {
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (name, _)) in pairs.iter().enumerate() {
        groups.entry(name.as_str()).or_default().push(i);
    }
    let mut labels: Vec<String> = pairs.iter().map(|(name, _)| name.clone()).collect();
    for indices in groups.values_mut() {
        if indices.len() < 2 {
            continue;
        }
        indices.sort_by(|&a, &b| pairs[a].1.cmp(&pairs[b].1));
        for (rank, &i) in indices.iter().enumerate() {
            if rank > 0 {
                labels[i] = format!("{} ({})", pairs[i].0, pairs[i].1);
            }
        }
    }
    labels
}

#[cfg(test)]
mod label_tests {
    use super::unique_labels;

    #[test]
    fn duplicate_names_get_url_suffix_by_smallest_url() {
        let pairs = vec![
            ("Torrentio".to_string(), "https://b.example".to_string()),
            ("Torrentio".to_string(), "https://a.example".to_string()),
            ("Cinemeta".to_string(), "https://c.example".to_string()),
        ];
        let labels = unique_labels(&pairs);
        // Smallest URL (a.example) keeps the plain name.
        assert_eq!(labels[1], "Torrentio");
        assert_eq!(labels[0], "Torrentio (https://b.example)");
        // Unique names stay plain.
        assert_eq!(labels[2], "Cinemeta");
    }

    #[test]
    fn labels_are_deterministic_across_orderings() {
        let a = vec![
            ("X".to_string(), "https://1".to_string()),
            ("X".to_string(), "https://2".to_string()),
        ];
        let b = vec![
            ("X".to_string(), "https://2".to_string()),
            ("X".to_string(), "https://1".to_string()),
        ];
        // Same URL->label mapping regardless of input order.
        let la = unique_labels(&a);
        let lb = unique_labels(&b);
        assert_eq!(la[0], lb[1]);
        assert_eq!(la[1], lb[0]);
    }
}

#[cfg(test)]
mod move_tests {
    use super::Bridge;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn move_up_down_and_clamp() {
        let mut items = v(&["a", "b", "c"]);
        assert_eq!(Bridge::move_item(&mut items, 1, -1), Some(0));
        assert_eq!(items, v(&["b", "a", "c"]));
        assert_eq!(Bridge::move_item(&mut items, 0, 5), Some(2));
        assert_eq!(items, v(&["a", "c", "b"]));
        assert_eq!(Bridge::move_item(&mut items, 2, 1), None);
        assert_eq!(items, v(&["a", "c", "b"]));
    }

    #[test]
    fn move_rejects_degenerate() {
        let mut one = v(&["a"]);
        assert_eq!(Bridge::move_item(&mut one, 0, 1), None);
        let mut empty: Vec<String> = Vec::new();
        assert_eq!(Bridge::move_item(&mut empty, 0, -1), None);
        let mut items = v(&["a", "b"]);
        assert_eq!(Bridge::move_item(&mut items, 7, -1), None);
        assert_eq!(items, v(&["a", "b"]));
    }
}
