//! Nova's binding of the shared backup container (Kit `nova-backup`): the
//! format, limits and strict decoding live in the kit, while which components
//! exist, how their records are validated and how a bundled addon package is
//! checked are decided here. Applying and undoing a restore stay in `mod.rs`.
use super::adapters;
#[cfg(test)]
pub(super) use nova_backup::archive::encode;
pub(super) use nova_backup::archive::{
    Archive, Component, Manifest, Package, Payload, Profile, digest, encode_with_packages,
};
use nova_backup::archive::{PackageIdentity, Schema};

struct NovaSchema;

impl Schema for NovaSchema {
    fn format(&self) -> &str {
        "nova-backup"
    }
    fn supported(&self, component: &str) -> bool {
        adapters::supported(component)
    }
    fn validate(&self, component: &str, payload: &Payload) -> Result<(), String> {
        adapters::validate(component, payload)
    }
    fn max_package_bytes(&self) -> u64 {
        nova_providers::MAX_ARCHIVE_BYTES as u64
    }
    fn package(&self, bytes: &[u8]) -> Result<PackageIdentity, String> {
        let parsed = nova_providers::AddonPackage::parse(bytes).map_err(|e| e.to_string())?;
        Ok(PackageIdentity {
            id: parsed.id().into(),
            version: parsed.version().into(),
            sha256: parsed.sha256().into(),
        })
    }
    fn declared_packages(&self, component: &str, payload: &Payload) -> Vec<Option<String>> {
        if component != "packages" {
            return Vec::new();
        }
        payload
            .records
            .values()
            .map(|value| value["sha256"].as_str().map(str::to_owned))
            .collect()
    }
}

pub(super) fn decode(bytes: &[u8]) -> Result<Archive, String> {
    nova_backup::archive::decode(bytes, &NovaSchema)
}
