//! Bounded metadata-only cache; confirmed bindings and list values never live
//! here. Transport failures must not be inserted as negative results.
use crate::{Media, ReleaseDetails, Service};
use serde::{Deserialize, Serialize};
pub const CATALOG_CACHE_KEY: &str = "tracking:catalog_cache:v1";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Cached {
    service: Service,
    key: String,
    retrieved_at: u64,
    expires_at: u64,
    provenance: String,
    media: Vec<Media>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogCache {
    entries: Vec<Cached>,
    #[serde(default)]
    releases: Vec<CachedRelease>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CachedRelease {
    service: Service,
    retrieved_at: u64,
    expires_at: u64,
    details: ReleaseDetails,
}
impl CatalogCache {
    pub fn release(
        &self,
        service: Service,
        id: std::num::NonZeroU32,
        now: u64,
    ) -> Option<ReleaseDetails> {
        self.releases
            .iter()
            .find(|r| {
                r.service == service
                    && r.details.media.id == id
                    && r.retrieved_at <= now
                    && now < r.expires_at
                    && r.details.aliases.len() <= 16
                    && r.details.relations.len() <= 64
            })
            .map(|r| r.details.clone())
    }
    pub fn insert_release(&mut self, service: Service, details: ReleaseDetails, now: u64) {
        self.releases.retain(|r| {
            r.expires_at > now && !(r.service == service && r.details.media.id == details.media.id)
        });
        self.entries.retain(|r| r.expires_at > now);
        self.make_room();
        self.releases.push(CachedRelease {
            service,
            retrieved_at: now,
            expires_at: now.saturating_add(3600),
            details,
        });
    }
    fn make_room(&mut self) {
        while self.entries.len() + self.releases.len() >= 128 {
            if self.entries.first().is_some_and(|e| {
                self.releases
                    .first()
                    .is_none_or(|r| e.retrieved_at <= r.retrieved_at)
            }) {
                self.entries.remove(0);
            } else {
                self.releases.remove(0);
            }
        }
    }
    pub fn get(&self, service: Service, key: &str, now: u64) -> Option<Vec<Media>> {
        self.entries
            .iter()
            .find(|e| {
                e.service == service
                    && e.key == key
                    && e.retrieved_at <= now
                    && now < e.expires_at
                    && e.key.len() <= 1024
                    && e.media.len() <= 20
            })
            .map(|e| e.media.clone())
    }
    pub fn insert(&mut self, service: Service, key: String, media: Vec<Media>, now: u64) {
        if key.len() > 1024 || media.len() > 20 {
            return;
        }
        self.entries
            .retain(|e| e.expires_at > now && !(e.service == service && e.key == key));
        self.releases.retain(|r| r.expires_at > now);
        self.make_room();
        let ttl = if media.is_empty() { 300 } else { 3600 };
        self.entries.push(Cached {
            service,
            key,
            retrieved_at: now,
            expires_at: now.saturating_add(ttl),
            provenance: match service {
                Service::MyAnimeList => "MAL API v2",
                Service::AniList => "AniList GraphQL",
            }
            .into(),
            media,
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negative_cache_expires_and_positive_cache_is_bounded() {
        let mut cache = CatalogCache::default();
        cache.insert(Service::AniList, "missing".into(), vec![], 100);
        assert_eq!(cache.get(Service::AniList, "missing", 399), Some(vec![]));
        assert_eq!(cache.get(Service::AniList, "missing", 400), None);
        assert_eq!(cache.get(Service::MyAnimeList, "missing", 150), None);
        for n in 0..200 {
            cache.insert(Service::AniList, n.to_string(), vec![], 100);
        }
        assert_eq!(cache.entries.len(), 128);
        assert_eq!(cache.get(Service::AniList, "0", 100), None);
    }
}

#[cfg(test)]
mod release_tests {
    use super::*;
    fn details(id: u32) -> ReleaseDetails {
        ReleaseDetails {
            media: Media {
                id: id.try_into().unwrap(),
                mal_id: None,
                title: "Release".into(),
                format: "TV".into(),
                episodes: Some(12.try_into().unwrap()),
                finished: true,
                year: Some(2021),
            },
            aliases: vec!["Alias".into()],
            start: crate::ListDate::default(),
            end: crate::ListDate::default(),
            relations: vec![],
        }
    }
    #[test]
    fn older_metadata_cache_is_readable_and_release_metadata_expires_without_account_values() {
        let mut cache: CatalogCache = serde_json::from_str(r#"{"entries":[]}"#).unwrap();
        cache.insert_release(Service::MyAnimeList, details(1), 100);
        assert!(
            cache
                .release(Service::MyAnimeList, 1.try_into().unwrap(), 3699)
                .is_some()
        );
        assert!(
            cache
                .release(Service::MyAnimeList, 1.try_into().unwrap(), 3700)
                .is_none()
        );
        assert!(
            cache
                .release(Service::AniList, 1.try_into().unwrap(), 101)
                .is_none()
        );
        assert!(
            cache
                .release(Service::MyAnimeList, 1.try_into().unwrap(), 99)
                .is_none()
        );
        let encoded = serde_json::to_string(&cache).unwrap();
        let decoded: CatalogCache = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, cache);
    }
    #[test]
    fn searches_and_release_details_share_one_bounded_cache() {
        let mut cache = CatalogCache::default();
        for i in 1..=200 {
            cache.insert(Service::MyAnimeList, i.to_string(), vec![], 100);
            cache.insert_release(Service::AniList, details(i), 100);
        }
        assert_eq!(cache.entries.len() + cache.releases.len(), 128);
        assert!(
            cache
                .release(Service::AniList, 200.try_into().unwrap(), 101)
                .is_some()
        );
        assert!(
            cache
                .release(Service::AniList, 1.try_into().unwrap(), 101)
                .is_none()
        );
    }
}
