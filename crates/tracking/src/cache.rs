//! Bounded metadata-only cache; confirmed bindings and list values never live
//! here. Transport failures must not be inserted as negative results.
use crate::{Media, Service};
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
}
impl CatalogCache {
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
        while self.entries.len() >= 128 {
            self.entries.remove(0);
        }
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
