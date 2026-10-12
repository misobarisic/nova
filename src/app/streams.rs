//! Stream-row mapping and display text helpers.
use super::*;

/// Presentation only: the original addon text stays intact for downloads and
/// accessibility, while the card can give the release title its own style.
#[derive(Clone, Default)]
pub(crate) struct StreamCardText {
    pub title: String,
    pub metadata: String,
    pub description: String,
    pub size: String,
}

pub(crate) fn stream_card_text(stream: &addons::Stream) -> StreamCardText {
    let label = tidy_stream_text(&stream.label());
    let display = stream_display(stream);
    let detail = display.strip_prefix(&label).unwrap_or_default().trim();
    let mut card = split_stream_card(&label, detail);
    // Stremio addons may expose the selected video's byte size in hints.
    // Read this before falling back to human-readable torrent metadata.
    card.size = stream
        .extra
        .get("behaviorHints")
        .and_then(|hints| hints.get("videoSize"))
        .or_else(|| stream.extra.get("size"))
        .and_then(|size| size.as_u64().or_else(|| size.as_str()?.parse().ok()))
        .filter(|size| *size > 0)
        .map(text::stream_size)
        .or_else(|| advertised_stream_size(&display))
        .unwrap_or_default();
    card
}

fn split_stream_card(label: &str, detail: &str) -> StreamCardText {
    if detail.is_empty() {
        let (title, metadata) = label.split_once('\n').unwrap_or((label, ""));
        return StreamCardText {
            title: title.to_owned(),
            metadata: metadata.to_owned(),
            ..Default::default()
        };
    }
    let (title, description) = detail.split_once('\n').unwrap_or((detail, ""));
    StreamCardText {
        title: title.to_owned(),
        metadata: label.to_owned(),
        description: description.to_owned(),
        size: String::new(),
    }
}

/// Old download jobs retain only the combined display string. Reconstruct
/// their presentation without migrating or rewriting the stored jobs.
pub(crate) fn downloaded_stream_card(job: &DownloadJob) -> StreamCardText {
    let display = tidy_stream_text(&job.display);
    let mut lines = display.lines().peekable();
    let first = lines.next().unwrap_or_default();
    let mut label = first.to_owned();
    // The common addon label is followed by a resolution on its own line.
    if lines.peek().is_some_and(|line| {
        matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "4k" | "8k" | "sd" | "hd" | "full hd"
        ) || line
            .trim()
            .strip_suffix('p')
            .is_some_and(|n| n.parse::<u32>().is_ok())
    }) {
        label.push('\n');
        label.push_str(lines.next().unwrap());
    }
    let detail = lines.collect::<Vec<_>>().join("\n");
    let mut card = split_stream_card(&label, &detail);
    card.size = job
        .total_bytes
        .filter(|size| *size > 0)
        .map(text::stream_size)
        .or_else(|| advertised_stream_size(&display))
        .unwrap_or_default();
    card
}

/// Extract an advertised size only at a numeric/unit boundary. Do not mistake
/// resolution, a release filename or a transfer rate for a file size.
fn advertised_stream_size(display: &str) -> Option<String> {
    for (start, ch) in display.char_indices() {
        if !ch.is_ascii_digit()
            || (start > 0
                && display[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || matches!(c, '.' | ',')))
        {
            continue;
        }
        let tail = &display[start..];
        let number_end = tail
            .find(|c: char| !c.is_ascii_digit() && !matches!(c, '.' | ','))
            .unwrap_or(tail.len());
        let number = &tail[..number_end];
        if number
            .replace(',', ".")
            .parse::<f64>()
            .ok()
            .is_none_or(|n| !n.is_finite() || n <= 0.0)
        {
            continue;
        }
        let rest = tail[number_end..].trim_start();
        let unit_end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let unit = &rest[..unit_end];
        if !matches!(
            unit.to_ascii_uppercase().as_str(),
            "B" | "KB" | "MB" | "GB" | "TB" | "KIB" | "MIB" | "GIB" | "TIB"
        ) {
            continue;
        }
        let after_unit = &rest[unit_end..];
        if after_unit.trim_start().starts_with('/')
            || after_unit
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '.' | '_'))
        {
            continue;
        }
        return Some(format!("{number} {unit}"));
    }
    None
}

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

/// Resolver envelopes from ordinary remote addons are deliberately ignored.
/// Only the installed bundled owner may route an action into its sandbox.
pub(crate) fn stream_source_for_addon(
    stream: &addons::Stream,
    owner_url: &str,
    addon_generation: u64,
    settings_generation: u64,
) -> StreamSource {
    if stream.web_url().is_some() || stream.info_hash.is_some() {
        return stream_source(stream);
    }
    let Some(raw) = stream.extra.get("novaResolver") else {
        return StreamSource::Unsupported;
    };
    let Some(provider) = raw.get("providerId").and_then(serde_json::Value::as_str) else {
        return StreamSource::Unsupported;
    };
    let Some(token) = raw
        .get("token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty() && token.len() <= 8192)
    else {
        return StreamSource::Unsupported;
    };
    let url = format!("nova-provider://{provider}");
    if owner_url.trim_end_matches('/') != url
        || !nova_providers::bundled_providers()
            .iter()
            .any(|definition| definition.url == url)
    {
        return StreamSource::Unsupported;
    }
    StreamSource::Resolver {
        provider_url: url,
        token: token.into(),
        settings_generation,
        addon_generation,
    }
}

/// A remote Stremio addon cannot inject internal provider status messages.
pub(crate) fn provider_stream_warnings(bytes: &[u8], owner_url: &str) -> Vec<(String, String)> {
    if !nova_providers::bundled_providers()
        .iter()
        .any(|provider| provider.url == owner_url)
    {
        return vec![];
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return vec![];
    };
    value
        .get("novaWarnings")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .take(16)
        .filter_map(|row| {
            let message = row.get("message")?.as_str()?;
            let source = row
                .get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            (!message.is_empty()
                && message.len() <= 256
                && source.len() <= 64
                && !message.chars().chain(source.chars()).any(char::is_control))
            .then(|| (source.into(), message.into()))
        })
        .collect()
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
