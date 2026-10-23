//! Stream-row mapping and display text helpers.
use super::*;

/// Remember the source, not its changing labels, seed counts or subtitle URLs.
/// Owner and movie/episode scope prevent an unrelated release from being pinned.
/// URL-only addons retain exact URL/header matching: stripping tokens or fuzzy
/// title matching could silently confuse two different files.
pub(crate) fn stream_choice_identity(
    owner: &str,
    media_type: &str,
    request: &str,
    stream: &addons::Stream,
) -> String {
    let mut source = stream.clone();
    source.subtitles.clear();
    let scope = serde_json::to_string(&(media_type, request)).expect("stream scope serializes");
    nova_providers::stable_stream_identity(owner, &scope, &source)
}

/// Native episodic types (including anime) share the show's preference, while
/// movies and missing identities have no next-episode recommendation scope.
/// Qualify by type so two provider namespaces cannot alias the same bare ID.
pub(super) fn stream_series_scope(media_type: &str, id: &str) -> Option<String> {
    (!media_type.trim().is_empty() && media_type != "movie" && !id.trim().is_empty())
        .then(|| serde_json::to_string(&(media_type, id)).expect("show scope serializes"))
}

/// Only bounded hashes and recognized release facts are cached; never URLs,
/// credentials, arbitrary addon descriptions or episode-specific file names.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct StreamPreference {
    owner: String,
    binge: Option<String>,
    group: Option<String>,
    resolution: Option<String>,
    codec: Option<String>,
    source: Option<String>,
}

pub(super) fn preference_hash(kind: &str, value: &str) -> String {
    stream_choice_identity(
        "preference",
        kind,
        "",
        &addons::Stream {
            url: Some(value.to_owned()),
            ..Default::default()
        },
    )
}

impl StreamPreference {
    pub(super) fn from_row(row: &StreamUi) -> Self {
        Self::from_card(&row.owner_url, &row.card)
    }

    pub(super) fn from_download(job: &DownloadJob) -> Self {
        Self::from_card("", &downloaded_stream_card(job))
    }

    fn from_card(owner: &str, card: &StreamCardText) -> Self {
        let title = card.title.to_ascii_lowercase();
        let normalized = format!("{} {}", card.metadata, title)
            .to_ascii_lowercase()
            .replace("h.264", "h264")
            .replace("h.265", "h265")
            .replace("web-dl", "webdl")
            .replace("blu-ray", "bluray");
        let tokens: Vec<_> = normalized
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|token| !token.is_empty())
            .collect();
        let fact = |alternatives: &[(&str, &[&str])]| {
            let mut matches = alternatives
                .iter()
                .filter(|(_, aliases)| aliases.iter().any(|alias| tokens.contains(alias)));
            let value = matches.next().map(|(canonical, _)| (*canonical).to_owned());
            if matches.next().is_some() {
                None
            } else {
                value
            }
        };
        // A final -GROUP or leading [GROUP] is release evidence, unlike words
        // from the show's title. Avoid harvesting metadata/seed-count lines.
        let group = title
            .strip_prefix('[')
            .and_then(|s| s.split_once(']').map(|(g, _)| g))
            .or_else(|| {
                title
                    .rsplit_once('-')
                    .map(|(_, g)| g.trim().trim_end_matches(".mkv").trim_end_matches(".mp4"))
            })
            .filter(|g| {
                (2..=32).contains(&g.len())
                    && g.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && g.chars().any(|c| c.is_ascii_alphabetic())
                    && ![
                        "dl", "rip", "webdl", "webrip", "bluray", "hdtv", "1080p", "720p", "2160p",
                        "hevc", "avc", "h264", "h265", "x264", "x265", "av1",
                    ]
                    .contains(g)
            })
            .map(|g| preference_hash("group", g));
        Self {
            owner: preference_hash("owner", owner.trim_end_matches('/')),
            binge: card.binge_group.clone(),
            group,
            resolution: fact(&[
                ("2160", &["2160p", "4k"]),
                ("1080", &["1080p"]),
                ("720", &["720p"]),
                ("480", &["480p"]),
            ]),
            codec: fact(&[
                ("hevc", &["hevc", "h265", "x265"]),
                ("avc", &["avc", "h264", "x264"]),
                ("av1", &["av1"]),
            ]),
            source: fact(&[
                ("webdl", &["webdl"]),
                ("webrip", &["webrip"]),
                ("bluray", &["bluray", "bdrip"]),
                ("hdtv", &["hdtv"]),
            ]),
        }
    }

    /// Offline transfers require the chosen family and every recorded quality
    /// fact. Playback may rank a softer match; unattended disk use may not.
    pub(super) fn offline_matches(&self, row: &StreamUi) -> bool {
        let candidate = Self::from_row(row);
        self.owner == candidate.owner
            && self.score(&candidate).is_some()
            && self
                .resolution
                .as_ref()
                .is_none_or(|v| candidate.resolution.as_ref() == Some(v))
            && self
                .codec
                .as_ref()
                .is_none_or(|v| candidate.codec.as_ref() == Some(v))
            && self
                .source
                .as_ref()
                .is_none_or(|v| candidate.source.as_ref() == Some(v))
    }

    fn score(&self, candidate: &Self) -> Option<u32> {
        // Downloads retain release text but not the original addon URL. Permit
        // local/online matching using the same release evidence, without
        // weakening owner isolation between two online addon responses.
        let local_owner = preference_hash("owner", "");
        if self.owner != candidate.owner
            && self.owner != local_owner
            && candidate.owner != local_owner
        {
            return None;
        }
        let same = |a: &Option<String>, b: &Option<String>| a.is_some() && a == b;
        let binge = same(&self.binge, &candidate.binge);
        let group = same(&self.group, &candidate.group);
        let resolution = same(&self.resolution, &candidate.resolution);
        let codec = same(&self.codec, &candidate.codec);
        let source = same(&self.source, &candidate.source);
        // Quality alone would recommend almost any row. Require a release
        // family, or agreement on all three independent technical facts.
        (binge || group || (resolution && codec && source)).then_some(
            u32::from(binge) * 32
                + u32::from(group) * 16
                + u32::from(resolution) * 4
                + u32::from(codec) * 2
                + u32::from(source) * 2,
        )
    }
}

pub(super) fn stream_recommendation_usable(row: &StreamUi) -> bool {
    match &row.source {
        StreamSource::Unsupported | StreamSource::ExternalUrl(_) => false,
        StreamSource::Downloaded { path, .. } => row.download.as_ref().is_some_and(|job| {
            job.phase == crate::download::DownloadPhase::Completed
                && job.artifact_path.as_ref() == Some(path)
                && path.is_file()
        }),
        _ => true,
    }
}

pub(super) fn recommended_stream(
    rows: &[StreamUi],
    exact: Option<&str>,
    preference: &StreamPreference,
) -> Option<usize> {
    if exact.is_some_and(|id| {
        rows.iter()
            .any(|row| stream_recommendation_usable(row) && row.choice_id.as_deref() == Some(id))
    }) {
        return None;
    }
    // Keep the addon's ranking on equal evidence, including cumulative batches.
    rows.iter()
        .enumerate()
        .filter(|(_, row)| stream_recommendation_usable(row))
        .filter_map(|(index, row)| {
            preference
                .score(&StreamPreference::from_row(row))
                .map(|score| (index, score))
        })
        .fold(None, |best, next| match best {
            Some((_, score)) if score >= next.1 => best,
            _ => Some(next),
        })
        .map(|(index, _)| index)
}

/// Presentation only: the original addon text stays intact for downloads and
/// accessibility, while the card can give the release title its own style.
#[derive(Clone, Default)]
pub(crate) struct StreamCardText {
    pub filter_facts: nova_config::stream_filters::StreamFacts,
    pub title: String,
    pub metadata: String,
    pub description: String,
    pub size: String,
    pub listing_bytes: Option<u64>,
    pub binge_group: Option<String>,
    pub subtitle_hints: Vec<(String, String)>,
}

pub(crate) fn stream_card_text(stream: &addons::Stream) -> StreamCardText {
    let label = tidy_stream_text(&stream.label());
    let display = stream_display(stream);
    let detail = display.strip_prefix(&label).unwrap_or_default().trim();
    let mut card = split_stream_card(&label, detail);
    card.filter_facts = stream_filters::stream_facts(stream);
    // externalUrl represents a browser destination, not a downloadable file.
    if stream.extra.contains_key("externalUrl") {
        return card;
    }
    if let Some(hints) = stream.extra.get("behaviorHints") {
        for key in ["filename", "videoHash", "videoSize"] {
            let Some(value) = hints.get(key) else {
                continue;
            };
            let value = value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_u64().map(|v| v.to_string()));
            if let Some(value) = value.filter(|value| {
                !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
            }) {
                card.subtitle_hints.push((key.into(), value));
            }
        }
    }
    card.listing_bytes = stream
        .extra
        .get("behaviorHints")
        .and_then(|h| h.get("videoSize"))
        .or_else(|| stream.extra.get("size"))
        .and_then(|s| s.as_u64().or_else(|| s.as_str()?.parse().ok()))
        .filter(|s| *s > 0);
    // Stremio addons may expose the selected video's byte size in hints.
    // Read this before falling back to human-readable torrent metadata.
    card.binge_group = stream
        .extra
        .get("behaviorHints")
        .and_then(|hints| hints.get("bingeGroup"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .map(|value| preference_hash("binge", value));
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
        listing_bytes: None,
        binge_group: None,
        subtitle_hints: Vec::new(),
        filter_facts: Default::default(),
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
/// An explicit externalUrl opens a validated browser destination. Otherwise a
/// direct URL (or YouTube id) plays without the embedded engine.
/// Only a bare `infoHash` row (no direct `url`, the Torrentio shape) becomes a
/// torrent — and only on native targets, since the web build has no librqbit
/// and reports it as unsupported.
pub(crate) fn stream_source(s: &addons::Stream) -> StreamSource {
    // The explicit SDK browser destination takes precedence even when an addon
    // also supplies an ambiguous media URL/hash. Invalid destinations must not
    // fall through to playback or a deferred resolver.
    if let Some(external) = s.extra.get("externalUrl") {
        return external
            .as_str()
            .and_then(detail_extras::web_url)
            .map(StreamSource::ExternalUrl)
            .unwrap_or(StreamSource::Unsupported);
    }
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
    if stream.extra.contains_key("externalUrl")
        || stream.web_url().is_some()
        || stream.info_hash.is_some()
    {
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
    if owner_url.trim_end_matches('/') != url || !nova_providers::has_installed_provider(&url) {
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
    if !nova_providers::has_installed_provider(owner_url) {
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

#[cfg(test)]
mod choice_identity_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn download_recommendations_match_online_releases_and_require_completed_files() {
        let root = std::env::temp_dir().join(format!(
            "nova-download-recommendation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("episode.mkv");
        fs::write(&path, b"fixture").unwrap();
        let job = DownloadJob {
            id: "local".into(),
            display: "Addon\n1080p\n[Family] Show 02 HEVC WEB-DL".into(),
            phase: crate::download::DownloadPhase::Completed,
            artifact_path: Some(path.clone()),
            ..Default::default()
        };
        let local = StreamUi {
            id: "download:local".into(),
            choice_id: Some("local".into()),
            owner_url: String::new(),
            display: job.display.clone(),
            card: downloaded_stream_card(&job),
            source: StreamSource::Downloaded {
                job_id: job.id.clone(),
                path: path.clone(),
            },
            addon: "Addon".into(),
            download: Some(job.clone()),
        };
        let online = StreamUi {
            id: "online".into(),
            choice_id: Some("online".into()),
            owner_url: "https://addon.example".into(),
            source: StreamSource::Url("https://media.example/video".into()),
            download: None,
            ..local.clone()
        };
        let preference = StreamPreference::from_row(&online);
        let from_download = StreamPreference::from_download(&job);
        assert!(preference.score(&from_download).is_some());
        assert!(from_download.score(&preference).is_some());
        assert_eq!(
            recommended_stream(&[local.clone(), online.clone()], None, &preference),
            Some(0)
        );
        assert_eq!(
            recommended_stream(&[online.clone()], None, &from_download),
            Some(0)
        );
        assert_eq!(
            recommended_stream(&[local.clone()], Some("local"), &preference),
            None
        );
        let mut other_addon = online.clone();
        other_addon.owner_url = "https://other.example".into();
        assert_eq!(recommended_stream(&[other_addon], None, &preference), None);
        let mut unrelated = local.clone();
        unrelated.card = StreamCardText {
            title: "[Other] Show 02".into(),
            ..Default::default()
        };
        assert_eq!(recommended_stream(&[unrelated], None, &preference), None);
        for phase in [
            crate::download::DownloadPhase::Queued,
            crate::download::DownloadPhase::Failed,
        ] {
            let mut unavailable = local.clone();
            unavailable.download.as_mut().unwrap().phase = phase;
            assert_eq!(recommended_stream(&[unavailable], None, &preference), None);
        }
        fs::remove_file(&path).unwrap();
        assert_eq!(
            recommended_stream(&[local, online], Some("local"), &preference),
            Some(1)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recommendation_scopes_cover_native_episodic_types_without_aliasing() {
        let anime = stream_series_scope("anime", "show").unwrap();
        let series = stream_series_scope("series", "show").unwrap();
        assert_ne!(anime, series);
        assert_ne!(anime, stream_series_scope("anime", "other").unwrap());
        assert!(stream_series_scope("movie", "show").is_none());
        assert!(stream_series_scope("anime", "  ").is_none());
        assert!(stream_series_scope("", "show").is_none());
    }

    #[test]
    fn changing_labels_seed_counts_and_subtitles_keep_the_same_choice() {
        let mut stream: addons::Stream = serde_json::from_value(json!({
            "infoHash": "ABCDEF0123456789ABCDEF0123456789ABCDEF01", "fileIdx": 3,
            "name": "Torrent 1080p", "description": "51 seeders",
            "subtitles": [{"id":"1", "url":"https://example.invalid/old", "lang":"en"}]
        }))
        .unwrap();
        let key = stream_choice_identity("https://addon.example", "series", "tt1:3:2", &stream);
        stream.name = Some("A renamed source".into());
        stream.description = Some("81 seeders and a different description".into());
        stream.extra.insert("seeders".into(), json!(81));
        stream.info_hash = stream.info_hash.map(|hash| hash.to_ascii_lowercase());
        stream.subtitles[0].url = "https://example.invalid/new".into();
        assert_eq!(
            key,
            stream_choice_identity("https://addon.example/", "series", "tt1:3:2", &stream)
        );
        for (owner, kind, request) in [
            ("https://other.example", "series", "tt1:3:2"),
            ("https://addon.example", "series", "tt1:3:3"),
            ("https://addon.example", "movie", "tt1:3:2"),
        ] {
            assert_ne!(key, stream_choice_identity(owner, kind, request, &stream));
        }
        stream.file_idx = Some(4);
        assert_ne!(
            key,
            stream_choice_identity("https://addon.example", "series", "tt1:3:2", &stream)
        );
    }

    #[test]
    fn owned_provider_ids_survive_temporary_urls_but_remote_urls_are_exact() {
        let mut stream: addons::Stream = serde_json::from_value(json!({
            "novaProviderStreamId":"supernova:hash:3:torbox", "url":"https://cdn.example/old-token"
        }))
        .unwrap();
        let key = stream_choice_identity("nova-provider://supernova", "series", "tt1:3:2", &stream);
        stream.url = Some("https://cdn.example/new-token".into());
        stream
            .extra
            .insert("novaResolver".into(), json!({"token":"refreshed"}));
        assert_eq!(
            key,
            stream_choice_identity("nova-provider://supernova", "series", "tt1:3:2", &stream)
        );
        stream.extra.insert(
            "novaProviderStreamId".into(),
            json!("supernova:hash:4:torbox"),
        );
        assert_ne!(
            key,
            stream_choice_identity("nova-provider://supernova", "series", "tt1:3:2", &stream)
        );
        let remote = stream_choice_identity("https://remote.example", "series", "tt1:3:2", &stream);
        stream.url = Some("https://cdn.example/different-video".into());
        assert_ne!(
            remote,
            stream_choice_identity("https://remote.example", "series", "tt1:3:2", &stream),
            "a forged native ID cannot merge unrelated remote files"
        );
        let remote = stream_choice_identity("https://remote.example", "series", "tt1:3:2", &stream);
        stream.extra.insert(
            "behaviorHints".into(),
            json!({"proxyHeaders":{"request":{"Authorization":"another-account"}}}),
        );
        assert_ne!(
            remote,
            stream_choice_identity("https://remote.example", "series", "tt1:3:2", &stream)
        );
    }

    #[test]
    fn declared_external_url_has_browser_semantics_without_file_metadata() {
        let stream: addons::Stream = serde_json::from_value(json!({
            "name": "WatchHub", "description": "Release · 2 GB",
            "externalUrl": "https://watch.example/title/42",
            "url": "https://media.example/ambiguous.mp4", "infoHash": "abc",
            "behaviorHints": {"videoSize": 1234, "bingeGroup": "family"},
            "novaResolver": {"providerId":"owner", "token":"deferred"}
        }))
        .unwrap();
        assert!(
            matches!(stream_source(&stream), StreamSource::ExternalUrl(url) if url == "https://watch.example/title/42")
        );
        assert!(matches!(
            stream_source_for_addon(&stream, "https://addon.example", 1, 1),
            StreamSource::ExternalUrl(_)
        ));
        let card = stream_card_text(&stream);
        assert!(card.size.is_empty());
        assert!(card.listing_bytes.is_none());
        assert!(card.subtitle_hints.is_empty());
        let row = StreamUi {
            id: "external".into(),
            choice_id: Some("external".into()),
            owner_url: "addon".into(),
            display: String::new(),
            card,
            source: stream_source(&stream),
            addon: "WatchHub".into(),
            download: None,
        };
        assert!(!stream_recommendation_usable(&row));
        assert_eq!(
            recommended_stream(
                &[row.clone()],
                Some("external"),
                &StreamPreference::from_row(&row)
            ),
            None
        );
    }

    #[test]
    fn invalid_declared_browser_destinations_never_fall_back_to_media() {
        for external in [
            json!("javascript:alert(1)"),
            json!("file:///tmp/video"),
            json!("https://user:password@example.test/page"),
            json!("https://example.test/\npage"),
            json!("https://example.test/\u{7f}page"),
            json!("https://example.test/".to_owned() + &"a".repeat(8192)),
            json!(null),
            json!(123),
        ] {
            let stream: addons::Stream = serde_json::from_value(json!({"externalUrl": external, "url": "https://media.example/video.mp4", "infoHash":"abc"})).unwrap();
            assert!(matches!(
                stream_source_for_addon(&stream, "https://addon.example", 1, 1),
                StreamSource::Unsupported
            ));
        }
    }

    #[test]
    fn ordinary_watchhub_named_or_hosted_urls_remain_in_app_media() {
        let direct: addons::Stream = serde_json::from_value(
            json!({"name":"WatchHub", "url":"https://watchhub.example/video.mp4"}),
        )
        .unwrap();
        assert!(
            matches!(stream_source(&direct), StreamSource::Url(url) if url == "https://watchhub.example/video.mp4")
        );
        let with_options: addons::Stream = serde_json::from_value(json!({"name":"WatchHub", "url":"https://watchhub.example/video.mp4", "behaviorHints":{"proxyHeaders":{"request":{"Referer":"https://watchhub.example/"}}}})).unwrap();
        assert!(matches!(
            stream_source(&with_options),
            StreamSource::UrlWithOptions { .. }
        ));
    }
}
