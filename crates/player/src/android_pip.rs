//! Android PiP keeps the existing mpv/Slint GL surface; only the scene is hidden.

#[cfg(target_os = "android")]
use super::*;

/// Display dimensions come from mpv (including rotation and pixel aspect ratio).
/// Reject audio, album art and not-yet-loaded sources rather than guessing 16:9.
pub(super) fn video_dimensions(width: i64, height: i64, image: bool) -> Option<(i32, i32)> {
    if image || width <= 0 || height <= 0 {
        return None;
    }
    Some((i32::try_from(width).ok()?, i32::try_from(height).ok()?))
}

#[cfg(target_os = "android")]
pub(super) fn publish(state: &State, app: &AppWindow, eof: bool) {
    let dimensions = state.mpv.lock().ok().and_then(|mpv| {
        let mpv = mpv.as_ref()?;
        // A selected cover image is not video playback.
        let id = mpv.get_property::<i64>("vid").ok()?;
        let tracks = mpv.get_property::<i64>("track-list/count").ok()?;
        let video = (0..tracks).find(|i| {
            mpv.get_property::<String>(&format!("track-list/{i}/type"))
                .is_ok_and(|kind| kind == "video")
                && mpv.get_property::<i64>(&format!("track-list/{i}/id")).ok() == Some(id)
        })?;
        let image = mpv
            .get_property::<bool>(&format!("track-list/{video}/image"))
            .unwrap_or(false)
            || mpv
                .get_property::<bool>(&format!("track-list/{video}/albumart"))
                .unwrap_or(false);
        video_dimensions(
            mpv.get_property("dwidth").unwrap_or(0),
            mpv.get_property("dheight").unwrap_or(0),
            image,
        )
    });
    let allowed = nova_config::active_cache_settings().android_pip_enabled
        && app.get_player_open()
        && app.get_playback_started()
        && !state.source_waiting.load(Ordering::SeqCst)
        && !state.reported.load(Ordering::SeqCst)
        && !eof
        && !app.get_onboarding_visible()
        && !app.get_resume_prompt_visible()
        && !app.get_completion_prompt_visible()
        && !app.get_party_page_open()
        && !app.get_remote_controls_open()
        && !app.get_remote_devices_open()
        && app.get_remote_replacement_token().is_empty();
    let (width, height) = dimensions.unwrap_or((16, 9));
    external::update_picture_in_picture(
        allowed && dimensions.is_some(),
        app.get_is_paused(),
        width,
        height,
    );
}

/// Called by the root activity bridge after marshalling to Slint's event loop.
#[cfg(target_os = "android")]
pub fn note_android_pip_event(event: i32) {
    let Some(state) = ANDROID_PLAYER
        .lock()
        .unwrap()
        .as_ref()
        .and_then(std::sync::Weak::upgrade)
    else {
        return;
    };
    let Some(app) = state.app.upgrade() else {
        return;
    };
    match event {
        0 | 1 => {
            app.set_android_pip_active(event == 1);
            if event == 0 && app.get_player_open() {
                app.invoke_osd_mouse_moved();
            }
            app.window().request_redraw();
        }
        2 | 3 if app.get_player_open() => {
            Player { state }.set_paused(event == 2);
        }
        4 => note_android_pause(),
        5 => {
            // A retained PiP window becomes visible on Start/unlock, without
            // an Activity Resume. Reuse the same auto-continue/party policy
            // and release recovery even when the surface is still rebuilding.
            app.set_android_pip_active(true);
            let settings = nova_config::active_cache_settings();
            resume_android_playback(settings.android_auto_continue && settings.android_pip_enabled);
            publish(&state, &app, false);
            app.window().request_redraw();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_loaded_video_has_pip_dimensions() {
        assert_eq!(video_dimensions(1920, 1080, false), Some((1920, 1080)));
        assert_eq!(video_dimensions(1080, 1920, false), Some((1080, 1920)));
        assert_eq!(video_dimensions(0, 0, false), None);
        assert_eq!(video_dimensions(-1, 1080, false), None);
        assert_eq!(video_dimensions(1920, 0, false), None);
        assert_eq!(video_dimensions(400, 400, true), None);
        assert_eq!(video_dimensions(i64::MAX, 1080, false), None);
    }
}
