//! Current official Trakt v2 protocol. Auth moved to auth.trakt.tv in 2026;
//! native device authorization uses a public client ID, never a client secret.
use super::*;
use std::time::Instant;

/// Public canonical metadata only; no account data or credentials in the cache.
pub(super) struct MovieSummary {
    requested: MediaIds,
    movie: Value,
    fetched: Instant,
}
const MAX_MOVIE_SUMMARIES: usize = 64;
const MOVIE_SUMMARY_TTL: Duration = Duration::from_secs(86400);

impl Client {
    fn trakt_pages(
        &self,
        credentials: &Credentials,
        path: &str,
        progress: bool,
    ) -> Result<Vec<Value>, ApiError> {
        let mut result = Vec::new();
        let mut previous = None;
        for page in 1..=MAX_PAGES {
            let mut query = vec![("page", page.to_string()), ("limit", "100".into())];
            if progress {
                query.push(("extended", "progress".into()));
            }
            let response = self.request(credentials, "GET", path, &query, None)?;
            let pagination = response.pagination;
            let body = response.body;
            let rows = body.as_array().ok_or(ApiError::InvalidResponse)?;
            if rows.is_empty() {
                return Ok(result);
            }
            // Some server revisions repeat the final page instead of returning
            // []; never silently accept a truncated or duplicate history.
            if previous.as_ref() == Some(&body) {
                return Err(ApiError::InvalidResponse);
            }
            result.extend(rows.iter().cloned());
            if result.len() > MAX_TITLES {
                return Err(ApiError::InvalidResponse);
            }
            if let Some((applied, count)) = pagination {
                if applied != page as u64 || applied > count {
                    return Err(ApiError::InvalidResponse);
                }
                if applied == count {
                    return Ok(result);
                }
            }
            previous = Some(body);
        }
        Err(ApiError::InvalidResponse)
    }

    pub(super) fn trakt_library(
        &self,
        credentials: &Credentials,
    ) -> Result<Vec<RemoteTitle>, ApiError> {
        let mut result = Vec::new();
        for (suffix, kind) in [("movies", MediaType::Movie), ("shows", MediaType::Show)] {
            for row in self.trakt_pages(
                credentials,
                &format!("/sync/watchlist/{suffix}/added/asc"),
                false,
            )? {
                merge_titles(
                    &mut result,
                    vec![parse_title(&row, kind, ListStatus::Planning, false)?],
                );
            }
            for mut row in self.trakt_pages(
                credentials,
                &format!("/sync/watched/{suffix}"),
                kind == MediaType::Show,
            )? {
                if kind == MediaType::Show {
                    let seasons = row
                        .get_mut("seasons")
                        .and_then(Value::as_array_mut)
                        .ok_or(ApiError::InvalidResponse)?;
                    for season in seasons {
                        let episodes = season
                            .get_mut("episodes")
                            .and_then(Value::as_array_mut)
                            .ok_or(ApiError::InvalidResponse)?;
                        episodes.retain(|ep| {
                            ep["plays"].as_u64().is_some_and(|n| n > 0)
                                || ep["completed"].as_bool() == Some(true)
                                || ep["last_watched_at"]
                                    .as_str()
                                    .is_some_and(|s| !s.is_empty())
                        });
                    }
                }
                let mut title = parse_title(&row, kind, ListStatus::Completed, false)?;
                if kind == MediaType::Show {
                    title.status = ListStatus::Watching;
                }
                merge_titles(&mut result, vec![title]);
            }
        }
        // Multiple episode playback rows may belong to one show. Retain the
        // most recently paused one (the endpoint's documented order).
        let rows = self.trakt_pages(credentials, "/sync/playback", false)?;
        for row in rows.iter().rev() {
            apply_playback(&mut result, &json!([row]))?;
        }
        if result.len() > MAX_TITLES {
            return Err(ApiError::InvalidResponse);
        }
        Ok(result)
    }

    pub(super) fn trakt_scrobble(
        &self,
        credentials: &Credentials,
        ids: &MediaIds,
        kind: MediaType,
        ep: Option<Episode>,
        progress: f64,
        action: ScrobbleAction,
    ) -> Result<(), ApiError> {
        validate_episode(kind, ep)?;
        if !progress.is_finite() || !(0.0..=100.0).contains(&progress) {
            return Err(ApiError::InvalidInput);
        }
        let mut body = json!({"progress":progress});
        if kind == MediaType::Movie {
            body["movie"] = self.trakt_movie_summary(credentials, ids)?;
        } else {
            let ep = ep.ok_or(ApiError::InvalidInput)?;
            // Trakt scrobbles require the episode's own ID, not its parent's
            // show ID. Resolve one exact season/episode against the known show.
            let show = self.trakt_show_id(credentials, ids)?;
            let episode = self
                .request(
                    credentials,
                    "GET",
                    &format!("/shows/{show}/seasons/{}/episodes/{}", ep.season, ep.number),
                    &[],
                    None,
                )?
                .body;
            if episode["season"].as_u64() != Some(u64::from(ep.season))
                || episode["number"].as_u64() != Some(u64::from(ep.number))
            {
                return Err(ApiError::InvalidResponse);
            }
            let id = episode["ids"]["trakt"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or(ApiError::InvalidResponse)?;
            body["episode"] = json!({"ids":{"trakt":id}});
        }
        let action = match action {
            ScrobbleAction::Start => "start",
            ScrobbleAction::Pause => "pause",
            ScrobbleAction::Stop => "stop",
        };
        mutation_receipt(
            self.request(
                credentials,
                "POST",
                &format!("/scrobble/{action}"),
                &[],
                Some(body),
            )?
            .body,
        )
    }

    fn trakt_movie_summary(
        &self,
        credentials: &Credentials,
        ids: &MediaIds,
    ) -> Result<Value, ApiError> {
        request_ids(ids, Service::Trakt)?;
        {
            let mut cache = self.trakt_movies.lock().map_err(|_| ApiError::Offline)?;
            cache.retain(|entry| entry.fetched.elapsed() < MOVIE_SUMMARY_TTL);
            if let Some(summary) = cache.iter().find(|entry| entry.requested == *ids) {
                return Ok(summary.movie.clone());
            }
        }
        let movie_id = if let Some(imdb) = &ids.imdb {
            imdb.clone()
        } else {
            let tmdb = ids.tmdb.ok_or(ApiError::Unsupported)?;
            let rows = self
                .request(
                    credentials,
                    "GET",
                    &format!("/search/tmdb/{tmdb}"),
                    &[("type", "movie".into())],
                    None,
                )?
                .body;
            let rows = rows.as_array().ok_or(ApiError::InvalidResponse)?;
            let mut matches = rows.iter().filter(|row| {
                row["type"] == "movie"
                    && parse_ids(&row["movie"], false).is_ok_and(|found| ids.matches(&found))
            });
            let found = matches.next().ok_or(ApiError::Unsupported)?;
            if matches.next().is_some() {
                return Err(ApiError::InvalidResponse);
            }
            found["movie"]["ids"]["trakt"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or(ApiError::InvalidResponse)?
                .to_string()
        };
        let value = self
            .request(
                credentials,
                "GET",
                &format!("/movies/{movie_id}"),
                &[],
                None,
            )?
            .body;
        let canonical = parse_ids(&value, false)?;
        if !ids.matches(&canonical) {
            return Err(ApiError::InvalidResponse);
        }
        let title = string(&value["title"])?;
        let year = value["year"]
            .as_u64()
            .filter(|n| (1..=9999).contains(n))
            .ok_or(ApiError::InvalidResponse)?;
        let trakt = value["ids"]["trakt"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or(ApiError::InvalidResponse)?;
        let mut canonical_ids = request_ids(&canonical, Service::Trakt)?;
        canonical_ids["trakt"] = json!(trakt);
        let movie = json!({"title":title,"year":year,"ids":canonical_ids});
        let mut cache = self.trakt_movies.lock().map_err(|_| ApiError::Offline)?;
        if cache.len() == MAX_MOVIE_SUMMARIES {
            cache.remove(0);
        }
        cache.push(MovieSummary {
            requested: ids.clone(),
            movie: movie.clone(),
            fetched: Instant::now(),
        });
        Ok(movie)
    }

    fn trakt_show_id(&self, credentials: &Credentials, ids: &MediaIds) -> Result<String, ApiError> {
        if !ids.valid() {
            return Err(ApiError::InvalidInput);
        }
        if let Some(imdb) = &ids.imdb {
            return Ok(imdb.clone());
        }
        let (kind, id) = ids
            .tmdb
            .map(|n| ("tmdb", n))
            .or_else(|| ids.tvdb.map(|n| ("tvdb", n)))
            .ok_or(ApiError::Unsupported)?;
        let rows = self
            .request(
                credentials,
                "GET",
                &format!("/search/{kind}/{id}"),
                &[("type", "show".into())],
                None,
            )?
            .body;
        let rows = rows.as_array().ok_or(ApiError::InvalidResponse)?;
        let mut matches = rows.iter().filter(|row| {
            row["type"] == "show"
                && parse_ids(&row["show"], false).is_ok_and(|found| ids.matches(&found))
        });
        let found = matches.next().ok_or(ApiError::Unsupported)?;
        if matches.next().is_some() {
            return Err(ApiError::InvalidResponse);
        }
        found["show"]["ids"]["trakt"]
            .as_u64()
            .filter(|n| *n > 0)
            .map(|n| n.to_string())
            .ok_or(ApiError::InvalidResponse)
    }

    fn trakt_auth(
        &self,
        client_id: &str,
        path: &str,
        body: Value,
        polling: bool,
    ) -> Result<Value, ApiError> {
        validate_client_id(client_id)?;
        let response = self.transport.send(Request {
            method: "POST",
            url: format!("https://auth.trakt.tv{path}"),
            bearer: None,
            trakt_client_id: Some(client_id.into()),
            body: Some(body),
        })?;
        if polling {
            match response.status {
                400 => return Err(ApiError::Pending),
                404 | 409 | 410 | 418 => return Err(ApiError::Expired),
                429 => return Err(ApiError::SlowDown),
                _ => {}
            }
        }
        if response.body["error"] == "invalid_grant" {
            return Err(ApiError::Authentication);
        }
        Ok(checked(response)?.body)
    }

    pub fn begin_trakt_device(&self, client_id: &str) -> Result<DeviceCode, ApiError> {
        let mut body = self.trakt_auth(
            client_id,
            "/oauth/device/code",
            json!({"client_id":client_id}),
            false,
        )?;
        let result = (|| {
            let uri = string(&body["verification_url"])?;
            let url = url::Url::parse(&uri).map_err(|_| ApiError::InvalidResponse)?;
            if url.scheme() != "https"
                || !matches!(
                    url.host_str(),
                    Some("auth.trakt.tv" | "trakt.tv" | "app.trakt.tv")
                )
                || url.path() != "/activate"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.port().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(ApiError::InvalidResponse);
            }
            let user_code = string(&body["user_code"])?;
            if user_code.len() > 32
                || !user_code
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(ApiError::InvalidResponse);
            }
            Ok(DeviceCode {
                device_code: Secret::new(string(&body["device_code"])?)?,
                user_code,
                verification_uri: uri,
                verification_uri_complete: None,
                expires_in: body["expires_in"]
                    .as_u64()
                    .filter(|n| (1..=3600).contains(n))
                    .ok_or(ApiError::InvalidResponse)?,
                interval: body["interval"]
                    .as_u64()
                    .filter(|n| (1..=60).contains(n))
                    .ok_or(ApiError::InvalidResponse)?,
            })
        })();
        wipe_json(&mut body);
        result
    }
    pub fn poll_trakt_device(&self, client_id: &str, code: &Secret) -> Result<Tokens, ApiError> {
        trakt_tokens(self.trakt_auth(
            client_id,
            "/oauth/device/token",
            json!({"client_id":client_id,"code":code.expose()}),
            true,
        )?)
    }
    /// Register this redirect URI with the public app, even for device sign-in.
    pub fn refresh_trakt(&self, client_id: &str, refresh: &Secret) -> Result<Tokens, ApiError> {
        trakt_tokens(self.trakt_auth(client_id,"/oauth/token",json!({"client_id":client_id,"refresh_token":refresh.expose(),"grant_type":"refresh_token","redirect_uri":"urn:ietf:wg:oauth:2.0:oob"}),false)?)
    }
}
fn trakt_tokens(mut body: Value) -> Result<Tokens, ApiError> {
    let result = (|| {
        if !body["token_type"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
        {
            return Err(ApiError::InvalidResponse);
        }
        Ok(Tokens {
            access_token: Secret::new(string(&body["access_token"])?)?,
            refresh_token: Secret::new(string(&body["refresh_token"])?)?,
            expires_in: body["expires_in"]
                .as_u64()
                .filter(|n| (1..=31 * 86400).contains(n))
                .ok_or(ApiError::InvalidResponse)?,
        })
    })();
    wipe_json(&mut body);
    result
}
