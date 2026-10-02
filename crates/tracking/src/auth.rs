//! Native public-client OAuth. No embedded client secrets or disk tokens.
use crate::{
    ApiError, Secret, Service,
    api::{Body, Request, Transport, checked},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRegistration {
    pub client_id: String,
    pub redirect_uri: String,
}
impl ClientRegistration {
    pub fn validate(&self, service: Service) -> Result<(), ApiError> {
        if self.client_id.is_empty()
            || self.client_id.len() > 128
            || !self
                .client_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(ApiError::InvalidInput);
        }
        let url = url::Url::parse(&self.redirect_uri).map_err(|_| ApiError::InvalidInput)?;
        let pin = service == Service::AniList
            && self.redirect_uri == "https://anilist.co/api/v2/oauth/pin";
        let loopback = url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some_and(|port| port != 0);
        if (!pin && !loopback)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ApiError::InvalidInput);
        }
        Ok(())
    }
}
pub struct Authorization {
    pub service: Service,
    pub registration: ClientRegistration,
    state: Secret,
    verifier: Option<Secret>,
}
impl Authorization {
    pub fn begin(service: Service, registration: ClientRegistration) -> Result<Self, ApiError> {
        registration.validate(service)?;
        Ok(Self {
            service,
            registration,
            state: random_secret()?,
            verifier: if service == Service::MyAnimeList {
                Some(random_secret()?)
            } else {
                None
            },
        })
    }
    pub fn url(&self) -> Result<String, ApiError> {
        let endpoint = match self.service {
            Service::MyAnimeList => "https://myanimelist.net/v1/oauth2/authorize",
            Service::AniList => "https://anilist.co/api/v2/oauth/authorize",
        };
        let mut url = url::Url::parse(endpoint).map_err(|_| ApiError::InvalidInput)?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.registration.client_id)
            .append_pair("redirect_uri", &self.registration.redirect_uri)
            .append_pair("state", self.state.expose())
            .append_pair(
                "response_type",
                if self.service == Service::MyAnimeList {
                    "code"
                } else {
                    "token"
                },
            );
        if let Some(verifier) = &self.verifier {
            url.query_pairs_mut()
                .append_pair("code_challenge", verifier.expose())
                .append_pair("code_challenge_method", "plain");
        }
        Ok(url.into())
    }
    /// Full callback URL pasted by the user or captured by the loopback listener.
    /// Consume this session once; cancellation must discard the Authorization.
    pub fn finish<T: Transport>(
        self,
        callback: &str,
        transport: &T,
        now: u64,
    ) -> Result<Tokens, ApiError> {
        let received = url::Url::parse(callback).map_err(|_| ApiError::InvalidInput)?;
        let redirect =
            url::Url::parse(&self.registration.redirect_uri).map_err(|_| ApiError::InvalidInput)?;
        if received.scheme() != redirect.scheme()
            || received.host_str() != redirect.host_str()
            || received.port_or_known_default() != redirect.port_or_known_default()
            || received.path() != redirect.path()
            || !received.username().is_empty()
            || received.password().is_some()
        {
            return Err(ApiError::InvalidInput);
        }
        let parameters = match self.service {
            Service::MyAnimeList => received.query().unwrap_or_default(),
            Service::AniList => received.fragment().unwrap_or_default(),
        };
        let mut state = None;
        let mut credential = None;
        let mut expires = None;
        for (key, value) in url::form_urlencoded::parse(parameters.as_bytes()) {
            match key.as_ref() {
                "state" => {
                    if state.replace(value.into_owned()).is_some() {
                        return Err(ApiError::InvalidInput);
                    }
                }
                "code" if self.service == Service::MyAnimeList => {
                    if credential.replace(value.into_owned()).is_some() {
                        return Err(ApiError::InvalidInput);
                    }
                }
                "access_token" if self.service == Service::AniList => {
                    if credential.replace(value.into_owned()).is_some() {
                        return Err(ApiError::InvalidInput);
                    }
                }
                "expires_in" => {
                    expires = value.parse::<u64>().ok();
                }
                "error" => return Err(ApiError::Authentication),
                _ => {}
            }
        }
        if !state.is_some_and(|s| constant_time_equal(s.as_bytes(), self.state.expose().as_bytes()))
        {
            return Err(ApiError::Authentication);
        }
        let credential = Secret::new(credential.ok_or(ApiError::Authentication)?)?;
        match self.service {
            Service::AniList => Ok(Tokens {
                access: credential,
                refresh: None,
                expires_at: expires.map(|s| now.saturating_add(s)),
            }),
            Service::MyAnimeList => {
                let form = vec![
                    ("grant_type".into(), "authorization_code".into()),
                    ("client_id".into(), self.registration.client_id.clone()),
                    ("code".into(), credential.expose().to_owned()),
                    (
                        "redirect_uri".into(),
                        self.registration.redirect_uri.clone(),
                    ),
                    (
                        "code_verifier".into(),
                        self.verifier
                            .as_ref()
                            .ok_or(ApiError::InvalidInput)?
                            .expose()
                            .to_owned(),
                    ),
                ];
                exchange(transport, form, now)
            }
        }
    }
    /// AniList's documented PIN page returns a token without callback state.
    /// Accept it only as an explicit user action in this pending PIN session;
    /// the app must still verify Viewer before activating the account.
    pub fn finish_pin(self, token: String) -> Result<Tokens, ApiError> {
        if self.service != Service::AniList
            || self.registration.redirect_uri != "https://anilist.co/api/v2/oauth/pin"
        {
            return Err(ApiError::InvalidInput);
        }
        Ok(Tokens {
            access: Secret::new(token)?,
            refresh: None,
            expires_at: None,
        })
    }
}
pub struct Tokens {
    pub access: Secret,
    pub refresh: Option<Secret>,
    pub expires_at: Option<u64>,
}
impl Tokens {
    pub fn refresh<T: Transport>(
        &self,
        registration: &ClientRegistration,
        transport: &T,
        now: u64,
    ) -> Result<Self, ApiError> {
        registration.validate(Service::MyAnimeList)?;
        let refresh = self.refresh.as_ref().ok_or(ApiError::Authentication)?;
        exchange(
            transport,
            vec![
                ("grant_type".into(), "refresh_token".into()),
                ("client_id".into(), registration.client_id.clone()),
                ("refresh_token".into(), refresh.expose().to_owned()),
            ],
            now,
        )
    }
}
fn exchange<T: Transport>(
    transport: &T,
    form: Vec<(String, String)>,
    now: u64,
) -> Result<Tokens, ApiError> {
    let mut value = checked(
        transport.send(Request {
            method: "POST",
            url: "https://myanimelist.net/v1/oauth2/token".into(),
            bearer: None,
            body: Body::Form(form),
        })?,
        now,
    )?;
    let access = value["access_token"]
        .take()
        .as_str()
        .map(str::to_owned)
        .ok_or(ApiError::InvalidResponse)?;
    let refresh = value["refresh_token"]
        .take()
        .as_str()
        .map(str::to_owned)
        .ok_or(ApiError::InvalidResponse)?;
    if value["token_type"]
        .as_str()
        .is_none_or(|v| !v.eq_ignore_ascii_case("bearer"))
    {
        return Err(ApiError::InvalidResponse);
    }
    let expires = value["expires_in"]
        .as_u64()
        .filter(|v| *v > 0)
        .ok_or(ApiError::InvalidResponse)?;
    Ok(Tokens {
        access: Secret::new(access)?,
        refresh: Some(Secret::new(refresh)?),
        expires_at: Some(now.saturating_add(expires)),
    })
}
fn random_secret() -> Result<Secret, ApiError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| ApiError::Offline)?;
    let text: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    use zeroize::Zeroize;
    bytes.zeroize();
    Secret::new(text)
}
fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b)
        .fold(0, |difference, (a, b)| difference | (a ^ b))
        == 0
}

/// A return URL or PIN pasted by the user. Redacted and zeroized on drop.
pub struct AuthReturn(zeroize::Zeroizing<String>);
impl AuthReturn {
    pub fn new(value: String) -> Result<Self, ApiError> {
        if value.len() > 32768 || value.trim().is_empty() {
            return Err(ApiError::InvalidInput);
        }
        Ok(Self(zeroize::Zeroizing::new(value)))
    }
}
impl Authorization {
    pub fn finish_return<T: Transport>(
        self,
        value: AuthReturn,
        transport: &T,
        now: u64,
    ) -> Result<Tokens, ApiError> {
        if self.registration.redirect_uri == "https://anilist.co/api/v2/oauth/pin" {
            self.finish_pin(value.0.trim().to_owned())
        } else {
            self.finish(value.0.trim(), transport, now)
        }
    }
}
