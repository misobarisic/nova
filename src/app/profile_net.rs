//! Capture ownership before leaf-network workers invoke app continuations.
use super::profiles;
pub use nova_media::net::*;
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
    let progress_context = context.clone();
    nova_media::net::fetch_bytes_with_progress(
        url,
        std::sync::Arc::new(move |bytes| {
            if profiles::is_current(&progress_context) {
                profiles::with_context(progress_context.clone(), || progress(bytes));
            }
        }),
        move |result| {
            if profiles::is_current(&context) {
                profiles::with_context(context, || then(result));
            }
        },
    );
}
pub(super) fn fetch_image(
    url: String,
    image_use: impl Into<nova_media::cache::ImageUse>,
    then: impl FnOnce(Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>) + Send + 'static,
) {
    let context = profiles::capture();
    nova_media::net::fetch_image(url, image_use, move |result| {
        if profiles::is_current(&context) {
            profiles::with_context(context, || then(result));
        }
    });
}
