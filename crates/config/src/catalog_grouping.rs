//! Profile-owned catalog presentation rules. These never rewrite media identity.
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogGroupingMode {
    #[default]
    Original,
    Season,
    Series,
}

impl CatalogGroupingMode {
    pub fn index(self) -> i32 {
        match self {
            Self::Original => 0,
            Self::Season => 1,
            Self::Series => 2,
        }
    }

    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Season,
            2 => Self::Series,
            _ => Self::Original,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSurface {
    Home,
    Discover,
}

/// Installation URLs are opaque identity, including any configured credentials.
/// Display labels must come from manifests and must never print this URL.
/// Genre is intentionally absent: every genre inherits its catalog's rule.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogGroupingKey {
    pub addon_url: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub catalog_id: String,
    pub surface: CatalogSurface,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogGroupingOverride {
    #[serde(flatten)]
    pub key: CatalogGroupingKey,
    pub mode: CatalogGroupingMode,
}

/// The entire vector is one synced settings field. Last occurrence wins for
/// duplicate keys, then key sorting gives deterministic persistence/UI order.
pub fn normalize_overrides(rows: Vec<CatalogGroupingOverride>) -> Vec<CatalogGroupingOverride> {
    rows.into_iter()
        .map(|row| (row.key.clone(), row))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

pub fn deserialize_overrides<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<CatalogGroupingOverride>, D::Error> {
    Vec::deserialize(deserializer).map(normalize_overrides)
}

impl crate::CacheSettings {
    pub fn catalog_grouping_mode(&self, key: &CatalogGroupingKey) -> CatalogGroupingMode {
        self.catalog_grouping_overrides
            .iter()
            .rev()
            .find(|row| &row.key == key)
            .map_or(self.catalog_grouping_default, |row| row.mode)
    }

    /// None means Default/inherit; explicit Original is retained independently.
    pub fn set_catalog_grouping_override(
        &mut self,
        key: CatalogGroupingKey,
        mode: Option<CatalogGroupingMode>,
    ) {
        self.catalog_grouping_overrides.retain(|row| row.key != key);
        if let Some(mode) = mode {
            self.catalog_grouping_overrides
                .push(CatalogGroupingOverride { key, mode });
        }
        self.catalog_grouping_overrides =
            normalize_overrides(std::mem::take(&mut self.catalog_grouping_overrides));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CacheSettings;

    fn key(url: &str, surface: CatalogSurface) -> CatalogGroupingKey {
        CatalogGroupingKey {
            addon_url: url.into(),
            type_: "series".into(),
            catalog_id: "anime".into(),
            surface,
        }
    }

    #[test]
    fn original_is_backwards_compatible_and_distinct_from_inheritance() {
        let mut value = serde_json::to_value(CacheSettings::default()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("catalog_grouping_default");
        value
            .as_object_mut()
            .unwrap()
            .remove("catalog_grouping_overrides");
        let mut settings: CacheSettings = serde_json::from_value(value).unwrap();
        let first = key("https://one.example/secret", CatalogSurface::Discover);
        let second = key("https://two.example/secret", CatalogSurface::Discover);
        assert_eq!(
            settings.catalog_grouping_mode(&first),
            CatalogGroupingMode::Original
        );
        settings.catalog_grouping_default = CatalogGroupingMode::Series;
        settings.set_catalog_grouping_override(first.clone(), Some(CatalogGroupingMode::Original));
        assert_eq!(
            settings.catalog_grouping_mode(&first),
            CatalogGroupingMode::Original
        );
        assert_eq!(
            settings.catalog_grouping_mode(&second),
            CatalogGroupingMode::Series
        );
        assert_eq!(
            settings
                .catalog_grouping_mode(&key("https://one.example/secret", CatalogSurface::Home)),
            CatalogGroupingMode::Series
        );
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
        settings.set_catalog_grouping_override(first.clone(), None);
        assert_eq!(
            settings.catalog_grouping_mode(&first),
            CatalogGroupingMode::Series
        );
    }

    #[test]
    fn unavailable_catalogs_survive_and_duplicate_keys_are_deterministic() {
        let k = key("https://uninstalled.example", CatalogSurface::Home);
        let rows = vec![
            CatalogGroupingOverride {
                key: k.clone(),
                mode: CatalogGroupingMode::Original,
            },
            CatalogGroupingOverride {
                key: k,
                mode: CatalogGroupingMode::Season,
            },
        ];
        let parsed: Vec<CatalogGroupingOverride> = normalize_overrides(rows);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].mode, CatalogGroupingMode::Season);
    }
}
