use super::*;
use nova_ui::backend_text as text;
use nova_ui_shared::{BackupMapping, BackupOption};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    archive: Option<Archive>,
    choices: Vec<Choice>,
    preview: Option<Preview>,
    selected_profiles: BTreeSet<String>,
    selected_components: BTreeSet<String>,
}
fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    std::rc::Rc::new(VecModel::from(rows)).into()
}
fn label(name: &str) -> String {
    text::tr(
        adapters::COMPONENTS
            .iter()
            .find(|(id, _)| *id == name)
            .map(|(_, label)| *label)
            .unwrap_or(name),
    )
    .to_string()
}
fn status(app: &crate::AppWindow, message: &str) {
    app.global::<crate::BackupBridge>()
        .set_status(text::tr(message).into());
}
fn changed(app: &crate::AppWindow, state: &mut State) {
    state.preview = None;
    let ui = app.global::<crate::BackupBridge>();
    ui.set_preview_ready(false);
    ui.set_preview_rows(model(Vec::<slint::SharedString>::new()));
}
fn publish_options(app: &crate::AppWindow, state: &State) {
    let ui = app.global::<crate::BackupBridge>();
    if let Some(archive) = &state.archive {
        ui.set_profiles(model(Vec::<BackupOption>::new()));
        ui.set_mappings(model(
            state
                .choices
                .iter()
                .map(|choice| BackupMapping {
                    id: choice.source.clone().into(),
                    label: choice.name.clone().into(),
                    target: if choice.create {
                        text::tr("Create new profile").to_string()
                    } else {
                        profiles::list()
                            .unwrap_or_default()
                            .into_iter()
                            .find(|profile| profile.id == choice.target)
                            .map(|profile| profile.name)
                            .unwrap_or_else(|| choice.target.clone())
                    }
                    .into(),
                })
                .collect(),
        ));
        ui.set_components(model(
            state
                .choices
                .iter()
                .flat_map(|choice| {
                    archive
                        .manifest
                        .profiles
                        .iter()
                        .find(|profile| profile.archive_profile_id == choice.source)
                        .into_iter()
                        .flat_map(move |profile| {
                            profile
                                .components
                                .iter()
                                .filter(move |component| {
                                    archive.payloads.contains_key(&(
                                        choice.source.clone(),
                                        component.name.clone(),
                                    ))
                                })
                                .map(move |component| BackupOption {
                                    id: format!("{}\0{}", choice.source, component.name).into(),
                                    label: format!(
                                        "{} / {} ({})",
                                        choice.name,
                                        label(&component.name),
                                        component.item_count
                                    )
                                    .into(),
                                    checked: choice.components.contains(&component.name),
                                })
                        })
                })
                .collect(),
        ));
        ui.set_conflicts(model(
            state
                .choices
                .iter()
                .flat_map(|choice| {
                    choice.components.iter().map(move |name| BackupOption {
                        id: format!("{}\0{name}", choice.source).into(),
                        label: format!("{} / {}", choice.name, label(name)).into(),
                        checked: choice.use_backup.contains(name),
                    })
                })
                .collect(),
        ));
    } else {
        ui.set_profiles(model(
            profiles::list()
                .unwrap_or_default()
                .into_iter()
                .map(|profile| BackupOption {
                    id: profile.id.clone().into(),
                    label: profile.name.into(),
                    checked: state.selected_profiles.contains(&profile.id),
                })
                .collect(),
        ));
        ui.set_mappings(model(Vec::<BackupMapping>::new()));
        ui.set_components(model(
            adapters::COMPONENTS
                .iter()
                .map(|(name, _)| BackupOption {
                    id: (*name).into(),
                    label: label(name).into(),
                    checked: state.selected_components.contains(*name),
                })
                .collect(),
        ));
        ui.set_conflicts(model(Vec::<BackupOption>::new()));
    }
    ui.set_importing(state.archive.is_some());
}
fn allowed() -> bool {
    app::bridge::with_global_bridge(|bridge| bridge.require_profile_management()).unwrap_or(false)
}
fn targets_allowed(mut ids: impl Iterator<Item = String>) -> bool {
    ids.all(|id| {
        app::bridge::with_global_bridge(|bridge| bridge.require_target_profile_management(&id))
            .unwrap_or(false)
    })
}

pub(super) fn install(app: &crate::AppWindow) {
    let state = Arc::new(Mutex::new(State::default()));
    let ui = app.global::<crate::BackupBridge>();
    let weak = app.as_weak();
    let state_open = state.clone();
    ui.on_opened(move || {
        if !app::bridge::with_global_bridge(|bridge| bridge.request_backup_management())
            .unwrap_or(false)
        {
            return;
        }
        let Some(app) = weak.upgrade() else { return };
        let mut state = state_open.lock().unwrap();
        state.archive = None;
        state.choices.clear();
        state.preview = None;
        state.selected_profiles = BTreeSet::from([profiles::current_id()]);
        state.selected_components = adapters::COMPONENTS
            .iter()
            .filter(|(name, _)| *name != "device" && *name != "packages")
            .map(|(name, _)| (*name).into())
            .collect();
        let ui = app.global::<crate::BackupBridge>();
        ui.set_open(true);
        ui.set_busy(false);
        ui.set_replace(false);
        ui.set_preview_ready(false);
        ui.set_preview_rows(model(Vec::<slint::SharedString>::new()));
        status(&app, "");
        publish_options(&app, &state);
    });
    let weak = app.as_weak();
    ui.on_closed(move || {
        if let Some(app) = weak.upgrade() {
            let ui = app.global::<crate::BackupBridge>();
            if !ui.get_busy() {
                ui.set_open(false);
                app::bridge::with_global_bridge(|bridge| bridge.close_profile_management());
                app.recover_back_focus();
            }
        }
    });
    let weak = app.as_weak();
    let state_preset = state.clone();
    ui.on_preset(move |preset| {
        if let Some(app) = weak.upgrade() {
            let selected = |name: &str| match preset {
                1 => matches!(
                    name,
                    "library"
                        | "categories"
                        | "progress"
                        | "journal"
                        | "activity"
                        | "continue_hidden"
                        | "history_hidden"
                        | "episodes_hidden"
                        | "collections"
                        | "bookmarks"
                        | "preferences"
                        | "queue"
                ),
                2 => matches!(name, "settings" | "addons" | "integrations" | "policy"),
                _ => name != "device" && name != "packages",
            };
            let mut state = state_preset.lock().unwrap();
            state.selected_components = adapters::COMPONENTS
                .iter()
                .filter(|(name, _)| selected(name))
                .map(|(name, _)| (*name).into())
                .collect();
            let mut selections = state
                .choices
                .iter()
                .map(|choice| {
                    (
                        choice.source.clone(),
                        components_for_preset(preset, state.archive.as_ref(), &choice.source),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            for choice in &mut state.choices {
                choice.components = selections.remove(&choice.source).unwrap_or_default();
            }
            changed(&app, &mut state);
            publish_options(&app, &state);
        }
    });
    let weak = app.as_weak();
    let state_profiles = state.clone();
    ui.on_toggle_profile(move |id| {
        if let Some(app) = weak.upgrade() {
            let mut state = state_profiles.lock().unwrap();
            if !state.selected_profiles.contains(id.as_str())
                && !app::bridge::with_global_bridge(|bridge| {
                    bridge.require_target_profile_management(id.as_str())
                })
                .unwrap_or(false)
            {
                return;
            }
            if !state.selected_profiles.remove(id.as_str()) {
                state.selected_profiles.insert(id.into());
            }
            changed(&app, &mut state);
            publish_options(&app, &state);
        }
    });
    let weak = app.as_weak();
    let state_components = state.clone();
    ui.on_toggle_component(move |id| {
        if let Some(app) = weak.upgrade() {
            let mut state = state_components.lock().unwrap();
            if let Some((source, name)) = id.split_once('\0') {
                if let Some(choice) = state
                    .choices
                    .iter_mut()
                    .find(|choice| choice.source.as_str() == source)
                    && !choice.components.remove(name)
                {
                    choice.components.insert(name.into());
                }
            } else if !state.selected_components.remove(id.as_str()) {
                state.selected_components.insert(id.into());
            }
            changed(&app, &mut state);
            publish_options(&app, &state);
        }
    });
    let weak = app.as_weak();
    let state_conflicts = state.clone();
    ui.on_toggle_conflict(move |id| {
        if let Some(app) = weak.upgrade() {
            let mut state = state_conflicts.lock().unwrap();
            if let Some((source, name)) = id.split_once('\0')
                && let Some(choice) = state
                    .choices
                    .iter_mut()
                    .find(|choice| choice.source.as_str() == source)
                && !choice.use_backup.remove(name)
            {
                choice.use_backup.insert(name.into());
            }
            changed(&app, &mut state);
            publish_options(&app, &state);
        }
    });
    let weak = app.as_weak();
    let state_mapping = state.clone();
    ui.on_cycle_target(move |source| {
        if let Some(app) = weak.upgrade() {
            let mut state = state_mapping.lock().unwrap();
            if let Some(choice) = state
                .choices
                .iter_mut()
                .find(|choice| choice.source.as_str() == source.as_str())
            {
                let live = profiles::list().unwrap_or_default();
                let next = if choice.create {
                    live.first()
                } else {
                    live.iter()
                        .position(|profile| profile.id == choice.target)
                        .and_then(|index| live.get(index + 1))
                };
                if let Some(target) = next {
                    if !app::bridge::with_global_bridge(|bridge| {
                        bridge.require_target_profile_management(&target.id)
                    })
                    .unwrap_or(false)
                    {
                        return;
                    }
                    choice.target = target.id.clone();
                    choice.create = false;
                } else if let Some(archive) = state.archive.clone()
                    && let Ok(fresh) = super::choices(&archive)
                    && let Some(new) = fresh
                        .into_iter()
                        .find(|new| new.source.as_str() == source.as_str())
                {
                    let choice = state
                        .choices
                        .iter_mut()
                        .find(|choice| choice.source.as_str() == source.as_str())
                        .unwrap();
                    choice.target = new.target;
                    choice.create = true;
                }
            }
            changed(&app, &mut state);
            publish_options(&app, &state);
        }
    });
    let weak = app.as_weak();
    let state_mode = state.clone();
    ui.on_mode_changed(move |replace| {
        if let Some(app) = weak.upgrade() {
            app.global::<crate::BackupBridge>().set_replace(replace);
            let mut state = state_mode.lock().unwrap();
            for choice in &mut state.choices {
                choice.replace = replace;
            }
            changed(&app, &mut state);
            publish_options(&app, &state);
        }
    });

    let weak = app.as_weak();
    let state_export = state.clone();
    ui.on_export_file(move || {
        if !allowed() {
            return;
        }
        let Some(app) = weak.upgrade() else { return };
        let selected = {
            let state = state_export.lock().unwrap();
            state
                .selected_profiles
                .iter()
                .map(|id| (id.clone(), state.selected_components.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        if selected.is_empty() || selected.values().all(BTreeSet::is_empty) {
            status(&app, "Choose at least one profile and component");
            return;
        }
        app.global::<crate::BackupBridge>().set_busy(true);
        status(&app, "Preparing backup…");
        let weak = app.as_weak();
        profiles::thread::spawn(move || match super::export(&selected) {
            Ok(bytes) => {
                app::documents::save(
                    &format!("Nova-{}.nova-backup", nova_config::now_secs()),
                    "application/zip",
                    bytes,
                    move |result| {
                        let _ = profiles::invoke_from_event_loop(move || {
                            if let Some(app) = weak.upgrade() {
                                app.global::<crate::BackupBridge>().set_busy(false);
                                match result {
                                    Ok(true) => status(&app, "Backup exported"),
                                    Ok(false) => status(&app, "Export cancelled"),
                                    Err(error) => status(&app, &error),
                                }
                            }
                        });
                    },
                );
            }
            Err(error) => {
                let _ = profiles::invoke_from_event_loop(move || {
                    if let Some(app) = weak.upgrade() {
                        app.global::<crate::BackupBridge>().set_busy(false);
                        status(&app, &error);
                    }
                });
            }
        });
    });
    let weak = app.as_weak();
    let state_import = state.clone();
    ui.on_import_file(move || {
        if !allowed() {return}
        let Some(app)=weak.upgrade() else {return};app.global::<crate::BackupBridge>().set_busy(true);status(&app,"Choose a backup file…");
        let weak=app.as_weak();let state=state_import.clone();let context=profiles::capture();
        app::documents::open("nova-backup",move |result| {
            let result=result.and_then(|bytes|bytes.map(|bytes|archive::decode(&bytes).and_then(|archive|super::choices(&archive).map(|choices|(archive,choices)))).transpose());
            profiles::with_context(context,|| {let _=profiles::invoke_from_event_loop(move ||if let Some(app)=weak.upgrade() {
                app.global::<crate::BackupBridge>().set_busy(false);
                match result {Ok(Some((archive,choices)))=>{let unsupported=archive.unsupported.join(", ");let mut state=state.lock().unwrap();state.archive=Some(archive);state.choices=choices;changed(&app,&mut state);publish_options(&app,&state);if unsupported.is_empty() {status(&app,"Choose target profiles and components, then rebuild the preview");} else {status(&app,&format!("{}: {unsupported}",text::tr("Unsupported optional components")));}},Ok(None)=>status(&app,"Import cancelled"),Err(error)=>status(&app,&error)}
            });});
        });
    });
    let weak = app.as_weak();
    let state_preview = state.clone();
    ui.on_build_preview(move || {
        if !allowed() {
            return;
        }
        let Some(app) = weak.upgrade() else { return };
        let input = {
            let state = state_preview.lock().unwrap();
            state
                .archive
                .clone()
                .map(|archive| (archive, state.choices.clone()))
        };
        let Some((archive, choices)) = input else {
            return;
        };
        if !targets_allowed(
            choices
                .iter()
                .filter(|choice| !choice.create && !choice.components.is_empty())
                .map(|choice| choice.target.clone()),
        ) {
            return;
        }
        app.global::<crate::BackupBridge>().set_busy(true);
        status(&app, "Preparing import preview…");
        let weak = app.as_weak();
        let state = state_preview.clone();
        profiles::thread::spawn(move || {
            let result = super::preview(archive, choices);
            let _ = profiles::invoke_from_event_loop(move || {
                if let Some(app) = weak.upgrade() {
                    show_preview(&app, &state, result);
                }
            });
        });
    });
    let weak = app.as_weak();
    let state_apply = state.clone();
    ui.on_apply_import(move || {
        if !allowed() {
            return;
        }
        let Some(app) = weak.upgrade() else { return };
        let Some(preview) = state_apply.lock().unwrap().preview.clone() else {
            return;
        };
        let targets = preview
            .choices
            .iter()
            .map(|choice| choice.target.clone())
            .collect::<Vec<_>>();
        if !targets_allowed(targets.iter().cloned()) {
            return;
        }
        let prepared =
            app::bridge::with_global_bridge(|bridge| bridge.profile_import_prepare(&targets))
                .unwrap_or(Err("Application is unavailable".into()));
        if let Err(error) = prepared {
            status(&app, &error);
            return;
        }
        app.global::<crate::BackupBridge>().set_busy(true);
        status(&app, "Importing selected data…");
        let weak = app.as_weak();
        let state = state_apply.clone();
        // The transaction's completion is account-owned. Quiescing an active
        // profile deliberately invalidates its old activation callbacks.
        std::thread::spawn(move || {
            let result = super::apply(&preview);
            let _ = profiles::invoke_account(move || {
                if let Some(app) = weak.upgrade() {
                    app.global::<crate::BackupBridge>().set_busy(false);
                    let mut state = state.lock().unwrap();
                    changed(&app, &mut state);
                    match result {
                        Ok(domains) => {
                            activate(domains);
                            app::bridge::with_global_bridge(|bridge| {
                                bridge.profile_import_committed(&targets)
                            });
                            status(
                                &app,
                                "Import complete. Recovery copy saved; Undo is available.",
                            );
                        }
                        Err(error) => {
                            app::bridge::with_global_bridge(|bridge| {
                                bridge.profile_import_committed(&targets)
                            });
                            status(&app, &error);
                        }
                    }
                }
            });
        });
    });
    let weak = app.as_weak();
    let state_undo = state.clone();
    ui.on_undo(move || {
        if !allowed() {
            return;
        }
        let Some(app) = weak.upgrade() else { return };
        let ids = match super::undo_targets() {
            Ok(ids) => ids,
            Err(error) => {
                status(&app, &error);
                return;
            }
        };
        if !targets_allowed(ids.into_iter()) {
            return;
        }
        app.global::<crate::BackupBridge>().set_busy(true);
        let weak = app.as_weak();
        let state = state_undo.clone();
        profiles::thread::spawn(move || {
            let result = super::undo_preview();
            let _ = profiles::invoke_from_event_loop(move || {
                if let Some(app) = weak.upgrade() {
                    if let Ok(preview) = &result {
                        let mut state = state.lock().unwrap();
                        state.archive = Some(preview.archive.clone());
                        state.choices = preview.choices.clone();
                        publish_options(&app, &state);
                        app.global::<crate::BackupBridge>().set_replace(true);
                    }
                    show_preview(&app, &state, result);
                }
            });
        });
    });
    let weak = app.as_weak();
    ui.on_delete_recovery(move || {
        if !allowed() {
            return;
        }
        let Some(app) = weak.upgrade() else { return };
        let result = nova_storage::try_scan_prefix(RECOVERY_PREFIX).and_then(|rows| {
            nova_storage::try_write_batch(
                &rows
                    .into_iter()
                    .map(|(key, _)| (key, None))
                    .collect::<Vec<_>>(),
            )
        });
        match result {
            Ok(()) => status(&app, "Recovery copies deleted"),
            Err(error) => status(&app, &error.to_string()),
        }
    });
    // A committed receipt survives a crash between storage and UI publication.
    if let Ok(Some(raw)) = nova_storage::try_get_str(ACTIVATION_KEY)
        && let Ok(domains) = serde_json::from_str(&raw)
    {
        // The global Bridge is published later during run(). Replay only
        // after startup finishes installing all profile-owned services.
        let _ = profiles::invoke_account(move || activate(domains));
    }
}

fn state_components(preset: i32) -> BTreeSet<String> {
    adapters::COMPONENTS
        .iter()
        .filter(|(name, _)| match preset {
            1 => matches!(
                *name,
                "library"
                    | "categories"
                    | "progress"
                    | "journal"
                    | "activity"
                    | "continue_hidden"
                    | "history_hidden"
                    | "episodes_hidden"
                    | "collections"
                    | "bookmarks"
                    | "preferences"
                    | "queue"
            ),
            2 => matches!(*name, "settings" | "addons" | "integrations" | "policy"),
            _ => *name != "device" && *name != "packages",
        })
        .map(|(name, _)| (*name).into())
        .collect()
}
pub(super) fn components_for_preset(
    preset: i32,
    archive: Option<&Archive>,
    source: &str,
) -> BTreeSet<String> {
    state_components(preset)
        .into_iter()
        .filter(|name| {
            archive.is_none_or(|archive| {
                archive
                    .payloads
                    .contains_key(&(source.into(), name.clone()))
            })
        })
        .collect()
}
fn show_preview(
    app: &crate::AppWindow,
    state: &Arc<Mutex<State>>,
    result: Result<Preview, String>,
) {
    let ui = app.global::<crate::BackupBridge>();
    ui.set_busy(false);
    match result {
        Ok(preview) => {
            ui.set_preview_rows(model(preview.rows.iter().map(|row| {
                let c=&row.counts;
                format!("{} / {}\n{}: {} · {}: {} · {}: {} · {}: {} · {}: {} · {}: {} · {}: {}{}{}",row.profile,label(&row.component),text::tr("Archive"),c.archived,text::tr("New"),c.added,text::tr("Identical"),c.identical,text::tr("Conflicts"),c.conflicts,text::tr("Updates"),c.updated,text::tr("Preserved local"),c.preserved,text::tr("Deletions"),c.deleted,if c.examples.is_empty() {String::new()} else {format!("\n{}: {}",text::tr("Conflict identities"),c.examples.join(", "))},if row.notes.is_empty() {String::new()} else {format!("\n{}",row.notes.join("\n"))}).into()
            }).collect()));
            state.lock().unwrap().preview = Some(preview);
            ui.set_preview_ready(true);
            status(app, "Review the exact changes, then import");
        }
        Err(error) => {
            ui.set_preview_ready(false);
            status(app, &error);
        }
    }
}
fn activate(domains: Vec<String>) {
    for domain in &domains {
        if let Some((id, base)) = nova_sync::parse_profile_domain(domain)
            && base == app::last_stream::FAMILY_DOMAIN
        {
            app::bridge::with_global_bridge(|bridge| bridge.last_stream.invalidate_owner(id));
        }
        if let Some((id, base)) = nova_sync::parse_profile_domain(domain)
            && base == app::release_reminders::DOMAIN
        {
            app::release_reminders::cancel_profile_notifications(id);
        }
    }
    let failures = app::io::write_failures();
    let published = app::bridge::with_global_bridge(|bridge| bridge.sync_apply(domains)).is_some();
    app::bridge::with_global_bridge(|bridge| bridge.reconcile_release_reminders());
    if !published || app::io::write_failures() != failures {
        return;
    }
    if let Err(error) = nova_storage::try_write_batch(&[(ACTIVATION_KEY.into(), None)]) {
        nova_storage::report(error);
    }
}
