//! Capture ownership before leaf-network workers invoke app continuations.
use super::profiles;
pub use nova_media::net::*;
fn accepts_work(context: &profiles::Context) -> bool {
    profiles::is_current(context)
        && !context
            .session
            .as_ref()
            .is_some_and(|session| session.cancelled.load(std::sync::atomic::Ordering::Acquire))
}
pub(super) fn fetch_bytes(
    url: String,
    then: impl FnOnce(Result<Vec<u8>, FetchError>) + Send + 'static,
) {
    fetch_bytes_with_progress(url, std::sync::Arc::new(|_| {}), then);
}
pub(super) fn fetch_bytes_with_progress(
    url: String,
    progress: std::sync::Arc<dyn Fn(Vec<u8>) + Send + Sync>,
    then: impl FnOnce(Result<Vec<u8>, FetchError>) + Send + 'static,
) {
    let context = profiles::capture();
    if !accepts_work(&context) {
        profiles::with_context(context, || {
            then(Err(FetchError {
                kind: FetchErrorKind::Other,
                status: None,
                message: "Profile session ended".into(),
            }))
        });
        return;
    }
    let settings = context
        .session
        .as_ref()
        .and_then(|session| session.providers.get())
        .map(|owner| owner.with(nova_providers::plugin_settings::capture_settings_capability))
        .unwrap_or_else(nova_providers::plugin_settings::capture_settings_capability);
    let inventory = if profiles::enabled() {
        context
            .session
            .as_ref()
            .expect("active requests require a session")
            .provider_inventory()
    } else {
        Ok(nova_providers::OwnedProviderSnapshot::capture())
    };
    let inventory = match inventory {
        Ok(inventory) => inventory,
        Err(message) => {
            profiles::with_context(context, || {
                then(Err(FetchError {
                    kind: FetchErrorKind::Other,
                    status: None,
                    message,
                }))
            });
            return;
        }
    };
    let progress_context = context.clone();
    let live_context = context.clone();
    nova_media::net::fetch_bytes_with_progress_for_owner_controlled(
        url,
        std::sync::Arc::new(move |bytes| {
            profiles::with_context(progress_context.clone(), || progress(bytes));
        }),
        settings,
        inventory,
        std::sync::Arc::new(move || accepts_work(&live_context)),
        move |result| {
            // Completion also releases permits and request bookkeeping. Only
            // publication through invoke_from_event_loop is conditional.
            profiles::with_context(context, || then(result));
        },
    );
}
pub(super) fn fetch_image(
    url: String,
    image_use: impl Into<nova_media::cache::ImageUse>,
    then: impl FnOnce(Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>) + Send + 'static,
) {
    let context = profiles::capture();
    if !accepts_work(&context) {
        profiles::with_context(context, || then(None));
        return;
    }
    // Capture fresh settings through the originating session's retained owner.
    // A worker completion may have another profile's ambient provider scope.
    let (settings, inventory) = if profiles::enabled() {
        let Some(session) = context.session.as_ref() else {
            profiles::with_context(context, || then(None));
            return;
        };
        let Some(owner) = session.providers.get() else {
            profiles::with_context(context, || then(None));
            return;
        };
        let settings = owner.with(nova_providers::plugin_settings::capture_settings_capability);
        let Ok(inventory) = session.provider_inventory() else {
            profiles::with_context(context, || then(None));
            return;
        };
        (settings, inventory)
    } else {
        (
            nova_providers::plugin_settings::capture_settings_capability(),
            nova_providers::OwnedProviderSnapshot::capture(),
        )
    };
    let owner = nova_media::net::ImageFetchOwner::new(settings, inventory);
    let live_context = context.clone();
    nova_media::net::fetch_image_for_owner_controlled(
        url,
        image_use,
        owner,
        std::sync::Arc::new(move || accepts_work(&live_context)),
        move |result| {
            profiles::with_context(context, || then(result));
        },
    );
}
