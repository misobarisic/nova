//! A small, bounded loopback receiver shared by desktop and Android browser
//! authorization. AniList's fragment is posted by a local page and never sent
//! to a third-party host. The OAuth layer still verifies the returned state.
use crate::{ApiError, Service, auth::ClientRegistration};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
pub struct CallbackReceiver {
    listener: TcpListener,
    redirect: url::Url,
    service: Service,
}
impl CallbackReceiver {
    pub fn bind(service: Service, registration: &ClientRegistration) -> Result<Self, ApiError> {
        registration.validate(service)?;
        let redirect =
            url::Url::parse(&registration.redirect_uri).map_err(|_| ApiError::InvalidInput)?;
        if redirect.scheme() != "http" {
            return Err(ApiError::InvalidInput);
        }
        let listener =
            TcpListener::bind(("127.0.0.1", redirect.port().ok_or(ApiError::InvalidInput)?))
                .map_err(|_| ApiError::Offline)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| ApiError::Offline)?;
        Ok(Self {
            listener,
            redirect,
            service,
        })
    }
    /// On cancellation the caller drops the pending authorization; a late
    /// captured callback must carry its original login generation to the app.
    pub fn receive(
        self,
        cancel: Arc<AtomicBool>,
        timeout: Duration,
    ) -> Result<zeroize::Zeroizing<String>, ApiError> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && !cancel.load(Ordering::Acquire) {
            match self.listener.accept() {
                Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                    // A stray or malformed local request must not consume the
                    // authorization session. State is verified after capture.
                    if let Ok(Some(value)) = self.read_callback(&mut stream) {
                        return Ok(zeroize::Zeroizing::new(value));
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(_) => return Err(ApiError::Offline),
            }
        }
        Err(ApiError::Authentication)
    }
    fn read_callback(&self, stream: &mut TcpStream) -> Result<Option<String>, ApiError> {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| ApiError::Offline)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| ApiError::Offline)?;
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        let mut buffer = [0u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut buffer).map_err(|_| ApiError::Offline)?;
            if read == 0 || bytes.len() + read > 32768 {
                return Err(ApiError::InvalidInput);
            }
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(index) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let header =
            std::str::from_utf8(&bytes[..header_end]).map_err(|_| ApiError::InvalidInput)?;
        let first = header.lines().next().ok_or(ApiError::InvalidInput)?;
        let parts: Vec<_> = first.split_ascii_whitespace().collect();
        if parts.len() != 3 {
            return Err(ApiError::InvalidInput);
        }
        let method = parts[0].to_owned();
        let path = parts[1].to_owned();
        let host = header.lines().find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case("host"))
                .map(|(_, value)| value.trim())
        });
        let expected_host = format!(
            "127.0.0.1:{}",
            self.redirect.port().ok_or(ApiError::InvalidInput)?
        );
        if host != Some(&expected_host) {
            respond(stream, 400, "Invalid callback")?;
            return Ok(None);
        }
        let length = header
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if length > 16384 {
            return Err(ApiError::InvalidInput);
        }
        while bytes.len() < header_end + length {
            let read = stream.read(&mut buffer).map_err(|_| ApiError::Offline)?;
            if read == 0 {
                return Err(ApiError::InvalidInput);
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        if method == "GET" && path.split('?').next() == Some(self.redirect.path()) {
            if self.service == Service::MyAnimeList {
                respond(
                    stream,
                    200,
                    "<!doctype html><title>Nova</title><p>You can return to Nova.</p>",
                )?;
                let mut callback = self.redirect.clone();
                callback.set_query(path.split_once('?').map(|(_, query)| query));
                return Ok(Some(callback.into()));
            }
            // Same-origin POST retains the fragment, including OAuth state.
            respond(
                stream,
                200,
                "<!doctype html><meta name=referrer content=no-referrer><title>Nova</title><p>You can return to Nova.</p><script>fetch('/nova-oauth-result',{method:'POST',headers:{'Content-Type':'text/plain'},body:location.hash.slice(1)});history.replaceState(null,'',location.pathname)</script>",
            )?;
            return Ok(None);
        }
        if self.service == Service::AniList && method == "POST" && path == "/nova-oauth-result" {
            let fragment = std::str::from_utf8(&bytes[header_end..header_end + length])
                .map_err(|_| ApiError::InvalidInput)?;
            let mut callback = self.redirect.clone();
            callback.set_fragment(Some(fragment));
            respond(stream, 200, "OK")?;
            return Ok(Some(callback.into()));
        }
        respond(stream, 404, "Not found")?;
        Ok(None)
    }
}
fn respond(stream: &mut TcpStream, status: u16, body: &str) -> Result<(), ApiError> {
    write!(stream,"HTTP/1.1 {status} OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",body.len()).map_err(|_| ApiError::Offline)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn receiver(service: Service) -> CallbackReceiver {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let redirect = url::Url::parse(&format!(
            "http://127.0.0.1:{}/callback",
            listener.local_addr().unwrap().port()
        ))
        .unwrap();
        CallbackReceiver {
            listener,
            redirect,
            service,
        }
    }
    fn send(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }
    #[test]
    fn malformed_requests_do_not_consume_the_mal_callback() {
        let receiver = receiver(Service::MyAnimeList);
        let port = receiver.redirect.port().unwrap();
        let task = std::thread::spawn(move || {
            receiver
                .receive(Arc::new(AtomicBool::new(false)), Duration::from_secs(5))
                .unwrap()
        });
        send(port, "invalid\r\n\r\n");
        assert!(
            send(
                port,
                "GET /callback?code=wrong HTTP/1.1\r\nHost: attacker.invalid\r\n\r\n"
            )
            .starts_with("HTTP/1.1 400")
        );
        send(
            port,
            &format!(
                "GET /callback?code=sample&state=sample HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
            ),
        );
        assert!(task.join().unwrap().ends_with("?code=sample&state=sample"));
    }
    #[test]
    fn anilist_fragment_stays_in_the_local_callback() {
        let receiver = receiver(Service::AniList);
        let port = receiver.redirect.port().unwrap();
        let task = std::thread::spawn(move || {
            receiver
                .receive(Arc::new(AtomicBool::new(false)), Duration::from_secs(5))
                .unwrap()
        });
        let page = send(
            port,
            &format!("GET /callback HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
        );
        assert!(page.contains("no-referrer"));
        assert!(page.contains("location.hash"));
        let body = "access_token=sample&state=sample&expires_in=300";
        send(
            port,
            &format!(
                "POST /nova-oauth-result HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        );
        assert!(task.join().unwrap().ends_with(&format!("#{body}")));
    }
    #[test]
    fn cancelled_receiver_releases_the_port() {
        let receiver = receiver(Service::AniList);
        let port = receiver.redirect.port().unwrap();
        assert!(matches!(
            receiver.receive(Arc::new(AtomicBool::new(true)), Duration::from_secs(5)),
            Err(ApiError::Authentication)
        ));
        TcpListener::bind(("127.0.0.1", port)).unwrap();
    }
}
