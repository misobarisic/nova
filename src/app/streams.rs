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
        let headers = stream_request_headers(s);
        let subtitles = stream_subtitle_files(s);
        if !headers.is_empty() || !subtitles.is_empty() {
            return StreamSource::UrlWithOptions {
                url,
                headers,
                subtitles,
            };
        }
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

fn stream_subtitle_files(stream: &addons::Stream) -> Vec<String> {
    let mut total_bytes = 0usize;
    stream
        .subtitles
        .iter()
        .take(20)
        .filter_map(|subtitle| {
            let url = &subtitle.url;
            if !(url.starts_with("https://") || url.starts_with("http://"))
                || url.len() > 4096
                || url.bytes().any(|byte| byte < b' ' || byte == 127)
            {
                return None;
            }
            total_bytes += url.len();
            (total_bytes <= 32 * 1024).then(|| url.clone())
        })
        .collect()
}

fn stream_request_headers(stream: &addons::Stream) -> Vec<(String, String)> {
    let Some(request_headers) = stream
        .extra
        .get("behaviorHints")
        .and_then(|hints| hints.get("proxyHeaders"))
        .and_then(|proxy| proxy.get("request"))
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };

    let mut headers = Vec::new();
    let mut total_bytes = 0usize;
    for (name, value) in request_headers {
        let Some(value) = value.as_str() else {
            continue;
        };
        if !valid_request_header(name, value) {
            continue;
        }
        total_bytes = total_bytes.saturating_add(name.len() + value.len());
        if headers.len() >= 16 || total_bytes > 8 * 1024 {
            break;
        }
        headers.push((name.clone(), value.to_owned()));
    }
    headers
}

fn valid_request_header(name: &str, value: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
        && value.len() <= 4096
        && !value.bytes().any(|byte| byte < b' ' || byte == 127)
        && !matches!(
            name.to_ascii_lowercase().as_str(),
            "host" | "content-length" | "connection" | "proxy-authorization"
        )
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
