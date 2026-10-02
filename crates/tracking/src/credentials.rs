//! Device-local plaintext credentials, separate from public registrations/state.
//! Replace this backend with protected platform storage without changing the
//! account verification and refresh lifecycle. Never include these keys in sync.
use crate::{
    AccountKey, ApiError, Secret, Service, StateStorage,
    api::{Client, Transport},
    auth::{ClientRegistration, Tokens},
};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

pub const CREDENTIAL_KEYS: [&str; 2] = [
    "tracking:credentials:mal:v1",
    "tracking:credentials:anilist:v1",
];
pub fn credential_key(service: Service) -> &'static str {
    CREDENTIAL_KEYS[match service {
        Service::MyAnimeList => 0,
        Service::AniList => 1,
    }]
}
#[derive(Debug, PartialEq, Eq)]
pub enum CredentialError {
    Storage,
    Invalid,
    Api(ApiError),
}
// Intentionally no Debug/Clone: a connection contains live bearer credentials.
pub struct SavedConnection {
    pub account: AccountKey,
    pub registration: ClientRegistration,
    pub tokens: Tokens,
}
#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    account: AccountKey,
    registration: ClientRegistration,
    access: String,
    refresh: Option<String>,
    expires_at: Option<u64>,
}
impl Drop for Record {
    fn drop(&mut self) {
        self.access.zeroize();
        self.refresh.zeroize();
    }
}
impl SavedConnection {
    pub(crate) fn encode(&self) -> Result<String, CredentialError> {
        self.registration
            .validate(self.account.service)
            .map_err(|_| CredentialError::Invalid)?;
        serde_json::to_string(&Record {
            version: 1,
            account: self.account.clone(),
            registration: self.registration.clone(),
            access: self.tokens.access.expose().into(),
            refresh: self.tokens.refresh.as_ref().map(|s| s.expose().into()),
            expires_at: self.tokens.expires_at,
        })
        .map_err(|_| CredentialError::Invalid)
    }
    pub fn load(
        storage: &impl StateStorage,
        service: Service,
    ) -> Result<Option<Self>, CredentialError> {
        let Some(raw) = storage
            .read(credential_key(service))
            .map_err(|_| CredentialError::Storage)?
        else {
            return Ok(None);
        };
        let raw = Zeroizing::new(raw);
        if raw.len() > 65536 {
            return Err(CredentialError::Invalid);
        }
        let mut record: Record =
            serde_json::from_str(&raw).map_err(|_| CredentialError::Invalid)?;
        if record.version != 1 || record.account.service != service {
            return Err(CredentialError::Invalid);
        }
        record
            .registration
            .validate(service)
            .map_err(|_| CredentialError::Invalid)?;
        Ok(Some(Self {
            account: record.account.clone(),
            registration: record.registration.clone(),
            tokens: Tokens {
                access: Secret::new(std::mem::take(&mut record.access))
                    .map_err(|_| CredentialError::Invalid)?,
                refresh: record
                    .refresh
                    .take()
                    .map(Secret::new)
                    .transpose()
                    .map_err(|_| CredentialError::Invalid)?,
                expires_at: record.expires_at,
            },
        }))
    }
    pub fn save(&self, storage: &impl StateStorage) -> Result<(), CredentialError> {
        let mut entries = [(
            credential_key(self.account.service).into(),
            Some(self.encode()?),
        )];
        let result = storage
            .write(&entries)
            .map_err(|_| CredentialError::Storage);
        entries[0].1.zeroize();
        result
    }
    /// Refresh rotation is saved before verification, including when a subsequent
    /// viewer request goes offline. Otherwise a consumed refresh token is lost.
    pub fn restore<T: Transport + Clone>(
        &mut self,
        storage: &impl StateStorage,
        transport: T,
        now: u64,
    ) -> Result<Client<T>, CredentialError> {
        // A retry may hold rotated tokens whose previous save failed.
        self.save(storage)?;
        let service = self.account.service;
        let expired = self
            .tokens
            .expires_at
            .is_some_and(|expiry| expiry <= now.saturating_add(60));
        if expired {
            if service != Service::MyAnimeList {
                return Err(CredentialError::Api(ApiError::Authentication));
            }
            self.tokens = self
                .tokens
                .refresh(&self.registration, &transport, now)
                .map_err(CredentialError::Api)?;
            self.save(storage)?;
        }
        let mut client = Client::new(service, self.tokens.access.duplicate(), transport.clone());
        let mut verified = client.verify(now);
        if !expired
            && service == Service::MyAnimeList
            && matches!(verified, Err(ApiError::Authentication))
            && self.tokens.refresh.is_some()
        {
            self.tokens = self
                .tokens
                .refresh(&self.registration, &transport, now)
                .map_err(CredentialError::Api)?;
            self.save(storage)?;
            client = Client::new(service, self.tokens.access.duplicate(), transport);
            verified = client.verify(now);
        }
        let viewer = verified.map_err(CredentialError::Api)?;
        if viewer.account != self.account {
            return Err(CredentialError::Api(ApiError::WrongAccount));
        }
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Account, Store, TrackingState,
        api::{Request, Response},
    };
    use serde_json::json;
    use std::{
        collections::HashMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };
    #[derive(Clone, Default)]
    struct Memory {
        rows: Arc<Mutex<HashMap<String, String>>>,
        fail: Arc<AtomicBool>,
    }
    impl StateStorage for Memory {
        fn read(&self, key: &str) -> Result<Option<String>, nova_storage::Error> {
            Ok(self.rows.lock().unwrap().get(key).cloned())
        }
        fn write(&self, entries: &[(String, Option<String>)]) -> Result<(), nova_storage::Error> {
            if self.fail.load(Ordering::Relaxed) {
                return Err(nova_storage::Error::new(
                    nova_storage::ErrorKind::Transaction,
                    "fixture",
                ));
            }
            let mut rows = self.rows.lock().unwrap();
            for (key, value) in entries {
                if let Some(value) = value {
                    rows.insert(key.clone(), value.clone());
                } else {
                    rows.remove(key);
                }
            }
            Ok(())
        }
    }
    #[derive(Clone)]
    struct Replies(Arc<Mutex<Vec<Response>>>);
    impl Transport for Replies {
        fn send(&self, _: Request) -> Result<Response, ApiError> {
            self.0.lock().unwrap().pop().ok_or(ApiError::Offline)
        }
    }
    fn replies(values: Vec<serde_json::Value>) -> Replies {
        Replies(Arc::new(Mutex::new(
            values
                .into_iter()
                .rev()
                .map(|body| Response {
                    status: 200,
                    retry_after: None,
                    reset_at: None,
                    body,
                })
                .collect(),
        )))
    }
    fn connection(service: Service) -> SavedConnection {
        SavedConnection {
            account: AccountKey {
                service,
                remote_user_id: 7.try_into().unwrap(),
            },
            registration: ClientRegistration {
                client_id: "fixture".into(),
                redirect_uri: if service == Service::MyAnimeList {
                    "http://127.0.0.1:53926/callback"
                } else {
                    "https://anilist.co/api/v2/oauth/pin"
                }
                .into(),
            },
            tokens: Tokens {
                access: Secret::new("fixture-access".into()).unwrap(),
                refresh: Some(Secret::new("fixture-refresh".into()).unwrap()),
                expires_at: Some(100),
            },
        }
    }
    #[test]
    fn restart_restores_both_services_and_verifies_the_saved_identity() {
        let storage = Memory::default();
        for service in [Service::MyAnimeList, Service::AniList] {
            let mut saved = connection(service);
            saved.tokens.expires_at = Some(10000);
            saved.save(&storage).unwrap();
            let mut loaded = SavedConnection::load(&storage, service).unwrap().unwrap();
            let viewer = if service == Service::MyAnimeList {
                json!({"id":7,"name":"fixture"})
            } else {
                json!({"data":{"Viewer":{"id":7,"name":"fixture","mediaListOptions":{"scoreFormat":"POINT_10"}}}})
            };
            let client = loaded
                .restore(&storage, replies(vec![viewer]), 100)
                .unwrap();
            assert_eq!(client.viewer().unwrap().account, saved.account);
            assert_eq!(loaded.tokens.access.expose(), "fixture-access");
        }
    }
    #[test]
    fn expired_mal_refresh_is_saved_even_if_viewer_verification_goes_offline() {
        let storage = Memory::default();
        let mut saved = connection(Service::MyAnimeList);
        saved.save(&storage).unwrap();
        let refresh = json!({"token_type":"Bearer","access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":3600});
        assert!(matches!(
            saved.restore(&storage, replies(vec![refresh]), 100),
            Err(CredentialError::Api(ApiError::Offline))
        ));
        let mut restarted = SavedConnection::load(&storage, Service::MyAnimeList)
            .unwrap()
            .unwrap();
        assert_eq!(
            restarted.tokens.refresh.as_ref().unwrap().expose(),
            "rotated-refresh"
        );
        assert_eq!(restarted.tokens.expires_at, Some(3700));
        assert!(
            restarted
                .restore(
                    &storage,
                    replies(vec![json!({"id":7,"name":"fixture"})]),
                    101
                )
                .is_ok()
        );
    }
    #[test]
    fn rotated_tokens_survive_a_failed_save_and_are_persisted_on_retry() {
        #[derive(Clone)]
        struct FailAfterRefresh(Memory);
        impl Transport for FailAfterRefresh {
            fn send(&self, _: Request) -> Result<Response, ApiError> {
                self.0.fail.store(true, Ordering::Relaxed);
                Ok(Response {
                    status: 200,
                    retry_after: None,
                    reset_at: None,
                    body: json!({"token_type":"Bearer","access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":3600}),
                })
            }
        }
        let storage = Memory::default();
        let mut saved = connection(Service::MyAnimeList);
        saved.save(&storage).unwrap();
        assert!(matches!(
            saved.restore(&storage, FailAfterRefresh(storage.clone()), 100),
            Err(CredentialError::Storage)
        ));
        assert_eq!(
            saved.tokens.refresh.as_ref().unwrap().expose(),
            "rotated-refresh"
        );
        storage.fail.store(false, Ordering::Relaxed);
        saved
            .restore(
                &storage,
                replies(vec![json!({"id":7,"name":"fixture"})]),
                101,
            )
            .unwrap();
        assert_eq!(
            SavedConnection::load(&storage, Service::MyAnimeList)
                .unwrap()
                .unwrap()
                .tokens
                .refresh
                .as_ref()
                .unwrap()
                .expose(),
            "rotated-refresh"
        );
    }
    #[test]
    fn revoked_mal_access_refreshes_once_and_wrong_account_cannot_be_restored() {
        let storage = Memory::default();
        let mut saved = connection(Service::MyAnimeList);
        saved.tokens.expires_at = Some(10000);
        let responses = replies(vec![
            json!({}),
            json!({"token_type":"Bearer","access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":3600}),
            json!({"id":8,"name":"other"}),
        ]);
        responses.0.lock().unwrap().last_mut().unwrap().status = 401;
        assert!(matches!(
            saved.restore(&storage, responses, 100),
            Err(CredentialError::Api(ApiError::WrongAccount))
        ));
        assert_eq!(
            SavedConnection::load(&storage, Service::MyAnimeList)
                .unwrap()
                .unwrap()
                .account
                .remote_user_id
                .get(),
            7
        );
    }
    #[test]
    fn expired_anilist_and_malformed_or_misplaced_records_require_reconnection() {
        let storage = Memory::default();
        let mut saved = connection(Service::AniList);
        saved.save(&storage).unwrap();
        assert!(matches!(
            saved.restore(&storage, replies(vec![]), 100),
            Err(CredentialError::Api(ApiError::Authentication))
        ));
        storage
            .write(&[(
                credential_key(Service::MyAnimeList).into(),
                Some(saved.encode().unwrap()),
            )])
            .unwrap();
        assert!(matches!(
            SavedConnection::load(&storage, Service::MyAnimeList),
            Err(CredentialError::Invalid)
        ));
        storage
            .write(&[(
                credential_key(Service::AniList).into(),
                Some("broken".into()),
            )])
            .unwrap();
        assert!(matches!(
            SavedConnection::load(&storage, Service::AniList),
            Err(CredentialError::Invalid)
        ));
        assert_eq!(
            storage
                .read(credential_key(Service::AniList))
                .unwrap()
                .as_deref(),
            Some("broken")
        );
    }
    #[test]
    fn account_and_credentials_commit_atomically_and_disconnect_removes_only_its_service() {
        let storage = Memory::default();
        let saved = connection(Service::MyAnimeList);
        let mut state = TrackingState::default();
        state.accounts.push(Account {
            key: saved.account.clone(),
            display_name: "fixture".into(),
            generation: std::num::NonZeroU64::MIN,
        });
        state.active_accounts.push(saved.account.clone());
        let mut store = Store::load(storage.clone()).unwrap();
        storage.fail.store(true, Ordering::Relaxed);
        assert!(
            store
                .save_with_connection(state.clone(), Service::MyAnimeList, Some(&saved))
                .is_err()
        );
        assert!(store.state().active_accounts.is_empty());
        assert!(
            SavedConnection::load(&storage, Service::MyAnimeList)
                .unwrap()
                .is_none()
        );
        storage.fail.store(false, Ordering::Relaxed);
        store
            .save_with_connection(state.clone(), Service::MyAnimeList, Some(&saved))
            .unwrap();
        connection(Service::AniList).save(&storage).unwrap();
        state.active_accounts.clear();
        store
            .save_with_connection(state, Service::MyAnimeList, None)
            .unwrap();
        assert!(
            SavedConnection::load(&storage, Service::MyAnimeList)
                .unwrap()
                .is_none()
        );
        assert!(
            SavedConnection::load(&storage, Service::AniList)
                .unwrap()
                .is_some()
        );
        Store::reset_retaining_backup(storage.clone()).unwrap();
        assert!(
            SavedConnection::load(&storage, Service::AniList)
                .unwrap()
                .is_none()
        );
    }
}
