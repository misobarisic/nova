//! Stream-row mapping and display text helpers.
use super::*;

/// Classify a parsed addon stream into how it can be played here.
///
/// A direct URL (or YouTube id) wins: it plays without the embedded engine.
/// Only a bare `infoHash` row (no direct `url`, the Torrentio shape) becomes a
/// torrent — and only on native targets, since the web build has no librqbit
/// and reports it as unsupported.
pub(crate) fn stream_source(s: &addons::Stream) -> StreamSource {
    if let Some(url) = s.web_url() {
        return StreamSource::Url(url);
    }
    if let Some(info_hash) = s.info_hash.as_deref().filter(|h| !h.trim().is_empty()) {
        return StreamSource::Torrent {
            info_hash: info_hash.to_string(),
            file_idx: s.file_idx,
        };
    }
    StreamSource::Unsupported
}
/// Normalise an addon-supplied label/description: keep newlines (rows render
/// multi-line) but collapse CRLF/CR and runs of blank lines, and trim the
/// leading/trailing newlines.
pub(crate) fn tidy_stream_text(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut on_newline = false;
    for ch in t.chars() {
        match ch {
            '\r' | '\n' => {
                if !on_newline {
                    out.push('\n');
                }
                on_newline = true;
            }
            _ => {
                out.push(ch);
                on_newline = false;
            }
        }
    }
    out.trim_matches('\n').to_string()
}
/// The multi-line text shown for one stream row.
///
/// The first line is the addon's short label (`name`). The second is the
/// detail: `description` when present, otherwise the legacy `title` field,
/// which is where Torrentio puts the release name + seeders/size while
/// leaving `description` empty. `Stream::label()` only uses `title` as a
/// fallback when `name` is missing, so without this the row collapses to just
/// "Torrentio / 4k".
pub(crate) fn stream_display(s: &addons::Stream) -> String {
    let label = tidy_stream_text(&s.label());
    let desc = tidy_stream_text(s.description.as_deref().unwrap_or(""));
    let legacy = tidy_stream_text(s.title_legacy.as_deref().unwrap_or(""));
    let detail = if !desc.is_empty() {
        desc
    } else if !legacy.is_empty() && legacy != label {
        legacy
    } else {
        String::new()
    };
    if detail.is_empty() {
        label
    } else {
        format!("{label}\n{detail}")
    }
}
