//! Portable, bounded ZIP/JSON format. No extraction or native storage pages.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};

pub(super) const MAX_BYTES: usize = 128 * 1024 * 1024;
const MAX_FILE: u64 = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_RECORDS: usize = 200_000;
type LocalValues = BTreeMap<String, Option<String>>;
type UndoLocal = (LocalValues, LocalValues);

pub(super) fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub format: String,
    pub version: u32,
    pub created_at: u64,
    pub app_version: String,
    pub archive_id: String,
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub packages: Vec<Package>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Package {
    pub id: String,
    pub version: String,
    pub path: String,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Profile {
    pub archive_profile_id: String,
    pub source_profile_id: String,
    pub name: String,
    pub components: Vec<Component>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Component {
    pub name: String,
    pub schema_version: u32,
    pub path: String,
    pub item_count: usize,
    pub sha256: String,
}
/// Logical records use adapter-defined semantic identities, never source HLCs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Payload {
    pub version: u32,
    #[serde(deserialize_with = "unique_records")]
    pub records: BTreeMap<String, Value>,
}
fn unique_records<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Value>, D::Error> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, Value>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("unique backup record identities")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Value>()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate backup record identity"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique)
}
#[derive(Clone)]
pub(super) struct Archive {
    pub manifest: Manifest,
    pub payloads: BTreeMap<(String, String), Payload>,
    pub checksum: String,
    pub unsupported: Vec<String>,
    pub undo_after: Option<BTreeMap<(String, String), Payload>>,
    pub packages: BTreeMap<String, Vec<u8>>,
    pub undo_local: Option<UndoLocal>,
}

fn normalized(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
pub(super) fn encode(
    manifest: Manifest,
    payloads: &BTreeMap<(String, String), Payload>,
) -> Result<Vec<u8>, String> {
    encode_with_packages(manifest, payloads, &BTreeMap::new())
}
pub(super) fn encode_with_packages(
    mut manifest: Manifest,
    payloads: &BTreeMap<(String, String), Payload>,
    packages: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut total = 0u64;
    if manifest
        .profiles
        .iter()
        .map(|profile| profile.components.len())
        .sum::<usize>()
        + manifest.packages.len()
        + 1
        > MAX_ENTRIES
    {
        return Err("Too many backup files".into());
    }
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for profile in &mut manifest.profiles {
        for component in &mut profile.components {
            let payload = payloads
                .get(&(profile.archive_profile_id.clone(), component.name.clone()))
                .ok_or("Missing export component")?;
            let bytes = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
            total = total
                .checked_add(bytes.len() as u64)
                .ok_or("Backup size overflow")?;
            if total > MAX_BYTES as u64 {
                return Err("Expanded backup exceeds the size limit".into());
            }
            if bytes.len() as u64 > MAX_FILE || payload.records.len() > MAX_RECORDS {
                return Err("Backup component exceeds the size limit".into());
            }
            component.sha256 = digest(&bytes);
            component.item_count = payload.records.len();
            zip.start_file(&component.path, options)
                .map_err(|e| e.to_string())?;
            zip.write_all(&bytes).map_err(|e| e.to_string())?;
        }
    }
    for package in &manifest.packages {
        let bytes = packages
            .get(&package.sha256)
            .ok_or("Missing addon package file")?;
        total = total
            .checked_add(bytes.len() as u64)
            .ok_or("Backup size overflow")?;
        if total > MAX_BYTES as u64 {
            return Err("Expanded backup exceeds the size limit".into());
        }
        zip.start_file(&package.path, options)
            .map_err(|e| e.to_string())?;
        zip.write_all(bytes).map_err(|e| e.to_string())?;
    }
    zip.start_file("manifest.json", options)
        .map_err(|e| e.to_string())?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let bytes = zip.finish().map_err(|e| e.to_string())?.into_inner();
    if bytes.len() > MAX_BYTES {
        return Err("Backup exceeds the size limit".into());
    }
    Ok(bytes)
}

pub(super) fn decode(bytes: &[u8]) -> Result<Archive, String> {
    if bytes.len() > MAX_BYTES {
        return Err("Backup exceeds the size limit".into());
    }
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("Invalid backup ZIP: {e}"))?;
    if zip.len() > MAX_ENTRIES {
        return Err("Too many backup files".into());
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for index in 0..zip.len() {
        let entry = zip.by_index(index).map_err(|e| e.to_string())?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("Invalid backup symbolic link".into());
        }
        if !normalized(entry.name()) || !names.insert(entry.name().to_owned()) {
            return Err("Duplicate or invalid backup path".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("Backup size overflow")?;
        if entry.size() > MAX_FILE || total > MAX_BYTES as u64 {
            return Err("Expanded backup exceeds the size limit".into());
        }
    }
    let mut manifest_bytes = Vec::new();
    zip.by_name("manifest.json")
        .map_err(|_| "Backup manifest is missing")?
        .take(MAX_FILE + 1)
        .read_to_end(&mut manifest_bytes)
        .map_err(|e| e.to_string())?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("Invalid backup manifest: {e}"))?;
    if manifest.format != "nova-backup" || manifest.version != 1 {
        return Err("Unsupported backup format version".into());
    }
    let mut profiles = BTreeSet::new();
    let mut listed = BTreeSet::new();
    let mut payloads = BTreeMap::new();
    let mut unsupported = Vec::new();
    let mut packages = BTreeMap::new();
    for profile in &manifest.profiles {
        if !profiles.insert(profile.archive_profile_id.clone())
            || profile.name.trim().is_empty()
            || profile.name.chars().count() > 64
        {
            return Err("Invalid or duplicate backup profile".into());
        }
        let mut components = BTreeSet::new();
        for component in &profile.components {
            if !components.insert(&component.name)
                || !normalized(&component.path)
                || !listed.insert(&component.path)
            {
                return Err("Duplicate or invalid backup component".into());
            }
            let mut data = Vec::new();
            zip.by_name(&component.path)
                .map_err(|_| format!("Missing backup component: {}", component.name))?
                .take(MAX_FILE + 1)
                .read_to_end(&mut data)
                .map_err(|e| e.to_string())?;
            if digest(&data) != component.sha256 {
                return Err(format!("Checksum failed for {}", component.name));
            }
            if component.schema_version != 1 || !super::adapters::supported(&component.name) {
                unsupported.push(component.name.clone());
                continue;
            }
            let payload: Payload = serde_json::from_slice(&data)
                .map_err(|e| format!("Invalid {}: {e}", component.name))?;
            if payload.version != 1
                || payload.records.len() != component.item_count
                || payload.records.len() > MAX_RECORDS
            {
                return Err(format!(
                    "Invalid record count/version for {}",
                    component.name
                ));
            }
            super::adapters::validate(&component.name, &payload)?;
            payloads.insert(
                (profile.archive_profile_id.clone(), component.name.clone()),
                payload,
            );
        }
    }
    for package in &manifest.packages {
        if package.path != format!("packages/{}.zip", package.sha256)
            || !normalized(&package.path)
            || !listed.insert(&package.path)
        {
            return Err("Duplicate or invalid addon package path".into());
        }
        let mut data = Vec::new();
        zip.by_name(&package.path)
            .map_err(|_| "Missing addon package file")?
            .take(nova_providers::MAX_ARCHIVE_BYTES as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        if digest(&data) != package.sha256 {
            return Err("Addon package checksum failed".into());
        }
        let parsed = nova_providers::AddonPackage::parse(&data).map_err(|e| e.to_string())?;
        if parsed.id() != package.id
            || parsed.version() != package.version
            || parsed.sha256() != package.sha256
        {
            return Err("Addon package identity mismatch".into());
        }
        packages.insert(package.sha256.clone(), data);
    }
    for ((_, name), payload) in &payloads {
        if name == "packages"
            && payload.records.values().any(|value| {
                value["sha256"]
                    .as_str()
                    .is_none_or(|hash| !packages.contains_key(hash))
            })
        {
            return Err("Missing declared addon package file".into());
        }
    }
    Ok(Archive {
        manifest,
        payloads,
        checksum: digest(bytes),
        unsupported,
        undo_after: None,
        packages,
        undo_local: None,
    })
}
