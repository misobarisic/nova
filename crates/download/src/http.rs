use std::io::SeekFrom;
use std::path::{Path, PathBuf};
#[cfg(target_os = "android")]
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, ACCEPT_ENCODING, ETAG, IF_RANGE, LAST_MODIFIED, RANGE};
use reqwest::{Client, Response, StatusCode, Url};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use super::{
    CancellationToken, DownloadError, DownloadJob, DownloadOutcome, DownloadProgress, DownloadResume,
    HttpDownloadRequest, part_path, sanitize_filename, validate_download_url,
};

/// Time allowed for the connection + response headers. A socket that is
/// accepted but never answered (a CDN waiting on a header the download client
/// does not send) would otherwise park the single download slot forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Maximum gap between body chunks before the transfer is treated as stalled.
/// This is a gap timeout, not a total-transfer timeout, so long downloads are
/// unaffected as long as bytes keep arriving.
const READ_STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// TCP connect timeout (DNS + handshake), independent of the header timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(not(target_os = "android"))]
pub fn new_http_client() -> Result<Client, DownloadError> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| DownloadError::InvalidResponse(error.to_string()))
}

#[cfg(target_os = "android")]
pub fn new_http_client() -> Result<Client, DownloadError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| DownloadError::InvalidResponse(error.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Client::builder()
        .tls_backend_preconfigured(config)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| DownloadError::InvalidResponse(error.to_string()))
}

pub async fn download_http<F>(
    client: &Client,
    url: impl Into<String>,
    destination: impl AsRef<Path>,
    cancellation: CancellationToken,
    on_progress: F,
) -> Result<DownloadOutcome, DownloadError>
where
    F: FnMut(DownloadProgress) + Send,
{
    let request = HttpDownloadRequest::new(url, destination.as_ref().to_path_buf());
    download_http_with_request(client, request, cancellation, on_progress).await
}

pub async fn download_http_with_request<F>(
    client: &Client,
    request: HttpDownloadRequest,
    cancellation: CancellationToken,
    on_progress: F,
) -> Result<DownloadOutcome, DownloadError>
where
    F: FnMut(DownloadProgress) + Send,
{
    download_http_inner(client, request, cancellation, on_progress).await
}

pub async fn download_http_with_options<F>(
    client: &Client,
    options: super::DownloadHttpOptions,
    cancellation: CancellationToken,
    on_progress: F,
) -> Result<DownloadOutcome, DownloadError>
where
    F: FnMut(DownloadProgress) + Send,
{
    download_http_inner(client, options, cancellation, on_progress).await
}

pub async fn download_http_for_job<F>(
    client: &Client,
    job: &DownloadJob,
    destination: impl Into<PathBuf>,
    cancellation: CancellationToken,
    on_progress: F,
) -> Result<DownloadOutcome, DownloadError>
where
    F: FnMut(DownloadProgress) + Send,
{
    let request = job.http_request(destination)?;
    download_http_inner(client, request, cancellation, on_progress).await
}

pub fn download_http_for_job_blocking<F>(
    client: &Client,
    job: &DownloadJob,
    destination: impl Into<PathBuf>,
    cancellation: CancellationToken,
    on_progress: F,
) -> Result<DownloadOutcome, DownloadError>
where
    F: FnMut(DownloadProgress) + Send,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| DownloadError::InvalidResponse(error.to_string()))?;
    runtime.block_on(download_http_for_job(
        client,
        job,
        destination,
        cancellation,
        on_progress,
    ))
}

pub async fn download_http_resume<F>(
    client: &Client,
    url: impl Into<String>,
    destination: impl AsRef<Path>,
    resume: DownloadResume,
    cancellation: CancellationToken,
    on_progress: F,
) -> Result<DownloadOutcome, DownloadError>
where
    F: FnMut(DownloadProgress) + Send,
{
    let request = HttpDownloadRequest::new(url, destination.as_ref().to_path_buf())
        .with_resume(resume);
    download_http_inner(client, request, cancellation, on_progress).await
}

async fn download_http_inner<F>(
    client: &Client,
    request: HttpDownloadRequest,
    cancellation: CancellationToken,
    on_progress: F,
) -> Result<DownloadOutcome, DownloadError>
where
    F: FnMut(DownloadProgress) + Send,
{
    download_http_inner_with_timeouts(
        client,
        request,
        cancellation,
        REQUEST_TIMEOUT,
        READ_STALL_TIMEOUT,
        on_progress,
    )
    .await
}

/// Core transfer loop. The timeouts are parameters so tests can drive the
/// stall detection without waiting a full minute.
async fn download_http_inner_with_timeouts<F>(
    client: &Client,
    request: HttpDownloadRequest,
    cancellation: CancellationToken,
    request_timeout: Duration,
    read_stall_timeout: Duration,
    mut on_progress: F,
) -> Result<DownloadOutcome, DownloadError>
where
    F: FnMut(DownloadProgress) + Send,
{
    if cancellation.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }
    let url = validate_download_url(&request.url)?;
    let destination = request.destination;
    if destination.as_os_str().is_empty() {
        return Err(DownloadError::InvalidResponse("empty destination path".into()));
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let part = part_path(&destination);
    let mut offset = part_size(&part).await?;
    let mut resume = request.resume.unwrap_or_default();
    if offset == 0 {
        resume = DownloadResume::default();
    } else if resume.bytes_downloaded > offset {
        reset_part(&part).await?;
        offset = 0;
        resume = DownloadResume::default();
    }

    let mut restart_count = 0u8;
    let (mut response, response_kind, total_bytes) = loop {
        if cancellation.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let response = tokio::time::timeout(
            request_timeout,
            send_request(client, &url, offset, &resume, &cancellation),
        )
        .await
        .map_err(|_| {
            DownloadError::InvalidResponse(format!(
                "request to {url} timed out after {}s",
                request_timeout.as_secs()
            ))
        })??;
        let final_url = response.url().to_string();
        if is_manifest_url(&final_url) {
            return Err(DownloadError::UnsupportedUrl(final_url));
        }
        if let Some(content_type) = header_string(response.headers(), "content-type")
            && is_manifest_content_type(&content_type)
        {
            return Err(DownloadError::UnsupportedContentType(content_type));
        }

        let status = response.status();
        if offset > 0 {
            match status {
                StatusCode::PARTIAL_CONTENT => {
                    let range = parse_content_range(header_string(response.headers(), "content-range").as_deref())
                        .map_err(DownloadError::InvalidResponse)?;
                    if range.start != offset {
                        return Err(DownloadError::InvalidResponse(format!(
                            "Content-Range starts at {}, expected {offset}",
                            range.start
                        )));
                    }
                    let etag = header_string(response.headers(), "etag");
                    let last_modified = header_string(response.headers(), "last-modified");
                    if validators_changed(&resume, &etag, &last_modified) {
                        if restart_count == 0 {
                            reset_part(&part).await?;
                            offset = 0;
                            resume = DownloadResume::default();
                            restart_count += 1;
                            continue;
                        }
                        return Err(DownloadError::InvalidResume(
                            "server returned a different representation after restart".into(),
                        ));
                    }
                    break (response, ResponseKind::Partial(range), range.total);
                }
                StatusCode::OK => {
                    reset_part(&part).await?;
                    offset = 0;
                    let content_length = response.content_length();
                    break (response, ResponseKind::Full, content_length);
                }
                StatusCode::RANGE_NOT_SATISFIABLE => {
                    let total = parse_unsatisfied_range(
                        header_string(response.headers(), "content-range").as_deref(),
                    );
                    let etag = header_string(response.headers(), "etag");
                    let last_modified = header_string(response.headers(), "last-modified");
                    if total == Some(offset) && !validators_changed(&resume, &etag, &last_modified) {
                        return finish_existing_part(
                            &part,
                            &destination,
                            offset,
                            Some(offset),
                            &etag,
                            &last_modified,
                            &cancellation,
                        )
                        .await;
                    }
                    if restart_count == 0 {
                        reset_part(&part).await?;
                        offset = 0;
                        resume = DownloadResume::default();
                        restart_count += 1;
                        continue;
                    }
                    return Err(DownloadError::InvalidResume(
                        "server cannot satisfy the requested range".into(),
                    ));
                }
                _ => {
                    return Err(DownloadError::HttpStatus {
                        status: status.as_u16(),
                        message: status.canonical_reason().unwrap_or("request failed").into(),
                    });
                }
            }
        } else {
            match status {
                StatusCode::OK => {
                    let content_length = response.content_length();
                    break (response, ResponseKind::Full, content_length);
                }
                StatusCode::PARTIAL_CONTENT => {
                    let range = parse_content_range(header_string(response.headers(), "content-range").as_deref())
                        .map_err(DownloadError::InvalidResponse)?;
                    if range.start != 0 {
                        return Err(DownloadError::InvalidResponse(format!(
                            "unsolicited Content-Range starts at {}",
                            range.start
                        )));
                    }
                    break (response, ResponseKind::Partial(range), range.total);
                }
                _ => {
                    return Err(DownloadError::HttpStatus {
                        status: status.as_u16(),
                        message: status.canonical_reason().unwrap_or("request failed").into(),
                    });
                }
            }
        }
    };

    if cancellation.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }
    let etag = header_string(response.headers(), ETAG.as_str());
    let last_modified = header_string(response.headers(), LAST_MODIFIED.as_str());
    let mut total_bytes = match response_kind {
        ResponseKind::Full => total_bytes,
        ResponseKind::Partial(range) => range.total.or_else(|| {
            response.content_length().map(|length| offset.saturating_add(length))
        }),
    };
    let expected_remaining = match response_kind {
        ResponseKind::Full => response.content_length(),
        ResponseKind::Partial(range) => range.end.checked_sub(range.start).and_then(|length| length.checked_add(1)),
    };
    if let Some(total) = total_bytes
        && offset > total
    {
        return Err(DownloadError::InvalidResume(format!(
            "partial file is {offset} bytes but response total is {total}"
        )));
    }

    fs::create_dir_all(parent).await?;
    let file_name = output_file_name(&destination, response.headers(), Some(response.url()));
    let mut file = open_part(&part, offset).await?;
    let started = Instant::now();
    let mut downloaded = offset;
    let mut bytes_this_run = 0u64;
    on_progress(DownloadProgress::new(downloaded, total_bytes, 0));

    loop {
        if cancellation.is_cancelled() {
            sync_after_error(&mut file).await;
            return Err(DownloadError::Cancelled);
        }
        let chunk_result = tokio::select! {
            biased;
            _ = cancellation.wait_cancelled() => {
                sync_after_error(&mut file).await;
                return Err(DownloadError::Cancelled);
            }
            // A socket that stops delivering data (server stalls, network
            // drop without RST) must not park the transfer forever. This is a
            // gap timeout between chunks, so an actively-transferring large
            // file is never cut off.
            result = tokio::time::timeout(read_stall_timeout, response.chunk()) => result,
        };
        let chunk = match chunk_result {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                sync_after_error(&mut file).await;
                return Err(DownloadError::InvalidResponse(format!(
                    "reading response body failed: {error}"
                )));
            }
            Err(_) => {
                sync_after_error(&mut file).await;
                return Err(DownloadError::InvalidResponse(format!(
                    "download stalled: no data for {}s",
                    read_stall_timeout.as_secs()
                )));
            }
        };
        if chunk.is_empty() {
            continue;
        }
        let next = downloaded.saturating_add(chunk.len() as u64);
        if let Some(expected) = expected_remaining
            && chunk.len() as u64 > expected.saturating_sub(bytes_this_run)
        {
            sync_after_error(&mut file).await;
            return Err(DownloadError::InvalidResponse(
                "response body is longer than Content-Range".into(),
            ));
        }
        if let Some(total) = total_bytes
            && next > total
        {
            sync_after_error(&mut file).await;
            return Err(DownloadError::InvalidResponse(
                "response body exceeds the advertised total size".into(),
            ));
        }
        if let Err(error) = file.write_all(&chunk).await {
            sync_after_error(&mut file).await;
            return Err(error.into());
        }
        downloaded = next;
        bytes_this_run = bytes_this_run.saturating_add(chunk.len() as u64);
        let elapsed = started.elapsed().as_secs_f64().max(0.001);
        on_progress(DownloadProgress::new(
            downloaded,
            total_bytes,
            (bytes_this_run as f64 / elapsed) as u64,
        ));
    }

    if let Some(expected) = expected_remaining
        && bytes_this_run != expected
    {
        sync_after_error(&mut file).await;
        return Err(DownloadError::InvalidResponse(format!(
            "response body ended after {bytes_this_run} of {expected} bytes"
        )));
    }
    if let Some(total) = total_bytes {
        if downloaded != total {
            sync_after_error(&mut file).await;
            return Err(DownloadError::InvalidResponse(format!(
                "download ended at {downloaded} of {total} bytes"
            )));
        }
        total_bytes = Some(total);
    }
    if cancellation.is_cancelled() {
        sync_after_error(&mut file).await;
        return Err(DownloadError::Cancelled);
    }
    if let Err(error) = file.flush().await {
        sync_after_error(&mut file).await;
        return Err(error.into());
    }
    if let Err(error) = file.sync_all().await {
        sync_after_error(&mut file).await;
        return Err(error.into());
    }
    drop(file);
    atomic_rename(&part, &destination).await?;
    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    let bytes_per_second = (bytes_this_run as f64 / elapsed) as u64;
    let outcome = DownloadOutcome {
        path: destination,
        part_path: part,
        file_name,
        bytes_downloaded: downloaded,
        total_bytes,
        bytes_per_second,
        etag,
        last_modified,
    };
    on_progress(DownloadProgress::new(
        outcome.bytes_downloaded,
        outcome.total_bytes,
        outcome.bytes_per_second,
    ));
    Ok(outcome)
}

#[derive(Clone, Copy)]
enum ResponseKind {
    Full,
    Partial(ContentRange),
}

#[derive(Clone, Copy)]
struct ContentRange {
    start: u64,
    end: u64,
    total: Option<u64>,
}

async fn send_request(
    client: &Client,
    url: &Url,
    offset: u64,
    resume: &DownloadResume,
    cancellation: &CancellationToken,
) -> Result<Response, DownloadError> {
    let mut request = client.get(url.clone()).header(ACCEPT_ENCODING, "identity");
    if offset > 0 {
        request = request.header(RANGE, format!("bytes={offset}-"));
        if let Some(validator) = resume.etag.as_deref().filter(|value| !value.is_empty()).or_else(|| {
            resume.last_modified.as_deref().filter(|value| !value.is_empty())
        }) {
            request = request.header(IF_RANGE, validator);
        }
    }
    tokio::select! {
        biased;
        _ = cancellation.wait_cancelled() => Err(DownloadError::Cancelled),
        result = request.send() => result.map_err(|error| {
            DownloadError::InvalidResponse(format!("request to {url} failed: {error}"))
        }),
    }
}

async fn part_size(path: &Path) -> Result<u64, DownloadError> {
    match fs::metadata(path).await {
        Ok(metadata) if metadata.is_file() => Ok(metadata.len()),
        Ok(_) => Err(DownloadError::InvalidResponse(format!(
            "{} is not a file",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

async fn reset_part(path: &Path) -> Result<(), DownloadError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .await?;
    file.set_len(0).await?;
    file.sync_all().await?;
    Ok(())
}

async fn open_part(path: &Path, offset: u64) -> Result<File, DownloadError> {
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .await?;
    file.set_len(offset).await?;
    file.seek(SeekFrom::Start(offset)).await?;
    Ok(file)
}

async fn sync_after_error(file: &mut File) {
    let _ = file.flush().await;
    let _ = file.sync_all().await;
}

async fn atomic_rename(part: &Path, destination: &Path) -> Result<(), DownloadError> {
    match fs::rename(part, destination).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::remove_file(destination).await?;
            fs::rename(part, destination).await?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

async fn finish_existing_part(
    part: &Path,
    destination: &Path,
    downloaded: u64,
    total: Option<u64>,
    etag: &Option<String>,
    last_modified: &Option<String>,
    cancellation: &CancellationToken,
) -> Result<DownloadOutcome, DownloadError> {
    if cancellation.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }
    let file = OpenOptions::new().read(true).write(true).open(part).await?;
    file.sync_all().await?;
    drop(file);
    atomic_rename(part, destination).await?;
    Ok(DownloadOutcome {
        path: destination.to_path_buf(),
        part_path: part.to_path_buf(),
        file_name: output_file_name(destination, &HeaderMap::new(), None),
        bytes_downloaded: downloaded,
        total_bytes: total,
        bytes_per_second: 0,
        etag: etag.clone(),
        last_modified: last_modified.clone(),
    })
}

fn validators_changed(
    resume: &DownloadResume,
    etag: &Option<String>,
    last_modified: &Option<String>,
) -> bool {
    resume
        .etag
        .as_deref()
        .is_some_and(|value| Some(value) != etag.as_deref())
        || resume
            .last_modified
            .as_deref()
            .is_some_and(|value| Some(value) != last_modified.as_deref())
}

fn parse_content_range(value: Option<&str>) -> Result<ContentRange, String> {
    let value = value.ok_or_else(|| "206 response has no Content-Range".to_string())?;
    let value = value.trim();
    let value = value
        .strip_prefix("bytes ")
        .ok_or_else(|| format!("unsupported Content-Range unit: {value}"))?;
    let (range, total) = value
        .split_once('/')
        .ok_or_else(|| format!("malformed Content-Range: {value}"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| format!("malformed Content-Range: {value}"))?;
    let start = start
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("malformed Content-Range start: {start}"))?;
    let end = end
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("malformed Content-Range end: {end}"))?;
    if end < start {
        return Err(format!("Content-Range end {end} precedes start {start}"));
    }
    let total = if total.trim() == "*" {
        None
    } else {
        let total = total
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("malformed Content-Range total: {total}"))?;
        if end >= total {
            return Err(format!("Content-Range end {end} exceeds total {total}"));
        }
        Some(total)
    };
    Ok(ContentRange { start, end, total })
}

fn parse_unsatisfied_range(value: Option<&str>) -> Option<u64> {
    let value = value?.trim();
    let value = value.strip_prefix("bytes ")?;
    let total = value.strip_prefix("*/")?.trim();
    total.parse().ok()
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn output_file_name(destination: &Path, headers: &HeaderMap, url: Option<&Url>) -> String {
    let disposition = headers
        .get("content-disposition")
        .and_then(|value| value.to_str().ok())
        .and_then(content_disposition_filename);
    let from_url = url
        .and_then(|url| url.path_segments())
        .and_then(|segments| segments.filter(|segment| !segment.is_empty()).last())
        .map(|segment| percent_decode(segment));
    destination
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_filename)
        .filter(|name| name != "download")
        .or_else(|| disposition.map(|name| sanitize_filename(&name)))
        .or_else(|| from_url.map(|name| sanitize_filename(&name)))
        .unwrap_or_else(|| "download".into())
}

fn content_disposition_filename(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    if let Some(index) = lower.find("filename*=") {
        let value = value[index + 10..].split(';').next()?.trim();
        let value = value.trim_matches('"');
        let value = value.rsplit_once("''").map(|(_, value)| value).unwrap_or(value);
        return Some(percent_decode(value));
    }
    let index = lower.find("filename=")?;
    let value = value[index + 9..].split(';').next()?.trim();
    Some(value.trim_matches('"').to_string())
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                output.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

pub fn is_manifest_content_type(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    matches!(
        essence.as_str(),
        "application/vnd.apple.mpegurl"
            | "application/x-mpegurl"
            | "audio/mpegurl"
            | "audio/x-mpegurl"
            | "text/vnd.apple.mpegurl"
            | "application/dash+xml"
            | "application/vnd.ms-playready.media+xml"
            | "application/manifest+json"
            | "application/manifest+xml"
            | "text/manifest"
            | "application/vnd.youtube"
            | "application/x-youtube"
            | "video/mp2t"
    )
}

pub fn is_manifest_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let parsed = Url::parse(url).ok();
    let host = parsed
        .as_ref()
        .and_then(|url| url.host_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if host == "youtu.be"
        || host == "youtube.com"
        || host.ends_with(".youtube.com")
        || host == "youtube-nocookie.com"
        || host.ends_with(".youtube-nocookie.com")
    {
        return true;
    }
    let path = parsed
        .as_ref()
        .map(|url| url.path().to_ascii_lowercase())
        .unwrap_or_else(|| lower.clone());
    let path = percent_decode(&path);
    let query = parsed
        .as_ref()
        .map(|url| url.query().unwrap_or_default().to_ascii_lowercase())
        .unwrap_or_default();
    if [
        ".m3u8", ".m3u", ".mpd", ".ism", ".isml", ".f4m", ".m3u8.txt",
    ]
    .iter()
    .any(|extension| path.ends_with(extension) || query.contains(extension))
    {
        return true;
    }
    path.split('/').any(|segment| {
        matches!(
            segment,
            "manifest"
                | "manifest.json"
                | "playlist"
                | "master"
                | "master.m3u8"
                | "hls"
                | "dash"
        )
    }) || query.contains("manifest")
        || query.contains("playlist")
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::{format_bytes, sanitize_filename};

    fn test_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("nova-download-{name}-{}", std::process::id()));
        path
    }

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&request).into_owned()
    }

    async fn one_shot_server(response: &'static [u8]) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            socket.write_all(response).await.unwrap();
            socket.flush().await.unwrap();
        });
        (format!("http://{address}/file.mp4"), task)
    }

    #[tokio::test]
    async fn rejects_manifest_urls_without_creating_a_file() {
        let path = test_path("manifest-url");
        let _ = tokio::fs::remove_file(&path).await;
        let client = Client::new();
        let error = download_http(
            &client,
            "http://127.0.0.1:1/master.m3u8",
            &path,
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DownloadError::UnsupportedUrl(_)));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn rejects_manifest_content_types() {
        let path = test_path("manifest-type");
        let _ = tokio::fs::remove_file(&path).await;
        let (url, task) = one_shot_server(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: 7\r\n\r\n#EXTM3U\n",
        )
        .await;
        let client = Client::new();
        let error = download_http(
            &client,
            url,
            &path,
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap_err();
        task.await.unwrap();
        assert!(matches!(error, DownloadError::UnsupportedContentType(_)));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn resumes_with_range_and_validates_content_range() {
        let path = test_path("resume");
        let part = part_path(&path);
        tokio::fs::write(&part, b"abc").await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request);
            assert!(text.to_ascii_lowercase().contains("range: bytes=3-"));
            assert!(text.to_ascii_lowercase().contains("if-range: \"v1\""));
            socket
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\nETag: \"v1\"\r\n\r\ndef",
                )
                .await
                .unwrap();
            socket.flush().await.unwrap();
        });
        let client = Client::new();
        let outcome = download_http_resume(
            &client,
            format!("http://{address}/file.mp4"),
            &path,
            DownloadResume::new(3, Some("\"v1\"".into()), None),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
        task.await.unwrap();
        assert_eq!(outcome.bytes_downloaded, 6);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"abcdef");
        assert!(!part.exists());
    }

    #[tokio::test]
    async fn restarts_when_a_range_validator_changes() {
        let path = test_path("validator-restart");
        let part = part_path(&path);
        tokio::fs::write(&part, b"old").await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let first_request = read_request(&mut first).await.to_ascii_lowercase();
            assert!(first_request.contains("range: bytes=3-"));
            first
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\nETag: \"new\"\r\nConnection: close\r\n\r\nnew",
                )
                .await
                .unwrap();
            first.flush().await.unwrap();

            let (mut second, _) = listener.accept().await.unwrap();
            let second_request = read_request(&mut second).await.to_ascii_lowercase();
            assert!(!second_request.contains("range:"));
            second
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nETag: \"new\"\r\nConnection: close\r\n\r\nabcdef",
                )
                .await
                .unwrap();
            second.flush().await.unwrap();
        });
        let client = Client::new();
        let outcome = download_http_resume(
            &client,
            format!("http://{address}/file.mp4"),
            &path,
            DownloadResume::new(3, Some("\"old\"".into()), None),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
        task.await.unwrap();
        assert_eq!(outcome.bytes_downloaded, 6);
        assert_eq!(outcome.etag.as_deref(), Some("\"new\""));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"abcdef");
        assert!(!part.exists());
    }

    #[tokio::test]
    async fn stalls_when_the_server_never_responds() {
        // The server accepts the connection, reads the request, then never
        // writes a response. Without a gap timeout this parks the transfer
        // (and the single download slot) forever.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let path = test_path("stall");
        let _ = tokio::fs::remove_file(&path).await;
        let client = Client::new();
        let error = download_http_inner_with_timeouts(
            &client,
            HttpDownloadRequest::new(format!("http://{address}/file.mp4"), &path),
            CancellationToken::new(),
            Duration::from_millis(200),
            Duration::from_millis(200),
            |_| {},
        )
        .await
        .unwrap_err();
        task.abort();
        assert!(matches!(error, DownloadError::InvalidResponse(_)));
        assert!(!path.exists());
    }

    #[test]
    fn manifest_detection_handles_urls_and_types() {
        assert!(is_manifest_url("https://youtu.be/abc"));
        assert!(is_manifest_url("https://cdn.example/x/master.m3u8"));
        assert!(is_manifest_url("https://cdn.example/video?manifest=1"));
        assert!(is_manifest_content_type("application/dash+xml; charset=utf-8"));
        assert!(!is_manifest_content_type("video/mp4"));
        assert_eq!(sanitize_filename("a/b:c?.mp4"), "b_c_.mp4");
        assert_eq!(format_bytes(1024), "1.0 KiB");
    }
}
