//! UI-thread callbacks capture stable context and enqueue work.
use super::*;
use nova_tracking::auth::{AuthReturn, ClientRegistration};
use worker::Command;
impl Bridge {
    pub(in crate::app) fn tracking_metadata(&self, id: String, type_: String, videos: &[Video]) {
        let source = SourceRef {
            provider_id: "nova".into(),
            source_id: id,
            media_type: type_,
        };
        let info = tracking_episode_info(videos);
        let generation = self.tracking.metadata_generation(&source, &info);
        self.tracking.send(Command::Metadata {
            source,
            generation,
            episode_ids: videos
                .iter()
                .filter(|v| v.season.is_some())
                .map(|v| v.id.clone())
                .collect(),
        });
    }
    pub(in crate::app) fn tracking_automatic(&self, index: i32, enabled: bool) {
        if let Some(service) = service(index) {
            self.tracking.send(Command::Automatic { service, enabled });
        }
    }
    pub(in crate::app) fn tracking_reset(&self) {
        for epoch in &self.tracking.login_generation {
            epoch.fetch_add(1, Ordering::AcqRel);
        }
        self.tracking.send(Command::Reset);
    }
    pub(in crate::app) fn tracking_connect(&self, index: i32, client: String, redirect: String) {
        let Some(service) = service(index) else {
            return;
        };
        let epoch = self.tracking.login_generation[service_index(service)]
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        self.tracking.send(Command::Connect {
            service,
            registration: ClientRegistration {
                client_id: client.trim().into(),
                redirect_uri: redirect.trim().into(),
            },
            epoch,
        });
    }
    pub(in crate::app) fn tracking_complete(&self, index: i32, value: String) {
        let Some(service) = service(index) else {
            return;
        };
        if let Ok(value) = AuthReturn::new(value) {
            let epoch =
                self.tracking.login_generation[service_index(service)].load(Ordering::Acquire);
            self.tracking.send(Command::Complete {
                service,
                value,
                epoch,
            });
        }
    }
    pub(in crate::app) fn tracking_cancel(&self, index: i32) {
        let Some(service) = service(index) else {
            return;
        };
        let epoch = self.tracking.login_generation[service_index(service)]
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        self.tracking.send(Command::Cancel { service, epoch });
    }
    pub(in crate::app) fn tracking_disconnect(&self, index: i32) {
        let Some(service) = service(index) else {
            return;
        };
        let epoch = self.tracking.login_generation[service_index(service)]
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        self.tracking.send(Command::Disconnect { service, epoch });
    }
    fn tracking_context(&self) -> Option<SourceContext> {
        let state = self.shared.lock().unwrap();
        let modal = state.modal_item.as_ref()?;
        let episodes = if modal.type_ == "movie" {
            vec![(modal.id.clone(), modal.name.clone())]
        } else {
            modal
                .videos
                .iter()
                .map(|v| {
                    (
                        v.id.clone(),
                        format!("{} · {}", episode_se_label(v), v.label()),
                    )
                })
                .collect()
        };
        let evidence = read_json_result::<SourceEvidence>(&evidence_key(&modal.type_, &modal.id))
            .ok()
            .flatten();
        let ids = evidence.as_ref().map(|e| e.ids.clone()).unwrap_or_default();
        let aliases = evidence
            .as_ref()
            .map(|e| e.aliases.clone())
            .unwrap_or_default();
        let year = evidence.as_ref().and_then(|e| e.year);
        let episode_info = if modal.type_ == "movie" {
            vec![nova_tracking::EpisodeInfo {
                id: modal.id.clone(),
                season: Some(1),
                number: Some(1),
                title: modal.name.clone(),
                released: nova_tracking::ListDate::default(),
            }]
        } else {
            tracking_episode_info(&modal.videos)
        };
        Some(SourceContext {
            source: SourceRef {
                provider_id: "nova".into(),
                source_id: modal.id.clone(),
                media_type: modal.type_.clone(),
            },
            title: modal.name.clone(),
            ids,
            aliases,
            year,
            episodes,
            episode_info,
        })
    }
    pub(in crate::app) fn tracking_show(&self) {
        let Some(context) = self.tracking_context() else {
            return;
        };
        *self.tracking.context_evidence.lock().unwrap() =
            Some((context.source.clone(), context.episode_info.clone()));
        let generation = self
            .tracking
            .context_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        if let Some(app) = self.app() {
            app.set_tracking_open(true);
            app.set_tracking_candidate_title("".into());
            app.set_tracking_candidates(
                Rc::new(VecModel::<crate::TrackingCandidateRow>::default()).into(),
            );
            app.set_tracking_notice(text::tr("Loading tracking…").into());
            app.set_tracking_busy(true);
            app.set_tracking_can_confirm(false);
        }
        self.tracking.send(Command::Show {
            context,
            generation,
        });
    }
    pub(in crate::app) fn tracking_close(&self) {
        *self.tracking.context_evidence.lock().unwrap() = None;
        self.tracking
            .context_generation
            .fetch_add(1, Ordering::AcqRel);
        if let Some(app) = self.app() {
            app.set_tracking_open(false);
        }
        self.tracking.send(Command::Close);
    }
    pub(in crate::app) fn tracking_search(&self, index: i32, query: String) {
        let Some(service) = service(index) else {
            return;
        };
        let Some(context) = self.tracking_context() else {
            return;
        };
        *self.tracking.context_evidence.lock().unwrap() =
            Some((context.source.clone(), context.episode_info.clone()));
        let generation = self
            .tracking
            .context_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        if let Some(app) = self.app() {
            app.set_tracking_busy(true);
            app.set_tracking_service(index);
            app.set_tracking_can_confirm(false);
            app.set_tracking_candidates(
                Rc::new(VecModel::<crate::TrackingCandidateRow>::default()).into(),
            );
        }
        self.tracking.send(Command::Search {
            context,
            service,
            query,
            generation,
        });
    }
    pub(in crate::app) fn tracking_pick(&self, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        self.tracking.send(Command::Pick {
            index,
            generation: self.tracking.context_generation.load(Ordering::Acquire),
        });
    }
    pub(in crate::app) fn tracking_preview(&self, first: String, last: String, start: String) {
        self.tracking.send(Command::Preview {
            first,
            last,
            start,
            generation: self.tracking.context_generation.load(Ordering::Acquire),
        });
    }
    pub(in crate::app) fn tracking_assign(&self, row: i32, value: String) {
        if let Ok(row) = usize::try_from(row) {
            if let Some(app) = self.app() {
                app.set_tracking_can_confirm(false);
            }
            self.tracking.send(Command::Assign {
                row,
                value,
                generation: self.tracking.context_generation.load(Ordering::Acquire),
            });
        }
    }
    pub(in crate::app) fn tracking_confirm(&self) {
        self.tracking.send(Command::Confirm {
            generation: self.tracking.context_generation.load(Ordering::Acquire),
        });
    }
    pub(in crate::app) fn tracking_cancel_adjust(&self) {
        self.tracking.send(Command::CancelAdjust {
            generation: self.tracking.context_generation.load(Ordering::Acquire),
        });
    }
    pub(in crate::app) fn tracking_accept_setup(&self, revision: String, include_history: bool) {
        if let Some(app) = self.app() {
            app.set_tracking_busy(true);
        }
        self.tracking.send(Command::AcceptSetup {
            revision,
            include_history,
            generation: self.tracking.context_generation.load(Ordering::Acquire),
        });
    }
    pub(in crate::app) fn tracking_adjust_setup(&self, index: i32) {
        if let Ok(index) = usize::try_from(index) {
            self.tracking.send(Command::AdjustSetup {
                index,
                generation: self.tracking.context_generation.load(Ordering::Acquire),
            });
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn tracking_edit(
        &self,
        id: String,
        progress: String,
        status: i32,
        score: String,
        set_started: bool,
        started: String,
        set_completed: bool,
        completed: String,
    ) {
        self.tracking.send(Command::Edit {
            id,
            progress,
            status,
            score,
            started: set_started.then_some(started),
            completed: set_completed.then_some(completed),
        });
    }
    pub(in crate::app) fn tracking_action(&self, id: String, action: i32) {
        self.tracking.send(Command::Action { id, action });
    }
}
