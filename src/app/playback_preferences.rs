//! Title overrides are independent of library membership and watch progress.
use super::*;
use nova_config::playback_preferences::{
    PreferenceValue, SessionPreferences, TitlePreferences, language_preferences,
    normalize_language, preference_key,
};

pub(super) const PREFERENCES_KEY: &str = "playback_preferences:v1";
type Preferences = HashMap<String, PreferenceValue>;

fn title_preferences(values: &Preferences, media_type: &str, id: &str) -> TitlePreferences {
    TitlePreferences {
        audio: match values.get(&preference_key(media_type, id, 0)) {
            Some(PreferenceValue::Audio(v)) => Some(v.clone()),
            _ => None,
        },
        subtitles: match values.get(&preference_key(media_type, id, 1)) {
            Some(PreferenceValue::Subtitles(v)) => Some(v.clone()),
            _ => None,
        },
        speed: match values.get(&preference_key(media_type, id, 2)) {
            Some(PreferenceValue::Speed(v)) => Some(nova_config::clamp_playback_speed(*v)),
            _ => None,
        },
    }
}

impl Bridge {
    fn playback_preferences(&self) -> Preferences {
        let mut values = read_json::<Preferences>(PREFERENCES_KEY).unwrap_or_default();
        for (key, value) in &self.shared.lock().unwrap().playback_preference_edits {
            match value {
                Some(value) => {
                    values.insert(key.clone(), value.clone());
                }
                None => {
                    values.remove(key);
                }
            }
        }
        values
    }

    pub(super) fn prepare_playback_preferences(&self) -> SessionPreferences {
        let title = self
            .playback_preference_identity(true)
            .map(|(media_type, id)| {
                title_preferences(&self.playback_preferences(), &media_type, &id)
            })
            .unwrap_or_default();
        self.preference_status_to_ui(&title);
        SessionPreferences::from_settings(&self.shared.lock().unwrap().cache_settings, title)
    }

    fn preference_status_to_ui(&self, title: &TitlePreferences) {
        if let Some(app) = self.app() {
            app.set_audio_preference_saved(title.audio.is_some());
            app.set_subtitle_preference_saved(title.subtitles.is_some());
            app.set_speed_preference_saved(title.speed.is_some());
        }
    }

    fn remember_playback_preference(
        &self,
        control: i32,
        value: Option<PreferenceValue>,
        immediate: bool,
    ) {
        let Some((media_type, id)) = self.playback_preference_identity(false) else {
            return;
        };
        {
            let mut state = self.shared.lock().unwrap();
            state
                .playback_preference_edits
                .insert(preference_key(&media_type, &id, control), value);
            state.playback_preference_save_at = Some(std::time::Instant::now());
        }
        let title = title_preferences(&self.playback_preferences(), &media_type, &id);
        self.preference_status_to_ui(&title);
        self.flush_playback_preferences(immediate);
    }

    pub(super) fn flush_playback_preferences(&self, force: bool) {
        let due = self
            .shared
            .lock()
            .unwrap()
            .playback_preference_save_at
            .is_some_and(|at| force || at.elapsed() >= std::time::Duration::from_millis(600));
        if !due || !writable_key(PREFERENCES_KEY) {
            return;
        }
        let values = self.playback_preferences();
        let failures = write_failures();
        write_json(PREFERENCES_KEY, &values);
        if failures == write_failures() {
            let mut state = self.shared.lock().unwrap();
            state.playback_preference_edits.clear();
            state.playback_preference_save_at = None;
        }
    }

    pub(super) fn pick_playback_track(&self, audio: bool, id: i32) {
        if let Some(choice) = self.player.pick_track_preference(audio, id) {
            self.remember_playback_preference(
                if audio { 0 } else { 1 },
                Some(if audio {
                    PreferenceValue::Audio(choice)
                } else {
                    PreferenceValue::Subtitles(choice)
                }),
                true,
            );
        }
    }

    pub(super) fn apply_playback_speed(&self, speed: f32) {
        let speed = nova_config::round_playback_speed(speed);
        self.player.set_title_speed(speed);
        if let Some(app) = self.app() {
            app.set_effective_playback_speed(speed);
        }
        self.remember_playback_preference(2, Some(PreferenceValue::Speed(speed)), false);
    }

    pub(super) fn restore_playback_default(&self, control: i32) {
        if !(0..=2).contains(&control) {
            return;
        }
        let settings = self.shared.lock().unwrap().cache_settings.clone();
        self.player.restore_playback_default(control, &settings);
        self.remember_playback_preference(control, None, true);
        self.refresh_effective_speed();
    }

    pub(super) fn refresh_effective_speed(&self) {
        if let Some(app) = self.app() {
            // The player mirrors its effective base rate, never the transient preview.
            app.set_effective_playback_speed(self.player.effective_playback_speed());
        }
    }

    pub(super) fn filter_playback_languages(&self, query: &str, subtitles: bool) {
        let Some(app) = self.app() else {
            return;
        };
        let current = if subtitles {
            app.get_default_subtitle_language()
        } else {
            app.get_default_audio_language()
        };
        let query = query.trim().to_lowercase();
        let mut choices = vec![(String::new(), text::tr("Automatic").to_owned())];
        if subtitles {
            choices.push(("off".into(), text::tr("Off").to_owned()));
        }
        let mut languages: Vec<_> = isolang_languages(&query);
        if let Some(code) = normalize_language(&query) {
            languages.push((code.clone(), language_label(&code)));
        }
        let selected = language_preferences(current.as_str());
        for code in &selected {
            languages.push((code.clone(), language_label(code)));
        }
        languages.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let mut seen = HashSet::new();
        languages.retain(|(code, _)| seen.insert(code.clone()));
        choices.extend(languages);
        let selected_index = choices
            .iter()
            .position(|(code, _)| code == current.as_str())
            .map_or(-1, |index| index as i32);
        let codes: Vec<slint::SharedString> = choices
            .iter()
            .map(|(code, _)| code.clone().into())
            .collect();
        let names: Vec<slint::SharedString> =
            choices.into_iter().map(|(_, name)| name.into()).collect();
        let label = match current.as_str() {
            "" => text::tr("Automatic").to_owned(),
            "off" => text::tr("Off").to_owned(),
            code => language_preferences(code)
                .iter()
                .map(|code| language_label(code))
                .collect::<Vec<_>>()
                .join(" → "),
        };
        let selected_codes = Rc::new(slint::VecModel::from(
            selected
                .iter()
                .map(|code| code.clone().into())
                .collect::<Vec<slint::SharedString>>(),
        ));
        let selected_names = Rc::new(slint::VecModel::from(
            selected
                .iter()
                .map(|code| language_label(code).into())
                .collect::<Vec<slint::SharedString>>(),
        ));
        if subtitles {
            app.set_subtitle_language_selected_codes(selected_codes.into());
            app.set_subtitle_language_selected_names(selected_names.into());
            app.set_subtitle_language_choice_index(selected_index);
            app.set_subtitle_language_codes(Rc::new(slint::VecModel::from(codes)).into());
            app.set_subtitle_language_names(Rc::new(slint::VecModel::from(names)).into());
            app.set_subtitle_language_label(label.into());
        } else {
            app.set_audio_language_selected_codes(selected_codes.into());
            app.set_audio_language_selected_names(selected_names.into());
            app.set_audio_language_choice_index(selected_index);
            app.set_audio_language_codes(Rc::new(slint::VecModel::from(codes)).into());
            app.set_audio_language_names(Rc::new(slint::VecModel::from(names)).into());
            app.set_audio_language_label(label.into());
        }
    }

    pub(super) fn pick_default_playback_language(&self, code: &str, subtitles: bool) {
        let code = if code.is_empty() || (subtitles && code == "off") {
            code.to_owned()
        } else {
            let Some(code) = normalize_language(code) else {
                return;
            };
            code
        };
        if let Some(app) = self.app() {
            if subtitles {
                app.set_default_subtitle_language(code.into());
            } else {
                app.set_default_audio_language(code.into());
            }
        }
        self.filter_playback_languages("", subtitles);
    }

    pub(super) fn edit_default_playback_language(&self, code: &str, action: i32, subtitles: bool) {
        let Some(app) = self.app() else {
            return;
        };
        let current = if subtitles {
            app.get_default_subtitle_language()
        } else {
            app.get_default_audio_language()
        };
        let Some(value) = edit_language_preferences(current.as_str(), code, action) else {
            return;
        };
        if subtitles {
            app.set_default_subtitle_language(value.into());
        } else {
            app.set_default_audio_language(value.into());
        }
        self.filter_playback_languages("", subtitles);
        if action == 0 {
            // Preserve the result being browsed so the next keyboard step
            // does not jump back to Automatic after adding a second language.
            let codes = if subtitles {
                app.get_subtitle_language_codes()
            } else {
                app.get_audio_language_codes()
            };
            let selected = normalize_language(code);
            let index = codes
                .iter()
                .position(|value| Some(value.as_str()) == selected.as_deref())
                .map_or(-1, |index| index as i32);
            if subtitles {
                app.set_subtitle_language_choice_index(index);
            } else {
                app.set_audio_language_choice_index(index);
            }
        }
    }
}

// Actions: 0 add, 1 remove, 2 raise priority, 3 lower priority.
fn edit_language_preferences(current: &str, code: &str, action: i32) -> Option<String> {
    let code = normalize_language(code)?;
    let mut selected = language_preferences(current);
    match action {
        0 => {
            if !selected.contains(&code) {
                selected.push(code);
            }
        }
        1 => selected.retain(|value| value != &code),
        2 | 3 => {
            let index = selected.iter().position(|value| value == &code)?;
            let next = if action == 2 {
                index.checked_sub(1)?
            } else {
                index + 1
            };
            if next >= selected.len() {
                return None;
            }
            selected.swap(index, next);
        }
        _ => return None,
    }
    Some(selected.join(","))
}

fn isolang_languages(query: &str) -> Vec<(String, String)> {
    // Keep the initial list familiar and short; searching also finds languages
    // without a two-letter code. The catalogue is owned by nova-config.
    nova_config::playback_preferences::language_choices(query)
}

fn language_label(code: &str) -> String {
    nova_config::playback_preferences::language_label(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn language_list_edits_preserve_priority_and_exclusive_modes() {
        assert_eq!(
            edit_language_preferences("en", "JPN", 0).as_deref(),
            Some("en,ja")
        );
        assert_eq!(
            edit_language_preferences("en,ja", "eng", 0).as_deref(),
            Some("en,ja")
        );
        assert_eq!(
            edit_language_preferences("en,ja", "ja", 2).as_deref(),
            Some("ja,en")
        );
        assert_eq!(
            edit_language_preferences("ja,en", "ja", 3).as_deref(),
            Some("en,ja")
        );
        assert_eq!(
            edit_language_preferences("en,ja", "en", 1).as_deref(),
            Some("ja")
        );
        assert_eq!(
            edit_language_preferences("en", "en", 1).as_deref(),
            Some("")
        );
        assert_eq!(
            edit_language_preferences("off", "hr", 0).as_deref(),
            Some("hr")
        );
        assert_eq!(edit_language_preferences("en", "en", 2), None);
        assert_eq!(edit_language_preferences("en", "en", 3), None);
        assert_eq!(edit_language_preferences("en", "invalid", 0), None);
    }
    #[test]
    fn title_fields_and_typed_identity_are_independent() {
        let mut values = Preferences::new();
        values.insert(
            preference_key("series", "same", 2),
            PreferenceValue::Speed(1.3),
        );
        values.insert(
            preference_key("series", "same", 1),
            PreferenceValue::Subtitles(nova_config::playback_preferences::TrackChoice::Off),
        );
        let title = title_preferences(&values, "series", "same");
        assert!(title.audio.is_none());
        assert!(title.subtitles.is_some());
        assert_eq!(title.speed, Some(1.3));
        assert!(title_preferences(&values, "movie", "same").speed.is_none());
        values.remove(&preference_key("series", "same", 1));
        assert!(
            title_preferences(&values, "series", "same")
                .subtitles
                .is_none()
        );
        assert_eq!(
            title_preferences(&values, "series", "same").speed,
            Some(1.3)
        );
    }
}
