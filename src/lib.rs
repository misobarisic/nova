// The generated Slint components live in the `nova-ui` leaf crate; re-export
// them at the crate root so `crate::AppWindow` / `crate::StreamRow` keep
// working unchanged across the app (and for downstream test crates).
pub use nova_ui::*;

// Persistent key-value storage (redb), its own leaf crate; re-exported so
// `crate::storage::…` keeps working.
pub use nova_storage as storage;

// Shared settings types + app paths (read by the media/player/torrent crates).
pub use nova_config;
// `web_log` is a no-op on native; re-exported so `crate::web_log` call sites
// stay unchanged.
pub use nova_config::web_log;

// Platform transport (HTTP + image cache), now its own crate; re-exported so
// `crate::net::…` keeps working.
pub use nova_media::net;

// In-window mpv player (desktop + Android), its own crate; re-exported so
// `crate::player::…` keeps working.
pub use nova_player as player;

// Durable stream download subsystem, its own leaf crate.
pub use nova_download as download;

// Normalized provider API and the bundled JavaScript source runtime.
pub use nova_providers as providers;

// Embedded BitTorrent streaming (librqbit), its own crate; re-exported so
// `crate::torrent::…` keeps working unchanged.
pub use nova_torrent as torrent;

// The full catalog app.
pub mod app;
mod diagnostics;
pub mod version;

// Android entry point: cargo-apk / xbuild launch `android_main`, not
// `main`. Initializes the Slint Android backend, then runs the same catalog
// app as desktop (src/main.rs calls `app::run()` the same way).
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    diagnostics::init();
    // Record panic messages for on-screen diagnostics (see nova-media/net).
    crate::net::install_panic_hook();
    // App-private dirs: the redb database lives in the internal files dir
    // (Data), the poster disk cache in the sibling system cache dir.
    // Capture the files dir before `init` takes the handle.
    if let Some(dir) = app.internal_data_path() {
        nova_config::set_android_files_dir(dir);
    } else {
        eprintln!("nova: no internal data path; storage falls back to NOVA_DATA_DIR/temp");
    }
    // Stash the VM + activity raw pointers for the external-player fallback.
    crate::player::set_android_runtime(app.vm_as_ptr(), app.activity_as_ptr());
    // Keep a clone of the AndroidApp so system-bar changes can be marshalled
    // onto the Java main thread (Slint's event loop runs on a native thread).
    crate::player::set_android_app(app.clone());
    // Resume and renderer setup jointly release a session-scoped recovery
    // request. The patched backend suspends on TerminateWindow while the old
    // window is valid, so mpv teardown runs with its original GL context current.
    // Activity pause outside visible PiP also pauses playback; returning from
    // the background follows the device-local auto-continue preference.
    slint::android::init_with_event_listener(app, |event| {
        use slint::android::android_activity::{MainEvent, PollEvent};
        if let PollEvent::Main(main) = event {
            match main {
                MainEvent::Pause => {
                    nova_sync::set_foreground(false);
                    // PiP pauses the activity while its native video surface
                    // remains visible. Only a real background/lock pauses mpv.
                    if !crate::player::picture_in_picture_keeps_playing() {
                        crate::app::remote_playback::playback_foreground(false);
                        crate::player::note_android_pause();
                        crate::app::note_android_pause_checkpoint();
                    }
                }
                MainEvent::Resume { .. } => {
                    nova_sync::set_foreground(true);
                    crate::app::remote_playback::playback_foreground(true);
                    crate::app::note_cache_maintenance_resume();
                    crate::player::note_android_resume()
                }
                _ => {}
            }
        }
    })
    .unwrap();
    if let Err(e) = app::run() {
        eprintln!("nova: {e}");
    }
}

/// NativeActivity callbacks arrive on Java's main thread; Slint and the player
/// are owned by android-activity's native event thread.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_NovaActivity_nativePipEvent(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    event: i32,
) {
    let _ = slint::invoke_from_event_loop(move || {
        crate::player::note_android_pip_event(event);
        if event == 4 {
            crate::app::remote_playback::playback_foreground(false);
            crate::app::note_android_pause_checkpoint();
        }
    });
}
