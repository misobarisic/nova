//! Bounded HTTPS requests. Errors never include response bodies or credentials.
use crate::*;
use serde_json::{Value, json};
use std::{io::Read, num::NonZeroU32, time::Duration};

pub struct Request {
    pub method: &'static str,
    pub url: String,
    pub bearer: Option<Secret>,
    pub body: Body,
}
pub enum Body {
    Empty,
    Json(Value),
    Form(Vec<(String, String)>),
}
impl Drop for Body {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        if let Self::Form(values) = self {
            for (_, value) in values {
                value.zeroize();
            }
        }
    }
}
pub struct Response {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub reset_at: Option<u64>,
    pub body: Value,
}
pub trait Transport: Send + Sync {
    fn send(&self, request: Request) -> Result<Response, ApiError>;
}
#[derive(Clone)]
pub struct HttpsTransport {
    client: reqwest::blocking::Client,
}
impl HttpsTransport {
    pub fn new() -> Result<Self, ApiError> {
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent("Nova/0.1 anime-tracking")
            .build()
            .map_err(|_| ApiError::Offline)?;
        Ok(Self { client })
    }
}
impl Transport for HttpsTransport {
    fn send(&self, request: Request) -> Result<Response, ApiError> {
        // Only the two official APIs and MAL's token endpoint may receive credentials.
        let url = url::Url::parse(&request.url).map_err(|_| ApiError::InvalidInput)?;
        if url.scheme() != "https"
            || !matches!(
                url.host_str(),
                Some("api.myanimelist.net" | "myanimelist.net" | "graphql.anilist.co")
            )
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err(ApiError::InvalidInput);
        }
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| ApiError::InvalidInput)?;
        let mut builder = self.client.request(method, url);
        if let Some(token) = request.bearer.as_ref() {
            builder = builder.bearer_auth(token.expose());
        }
        builder = match &request.body {
            Body::Empty => builder,
            Body::Json(value) => builder.json(value),
            Body::Form(value) => builder.form(value),
        };
        let mut response = builder.send().map_err(|_| ApiError::Offline)?;
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.parse().ok())
        };
        let retry_after = header("retry-after");
        let reset_at = header("x-ratelimit-reset");
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        response
            .by_ref()
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ApiError::Offline)?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(ApiError::InvalidResponse);
        }
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Ok(Response {
            status,
            retry_after,
            reset_at,
            body,
        })
    }
}
pub(crate) fn checked(response: Response, now: u64) -> Result<Value, ApiError> {
    match response.status {
        200..=299 => Ok(response.body),
        401 | 403 => Err(ApiError::Authentication),
        429 => Err(ApiError::RateLimited {
            retry_at: response
                .reset_at
                .filter(|t| *t > now)
                .unwrap_or_else(|| now.saturating_add(response.retry_after.unwrap_or(60).max(1))),
        }),
        500..=599 => Err(ApiError::Offline),
        _ => Err(ApiError::Rejected),
    }
}
fn id(value: &Value) -> Result<NonZeroU32, ApiError> {
    value
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v <= i32::MAX as u32)
        .and_then(NonZeroU32::new)
        .ok_or(ApiError::InvalidResponse)
}
fn unsigned(value: &Value) -> Result<u32, ApiError> {
    value
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v <= i32::MAX as u32)
        .ok_or(ApiError::InvalidResponse)
}
fn string(value: &Value) -> Result<String, ApiError> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .map(str::to_owned)
        .ok_or(ApiError::InvalidResponse)
}
fn date(value: &Value) -> Result<ListDate, ApiError> {
    if value.is_null() {
        return Ok(ListDate::default());
    }
    let parsed = if let Some(s) = value.as_str() {
        let parts: Vec<_> = s.split('-').collect();
        if parts.len() != 3 {
            return Err(ApiError::InvalidResponse);
        }
        ListDate {
            year: Some(parts[0].parse().map_err(|_| ApiError::InvalidResponse)?),
            month: Some(parts[1].parse().map_err(|_| ApiError::InvalidResponse)?),
            day: Some(parts[2].parse().map_err(|_| ApiError::InvalidResponse)?),
        }
    } else {
        serde_json::from_value(value.clone()).map_err(|_| ApiError::InvalidResponse)?
    };
    if !parsed.valid() {
        return Err(ApiError::InvalidResponse);
    }
    Ok(parsed)
}
fn anilist_status(s: &str) -> Result<ListStatus, ApiError> {
    Ok(match s {
        "PLANNING" => ListStatus::Planning,
        "CURRENT" => ListStatus::Watching,
        "COMPLETED" => ListStatus::Completed,
        "PAUSED" => ListStatus::OnHold,
        "DROPPED" => ListStatus::Dropped,
        "REPEATING" => ListStatus::Repeating,
        _ => return Err(ApiError::InvalidResponse),
    })
}
fn mal_status(s: &str) -> Result<ListStatus, ApiError> {
    Ok(match s {
        "plan_to_watch" => ListStatus::Planning,
        "watching" => ListStatus::Watching,
        "completed" => ListStatus::Completed,
        "on_hold" => ListStatus::OnHold,
        "dropped" => ListStatus::Dropped,
        _ => return Err(ApiError::InvalidResponse),
    })
}
fn status_value(service: Service, status: ListStatus) -> &'static str {
    match (service, status) {
        (Service::AniList, ListStatus::Planning) => "PLANNING",
        (Service::AniList, ListStatus::Watching) => "CURRENT",
        (Service::AniList, ListStatus::Completed) => "COMPLETED",
        (Service::AniList, ListStatus::OnHold) => "PAUSED",
        (Service::AniList, ListStatus::Dropped) => "DROPPED",
        (Service::AniList, ListStatus::Repeating) => "REPEATING",
        (Service::MyAnimeList, ListStatus::Planning) => "plan_to_watch",
        (Service::MyAnimeList, ListStatus::Watching) => "watching",
        (Service::MyAnimeList, ListStatus::Completed) => "completed",
        (Service::MyAnimeList, ListStatus::OnHold) => "on_hold",
        (Service::MyAnimeList, ListStatus::Dropped) => "dropped",
        (Service::MyAnimeList, ListStatus::Repeating) => "watching",
    }
}
const ENTRY_FIELDS: &str = "id userId mediaId progress status score(format:$scoreFormat) startedAt{year month day} completedAt{year month day}";
const MEDIA_FIELDS: &str =
    "id idMal type title{romaji english native} format episodes status seasonYear";
/// An adapter is scoped to one verified account and one in-memory token.
pub struct Client<T: Transport = HttpsTransport> {
    service: Service,
    token: Option<Secret>,
    transport: T,
    viewer: Option<Viewer>,
}
impl<T: Transport> Client<T> {
    pub fn new(service: Service, token: Secret, transport: T) -> Self {
        Self {
            service,
            token: Some(token),
            transport,
            viewer: None,
        }
    }
    fn request(
        &self,
        method: &'static str,
        url: String,
        body: Body,
        now: u64,
    ) -> Result<Value, ApiError> {
        checked(
            self.transport.send(Request {
                method,
                url,
                bearer: self.token.as_ref().map(Secret::duplicate),
                body,
            })?,
            now,
        )
    }
    fn graphql(&self, query: String, variables: Value, now: u64) -> Result<Value, ApiError> {
        let value = self.request(
            "POST",
            "https://graphql.anilist.co".into(),
            Body::Json(json!({"query":query,"variables":variables})),
            now,
        )?;
        if value
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty())
        {
            let errors = value["errors"]
                .as_array()
                .ok_or(ApiError::InvalidResponse)?;
            if errors.iter().any(|e| e["status"].as_u64() == Some(401)) {
                return Err(ApiError::Authentication);
            }
            if errors.iter().any(|e| e["status"].as_u64() == Some(429)) {
                return Err(ApiError::RateLimited {
                    retry_at: now.saturating_add(60),
                });
            }
            return Err(ApiError::Rejected);
        }
        value
            .get("data")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or(ApiError::InvalidResponse)
    }
    pub fn verify(&mut self, now: u64) -> Result<Viewer, ApiError> {
        if self.token.is_none() {
            return Err(ApiError::Authentication);
        }
        let (value, score_format) = match self.service {
            Service::MyAnimeList => (
                self.request(
                    "GET",
                    "https://api.myanimelist.net/v2/users/@me".into(),
                    Body::Empty,
                    now,
                )?,
                ScoreFormat::Point10,
            ),
            Service::AniList => {
                let data = self.graphql(
                    "query{Viewer{id name mediaListOptions{scoreFormat}}}".into(),
                    json!({}),
                    now,
                )?;
                let value = data["Viewer"].clone();
                let score = match value["mediaListOptions"]["scoreFormat"].as_str() {
                    Some("POINT_100") => ScoreFormat::Point100,
                    Some("POINT_10_DECIMAL") => ScoreFormat::Point10Decimal,
                    Some("POINT_10") => ScoreFormat::Point10,
                    Some("POINT_5") => ScoreFormat::Point5,
                    Some("POINT_3") => ScoreFormat::Point3,
                    _ => return Err(ApiError::InvalidResponse),
                };
                (value, score)
            }
        };
        let viewer = Viewer {
            account: AccountKey {
                service: self.service,
                remote_user_id: id(&value["id"])?,
            },
            name: string(&value["name"])?,
            score_format,
        };
        if self
            .viewer
            .as_ref()
            .is_some_and(|old| old.account != viewer.account)
        {
            return Err(ApiError::WrongAccount);
        }
        self.viewer = Some(viewer.clone());
        Ok(viewer)
    }
    /// Public AniList metadata can resolve cross-references without connecting
    /// the unrelated service. It can never verify or mutate a user list.
    pub fn public_anilist(transport: T) -> Self {
        Self {
            service: Service::AniList,
            token: None,
            transport,
            viewer: None,
        }
    }
    pub fn media_by_mal(&self, mal_id: NonZeroU32, now: u64) -> Result<Option<Media>, ApiError> {
        if self.service != Service::AniList {
            return Err(ApiError::UnsupportedField);
        }
        let data=self.graphql(format!("query($idMal:Int){{Page(page:1,perPage:2){{media(idMal:$idMal,type:ANIME){{{MEDIA_FIELDS}}}}}}}"),json!({"idMal":mal_id}),now)?;
        let entries = data["Page"]["media"]
            .as_array()
            .ok_or(ApiError::InvalidResponse)?;
        if entries.len() > 1 {
            return Err(ApiError::InvalidResponse);
        }
        let media = entries
            .first()
            .map(|value| parse_media(Service::AniList, value))
            .transpose()?;
        if media.as_ref().is_some_and(|m| m.mal_id != Some(mal_id)) {
            return Err(ApiError::InvalidResponse);
        }
        Ok(media)
    }
    pub fn viewer(&self) -> Result<&Viewer, ApiError> {
        self.viewer.as_ref().ok_or(ApiError::Authentication)
    }
    pub fn search(&self, query: &str, now: u64) -> Result<Vec<Media>, ApiError> {
        if query.trim().is_empty() || query.len() > 512 {
            return Err(ApiError::InvalidInput);
        }
        match self.service {
            Service::MyAnimeList => {
                let mut url = url::Url::parse("https://api.myanimelist.net/v2/anime")
                    .map_err(|_| ApiError::InvalidInput)?;
                url.query_pairs_mut()
                    .append_pair("q", query)
                    .append_pair("limit", "20")
                    .append_pair(
                        "fields",
                        "id,title,media_type,num_episodes,status,start_date",
                    );
                let value = self.request("GET", url.into(), Body::Empty, now)?;
                value["data"]
                    .as_array()
                    .filter(|v| v.len() <= 20)
                    .ok_or(ApiError::InvalidResponse)?
                    .iter()
                    .map(|v| parse_media(self.service, &v["node"]))
                    .collect()
            }
            Service::AniList => {
                let data=self.graphql(format!("query($search:String){{Page(page:1,perPage:20){{media(search:$search,type:ANIME){{{MEDIA_FIELDS}}}}}}}"),json!({"search":query}),now)?;
                data["Page"]["media"]
                    .as_array()
                    .filter(|v| v.len() <= 20)
                    .ok_or(ApiError::InvalidResponse)?
                    .iter()
                    .map(|v| parse_media(self.service, v))
                    .collect()
            }
        }
    }
    pub fn media(&self, media_id: NonZeroU32, now: u64) -> Result<Media, ApiError> {
        let value=match self.service {
            Service::MyAnimeList => self.request("GET",format!("https://api.myanimelist.net/v2/anime/{media_id}?fields=id,title,media_type,num_episodes,status,start_date"),Body::Empty,now)?,
            Service::AniList => self.graphql(format!("query($id:Int){{Media(id:$id,type:ANIME){{{MEDIA_FIELDS}}}}}"),json!({"id":media_id}),now)?["Media"].clone(),
        };
        let media = parse_media(self.service, &value)?;
        if media.id != media_id {
            return Err(ApiError::InvalidResponse);
        }
        Ok(media)
    }
    pub fn read(&self, media_id: NonZeroU32, now: u64) -> Result<Option<RemoteEntry>, ApiError> {
        let viewer = self.viewer()?;
        let value = match self.service {
            Service::MyAnimeList => {
                let data = self.request(
                    "GET",
                    format!(
                        "https://api.myanimelist.net/v2/anime/{media_id}?fields=my_list_status"
                    ),
                    Body::Empty,
                    now,
                )?;
                if id(&data["id"])? != media_id {
                    return Err(ApiError::InvalidResponse);
                }
                data.get("my_list_status").cloned().unwrap_or(Value::Null)
            }
            // Using Media.mediaListEntry scopes absence to the authenticated Viewer,
            // without treating a GraphQL 404 or another error as an absent list entry.
            Service::AniList => {
                let data=self.graphql(format!("query($id:Int,$scoreFormat:ScoreFormat){{Media(id:$id,type:ANIME){{id mediaListEntry{{{ENTRY_FIELDS}}}}}}}"),json!({"id":media_id,"scoreFormat":viewer.score_format.anilist()}),now)?;
                if id(&data["Media"]["id"])? != media_id {
                    return Err(ApiError::InvalidResponse);
                }
                data["Media"]
                    .get("mediaListEntry")
                    .cloned()
                    .ok_or(ApiError::InvalidResponse)?
            }
        };
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(parse_entry(viewer, media_id, &value)?))
    }
    pub fn update(
        &self,
        media_id: NonZeroU32,
        existing: Option<&RemoteEntry>,
        patch: &EntryPatch,
        now: u64,
    ) -> Result<RemoteEntry, ApiError> {
        let viewer = self.viewer()?;
        patch.validate(self.service, viewer.score_format)?;
        if existing.is_some_and(|e| e.account != viewer.account || e.media_id != media_id) {
            return Err(ApiError::WrongAccount);
        }
        let value = match self.service {
            Service::MyAnimeList => {
                let mut form = Vec::new();
                if let Some(v) = patch.progress {
                    form.push(("num_watched_episodes".into(), v.to_string()));
                }
                if let Some(v) = patch.status {
                    form.push(("status".into(), status_value(self.service, v).into()));
                }
                if let Some(v) = patch.score_tenths {
                    form.push(("score".into(), (v / 10).to_string()));
                }
                self.request(
                    "PATCH",
                    format!("https://api.myanimelist.net/v2/anime/{media_id}/my_list_status"),
                    Body::Form(form),
                    now,
                )?
            }
            Service::AniList => {
                let mut vars =
                    json!({"mediaId":media_id,"scoreFormat":viewer.score_format.anilist()});
                if let Some(entry) = existing {
                    vars["id"] = json!(entry.entry_id.ok_or(ApiError::InvalidResponse)?);
                }
                if let Some(v) = patch.progress {
                    vars["progress"] = json!(v);
                }
                if let Some(v) = patch.status {
                    vars["status"] = json!(status_value(self.service, v));
                }
                if let Some(v) = patch.score_tenths {
                    vars["score"] = json!(f64::from(v) / 10.0);
                }
                if let Some(v) = patch.started {
                    vars["startedAt"] = json!(v);
                }
                if let Some(v) = patch.completed {
                    vars["completedAt"] = json!(v);
                }
                let fields = [
                    ("id", "Int"),
                    ("mediaId", "Int"),
                    ("progress", "Int"),
                    ("status", "MediaListStatus"),
                    ("score", "Float"),
                    ("startedAt", "FuzzyDateInput"),
                    ("completedAt", "FuzzyDateInput"),
                ];
                let present: Vec<_> = fields
                    .iter()
                    .filter(|(name, _)| vars.get(*name).is_some())
                    .collect();
                let declarations = present
                    .iter()
                    .map(|(name, ty)| format!("${name}:{ty}"))
                    .chain(std::iter::once("$scoreFormat:ScoreFormat".into()))
                    .collect::<Vec<_>>()
                    .join(",");
                let arguments = present
                    .iter()
                    .map(|(name, _)| format!("{name}:${name}"))
                    .collect::<Vec<_>>()
                    .join(",");
                self.graphql(format!("mutation({declarations}){{SaveMediaListEntry({arguments}){{{ENTRY_FIELDS}}}}}"),vars,now)?["SaveMediaListEntry"].clone()
            }
        };
        let result = parse_entry(viewer, media_id, &value)?;
        if patch.progress.is_some_and(|v| result.progress != v)
            || patch.status.is_some_and(|v| result.status != v)
            || patch.score_tenths.is_some_and(|v| result.score_tenths != v)
            || patch.started.is_some_and(|v| result.started != v)
            || patch.completed.is_some_and(|v| result.completed != v)
        {
            return Err(ApiError::InvalidResponse);
        }
        Ok(result)
    }
}
fn parse_media(service: Service, value: &Value) -> Result<Media, ApiError> {
    let media_id = id(&value["id"])?;
    let (title, format, episodes, finished, year, mal_id) = match service {
        Service::MyAnimeList => (
            string(&value["title"])?,
            string(&value["media_type"])?,
            unsigned(&value["num_episodes"])?,
            value["status"].as_str() == Some("finished_airing"),
            value["start_date"]
                .as_str()
                .and_then(|s| s.get(..4))
                .and_then(|s| s.parse().ok()),
            Some(media_id),
        ),
        Service::AniList => {
            if value["type"].as_str() != Some("ANIME") {
                return Err(ApiError::InvalidResponse);
            }
            let title = value["title"]["english"]
                .as_str()
                .or(value["title"]["romaji"].as_str())
                .or(value["title"]["native"].as_str())
                .ok_or(ApiError::InvalidResponse)?;
            (
                string(&json!(title))?,
                string(&value["format"])?,
                if value["episodes"].is_null() {
                    0
                } else {
                    unsigned(&value["episodes"])?
                },
                value["status"].as_str() == Some("FINISHED"),
                value["seasonYear"]
                    .as_u64()
                    .and_then(|v| u16::try_from(v).ok()),
                if value["idMal"].is_null() {
                    None
                } else {
                    Some(id(&value["idMal"])?)
                },
            )
        }
    };
    Ok(Media {
        id: media_id,
        mal_id,
        title,
        format,
        episodes: NonZeroU32::new(episodes),
        finished,
        year,
    })
}
fn parse_entry(
    viewer: &Viewer,
    media_id: NonZeroU32,
    value: &Value,
) -> Result<RemoteEntry, ApiError> {
    let (entry_id, progress, status, score, started, completed, repeating) =
        match viewer.account.service {
            Service::MyAnimeList => (
                None,
                unsigned(&value["num_episodes_watched"])?,
                mal_status(value["status"].as_str().ok_or(ApiError::InvalidResponse)?)?,
                unsigned(&value["score"])?
                    .checked_mul(10)
                    .ok_or(ApiError::InvalidResponse)?,
                date(&value["start_date"])?,
                date(&value["finish_date"])?,
                value["is_rewatching"]
                    .as_bool()
                    .ok_or(ApiError::InvalidResponse)?,
            ),
            Service::AniList => {
                if id(&value["userId"])? != viewer.account.remote_user_id
                    || id(&value["mediaId"])? != media_id
                {
                    return Err(ApiError::WrongAccount);
                }
                let status =
                    anilist_status(value["status"].as_str().ok_or(ApiError::InvalidResponse)?)?;
                let score = value["score"]
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 100.0)
                    .ok_or(ApiError::InvalidResponse)?;
                let tenths = (score * 10.0).round() as u32;
                if (score * 10.0 - f64::from(tenths)).abs() > 0.0001 {
                    return Err(ApiError::InvalidResponse);
                }
                (
                    Some(id(&value["id"])?),
                    unsigned(&value["progress"])?,
                    status,
                    tenths,
                    date(&value["startedAt"])?,
                    date(&value["completedAt"])?,
                    status == ListStatus::Repeating,
                )
            }
        };
    if !viewer.score_format.valid(score) {
        return Err(ApiError::InvalidResponse);
    }
    Ok(RemoteEntry {
        account: viewer.account.clone(),
        media_id,
        entry_id,
        progress,
        status,
        score_tenths: score,
        started,
        completed,
        repeating,
    })
}
