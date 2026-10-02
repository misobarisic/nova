//! UI-thread callbacks capture stable context and enqueue work.
use super::*;
use nova_tracking::auth::{AuthReturn, ClientRegistration};
use worker::Command;
impl Bridge {
    pub(in crate::app) fn tracking_metadata(&self, id: String, type_: String, videos: &[Video]) {
        self.tracking.send(Command::Metadata {
            source: SourceRef {
                provider_id: "nova".into(),
                source_id: id,
                media_type: type_,
            },
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
    pub(in crate::app) fn tracking_show(&self) {
        let context = {
            let state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            let episodes = if modal.type_ == "movie" {
                vec![(modal.id.clone(), modal.name.clone())]
            } else {
                modal
                    .videos
                    .iter()
                    .map(|v| {
                        (
                            v.id.clone(),
                            format!("{} · {}", episode_context_label(v), v.label()),
                        )
                    })
                    .collect()
            };
            let evidence =
                read_json_result::<SourceEvidence>(&evidence_key(&modal.type_, &modal.id))
                    .ok()
                    .flatten();
            let ids = evidence.as_ref().map(|e| e.ids.clone()).unwrap_or_default();
            let aliases = evidence
                .as_ref()
                .map(|e| e.aliases.clone())
                .unwrap_or_default();
            let year = evidence.as_ref().and_then(|e| e.year);
            SourceContext {
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
            }
        };
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
        }
        self.tracking.send(Command::Show {
            context,
            generation,
        });
    }
    pub(in crate::app) fn tracking_close(&self) {
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
        let generation = self
            .tracking
            .context_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        if let Some(app) = self.app() {
            app.set_tracking_busy(true);
            app.set_tracking_can_confirm(false);
        }
        self.tracking.send(Command::Search {
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
