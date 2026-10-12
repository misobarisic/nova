//! Bounded, manifest-driven ZIP loading. Packages remain immutable during worker calls.
use crate::{PluginManifest, ProviderHostError};
use std::{
    collections::{BTreeMap, HashSet},
    io::{Cursor, Read},
    sync::Arc,
};

pub const MAX_ARCHIVE_BYTES: usize = 8 * 1024 * 1024;
const MAX_EXPANDED_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_SCRIPT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub struct AddonPackage {
    pub(crate) plugin: PluginManifest,
    pub(crate) addon: addons::Manifest,
    pub(crate) source: String,
    pub(crate) assets: BTreeMap<String, Arc<[u8]>>,
    pub(crate) digest: String,
}
fn invalid(message: &str) -> ProviderHostError {
    ProviderHostError(message.into())
}
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0', ':'])
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
fn validate_script_syntax(source: &str) -> Result<(), ProviderHostError> {
    let runtime =
        rquickjs::Runtime::new().map_err(|_| invalid("cannot create script validator"))?;
    runtime.set_memory_limit(32 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    let started = std::time::Instant::now();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        started.elapsed() > std::time::Duration::from_secs(5)
    })));
    let context =
        rquickjs::Context::full(&runtime).map_err(|_| invalid("cannot create script validator"))?;
    let source =
        std::ffi::CString::new(source).map_err(|_| invalid("script contains a NUL byte"))?;
    context.with(|ctx| {
        // rquickjs EvalOptions does not expose compile-only global evaluation.
        // Keep the source buffer/context alive and free the owned result; the
        // compile-only flag parses the exact global script without executing any
        // addon code, installing host services or reading user preferences.
        unsafe {
            let flags = rquickjs::qjs::JS_EVAL_TYPE_GLOBAL
                | rquickjs::qjs::JS_EVAL_FLAG_STRICT
                | rquickjs::qjs::JS_EVAL_FLAG_COMPILE_ONLY;
            let value = rquickjs::qjs::JS_Eval(
                ctx.as_raw().as_ptr(),
                source.as_ptr(),
                source.as_bytes().len() as _,
                c"package.js".as_ptr(),
                flags as _,
            );
            let failed = rquickjs::qjs::JS_IsException(value);
            rquickjs::qjs::JS_FreeValue(ctx.as_raw().as_ptr(), value);
            if failed {
                Err(invalid("invalid or oversized JavaScript syntax"))
            } else {
                Ok(())
            }
        }
    })
}
impl AddonPackage {
    pub fn parse(bytes: &[u8]) -> Result<Arc<Self>, ProviderHostError> {
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err(invalid("package exceeds archive size limit"));
        }
        let mut zip =
            zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid("invalid ZIP package"))?;
        // zip's name index collapses duplicate central-directory entries. Count
        // the original records before trusting that index or the entry limit.
        let mut offset = usize::try_from(zip.central_directory_start())
            .map_err(|_| invalid("invalid ZIP directory"))?;
        let mut records = 0usize;
        while bytes.get(offset..offset.saturating_add(4)) == Some(b"PK\x01\x02".as_slice()) {
            let header = bytes
                .get(offset..offset.saturating_add(46))
                .ok_or_else(|| invalid("truncated ZIP directory"))?;
            let length = |i| usize::from(u16::from_le_bytes([header[i], header[i + 1]]));
            offset = offset
                .checked_add(46 + length(28) + length(30) + length(32))
                .ok_or_else(|| invalid("ZIP directory overflow"))?;
            if offset > bytes.len() {
                return Err(invalid("truncated ZIP directory"));
            }
            records += 1;
            if records > MAX_ENTRIES {
                return Err(invalid("package exceeds entry limit"));
            }
        }
        if records != zip.len() {
            return Err(invalid("duplicate ZIP entry"));
        }
        if zip.len() > MAX_ENTRIES {
            return Err(invalid("package exceeds entry limit"));
        }
        let mut files = BTreeMap::new();
        let mut names = HashSet::new();
        let mut expanded = 0usize;
        for index in 0..zip.len() {
            let mut file = zip
                .by_index(index)
                .map_err(|_| invalid("unreadable or encrypted ZIP entry"))?;
            let name = file.name().to_owned();
            let path = name.strip_suffix('/').unwrap_or(&name);
            if !safe_path(path)
                || !names.insert(name.clone())
                || file.encrypted()
                || file
                    .unix_mode()
                    .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err(invalid("unsafe or duplicate ZIP entry"));
            }
            let size =
                usize::try_from(file.size()).map_err(|_| invalid("package entry too large"))?;
            expanded = expanded
                .checked_add(size)
                .ok_or_else(|| invalid("package size overflow"))?;
            if expanded > MAX_EXPANDED_BYTES {
                return Err(invalid("package exceeds expanded size limit"));
            }
            if file.is_dir() {
                continue;
            }
            let mut data = Vec::new();
            file.by_ref()
                .take((MAX_EXPANDED_BYTES + 1) as u64)
                .read_to_end(&mut data)
                .map_err(|_| invalid("invalid ZIP entry data"))?;
            if data.len() != size {
                return Err(invalid("ZIP entry size mismatch"));
            }
            files.insert(name, data);
        }
        if !files.contains_key("manifest.json") {
            let roots: HashSet<_> = files
                .keys()
                .filter_map(|p| p.split_once('/').map(|(root, _)| root.to_owned()))
                .collect();
            if roots.len() != 1 || files.keys().any(|p| !p.contains('/')) {
                return Err(invalid("missing package manifest"));
            }
            let prefix = format!("{}/", roots.into_iter().next().unwrap());
            files = files
                .into_iter()
                .map(|(name, data)| (name.strip_prefix(&prefix).unwrap().to_owned(), data))
                .collect();
        }
        let get = |name: &str| {
            files
                .get(name)
                .ok_or_else(|| invalid("missing declared package file"))
        };
        let raw_manifest: serde_json::Value = serde_json::from_slice(get("manifest.json")?)
            .map_err(|_| invalid("invalid plugin manifest"))?;
        if raw_manifest
            .get("scripts")
            .is_some_and(|scripts| scripts.as_array().is_some_and(Vec::is_empty))
        {
            return Err(invalid("explicit scripts list must not be empty"));
        }
        let plugin: PluginManifest =
            serde_json::from_value(raw_manifest).map_err(|_| invalid("invalid plugin manifest"))?;
        let addon: addons::Manifest = serde_json::from_slice(get("stremio-manifest.json")?)
            .map_err(|_| invalid("invalid Stremio manifest"))?;
        if plugin.format_version != 1
            || plugin.id.is_empty()
            || plugin.id.len() > 64
            || !plugin
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
            || semver::Version::parse(&plugin.version).is_err()
            || addon.version != plugin.version
            || addon.id != format!("nova.{}", plugin.id)
            || plugin.name.trim().is_empty()
            || addon.name.trim().is_empty()
        {
            return Err(invalid("invalid package identity or version"));
        }
        crate::plugin_settings::validate_schema(&plugin.configuration)?;
        get("LICENSE")?;
        get("NOTICE.md")?;
        let scripts = if plugin.scripts.is_empty() {
            vec![plugin.entry.clone()]
        } else {
            plugin.scripts.clone()
        };
        if scripts.last() != Some(&plugin.entry) {
            return Err(invalid("entry script must be last"));
        }
        let mut declared = HashSet::from([
            "manifest.json".to_owned(),
            "stremio-manifest.json".to_owned(),
            "LICENSE".to_owned(),
            "NOTICE.md".to_owned(),
        ]);
        let mut source = String::new();
        for path in scripts {
            if !safe_path(&path) || !path.ends_with(".js") || !declared.insert(path.clone()) {
                return Err(invalid("invalid script declaration"));
            }
            let script =
                std::str::from_utf8(get(&path)?).map_err(|_| invalid("script must be UTF-8"))?;
            if source.len().saturating_add(script.len()).saturating_add(3) > MAX_SCRIPT_BYTES {
                return Err(invalid("assembled script exceeds size limit"));
            }
            source.push_str(script);
            source.push_str("\n;\n");
        }
        let mut assets = BTreeMap::new();
        for path in &plugin.assets {
            if !safe_path(path) || !declared.insert(path.clone()) {
                return Err(invalid("invalid asset declaration"));
            }
            assets.insert(path.clone(), Arc::from(get(path)?.as_slice()));
        }
        if files.keys().any(|path| !declared.contains(path)) {
            return Err(invalid("undeclared package file"));
        }
        validate_script_syntax(&source)?;
        let hash = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes);
        let digest = hash
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Arc::new(Self {
            plugin,
            addon,
            source,
            assets,
            digest,
        }))
    }
    pub fn id(&self) -> &str {
        &self.plugin.id
    }
    pub fn version(&self) -> &str {
        &self.plugin.version
    }
    pub fn sha256(&self) -> &str {
        &self.digest
    }
    pub fn url(&self) -> String {
        format!("nova-provider://{}", self.id())
    }
    pub fn addon_manifest(&self) -> &addons::Manifest {
        &self.addon
    }
    pub(crate) fn asset(&self, path: &str) -> Option<Arc<[u8]>> {
        self.assets.get(path).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::{ZipWriter, write::SimpleFileOptions};
    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            writer
                .start_file(
                    *name,
                    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    fn fixture(manifest: serde_json::Value) -> Vec<(String, Vec<u8>)> {
        vec![
            (
                "manifest.json".into(),
                serde_json::to_vec(&manifest).unwrap(),
            ),
            (
                "stremio-manifest.json".into(),
                br#"{"id":"nova.fixture","name":"Fixture","version":"1.0.0"}"#.to_vec(),
            ),
            ("LICENSE".into(), b"license".to_vec()),
            ("NOTICE.md".into(), b"notice".to_vec()),
            ("index.js".into(), b"globalThis.novaProvider={};".to_vec()),
        ]
    }
    fn manifest() -> serde_json::Value {
        serde_json::json!({"formatVersion":1,"id":"fixture","name":"Fixture","version":"1.0.0","entry":"index.js","permissions":{}})
    }
    fn zip(files: &[(String, Vec<u8>)]) -> Vec<u8> {
        archive(
            &files
                .iter()
                .map(|(n, d)| (n.as_str(), d.as_slice()))
                .collect::<Vec<_>>(),
        )
    }
    #[test]
    fn loads_flat_and_enclosed_packages_with_stable_identity() {
        let files = fixture(manifest());
        let bytes = zip(&files);
        let package = AddonPackage::parse(&bytes).unwrap();
        assert_eq!(package.id(), "fixture");
        assert_eq!(package.version(), "1.0.0");
        assert_eq!(package.url(), "nova-provider://fixture");
        assert_eq!(package.sha256().len(), 64);
        assert_eq!(
            package.sha256(),
            AddonPackage::parse(&bytes).unwrap().sha256()
        );
        let enclosed: Vec<_> = files
            .into_iter()
            .map(|(n, d)| (format!("release/{n}"), d))
            .collect();
        assert!(AddonPackage::parse(&zip(&enclosed)).is_ok());
    }
    #[test]
    fn assembles_declared_order_and_serves_only_declared_assets() {
        let mut manifest = manifest();
        manifest["scripts"] = serde_json::json!(["before.js", "index.js"]);
        manifest["assets"] = serde_json::json!(["images/logo.jpg"]);
        let mut files = fixture(manifest);
        files.push(("before.js".into(), b"first();".to_vec()));
        files.push(("images/logo.jpg".into(), vec![1, 2, 3]));
        let package = AddonPackage::parse(&zip(&files)).unwrap();
        assert!(package.source.starts_with("first();\n;\n"));
        assert_eq!(&*package.asset("images/logo.jpg").unwrap(), &[1, 2, 3]);
        assert!(package.asset("index.js").is_none());
    }
    #[test]
    fn rejects_paths_missing_files_and_unlisted_inputs() {
        for name in [
            "../escape",
            "/absolute",
            "dir\\file",
            "a/../b",
            "C:/file",
            "unused.js",
        ] {
            let mut files = fixture(manifest());
            files.push((name.into(), b"bad".to_vec()));
            assert!(AddonPackage::parse(&zip(&files)).is_err(), "{name}");
        }
        for missing in ["LICENSE", "NOTICE.md", "index.js", "stremio-manifest.json"] {
            let files: Vec<_> = fixture(manifest())
                .into_iter()
                .filter(|(name, _)| name != missing)
                .collect();
            assert!(AddonPackage::parse(&zip(&files)).is_err());
        }
    }
    #[test]
    fn rejects_invalid_identity_and_script_order() {
        for (key, value) in [
            ("id", serde_json::json!("Fixture")),
            ("version", serde_json::json!("1")),
            ("formatVersion", serde_json::json!(2)),
            ("scripts", serde_json::json!(["index.js", "before.js"])),
            ("scripts", serde_json::json!(["index.js", "index.js"])),
            ("scripts", serde_json::json!([])),
        ] {
            let mut m = manifest();
            m[key] = value;
            assert!(AddonPackage::parse(&zip(&fixture(m))).is_err());
        }
    }
    #[test]
    fn enforces_archive_and_assembled_script_limits() {
        assert!(AddonPackage::parse(&vec![0; MAX_ARCHIVE_BYTES + 1]).is_err());
        let mut files = fixture(manifest());
        files.iter_mut().find(|(n, _)| n == "index.js").unwrap().1 = vec![b' '; MAX_SCRIPT_BYTES];
        assert!(AddonPackage::parse(&zip(&files)).is_err());
    }
    #[test]
    fn rejects_symlinks_encryption_duplicates_and_entry_floods() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink("link", "manifest.json", SimpleFileOptions::default())
            .unwrap();
        assert!(AddonPackage::parse(&writer.finish().unwrap().into_inner()).is_err());
        let mut encrypted = zip(&fixture(manifest()));
        // General-purpose flag bit zero marks encryption in both ZIP headers.
        for i in 0..encrypted.len().saturating_sub(10) {
            if &encrypted[i..i + 4] == b"PK\x03\x04" {
                encrypted[i + 6] |= 1;
            }
            if &encrypted[i..i + 4] == b"PK\x01\x02" {
                encrypted[i + 8] |= 1;
            }
        }
        assert!(AddonPackage::parse(&encrypted).is_err());
        let mut files = fixture(manifest());
        files.push(("other.js".into(), b"duplicate".to_vec()));
        let mut duplicate = zip(&files);
        for i in 0..duplicate.len().saturating_sub(8) {
            if &duplicate[i..i + 8] == b"other.js" {
                duplicate[i..i + 8].copy_from_slice(b"index.js");
            }
        }
        assert!(AddonPackage::parse(&duplicate).is_err());
        let files: Vec<_> = (0..=MAX_ENTRIES)
            .map(|n| (format!("file{n}"), vec![]))
            .collect();
        assert!(AddonPackage::parse(&zip(&files)).is_err());
    }
    #[test]
    fn rejects_compressed_expansion_limit() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "huge",
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(&vec![0; MAX_EXPANDED_BYTES + 1]).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(bytes.len() < MAX_ARCHIVE_BYTES);
        assert!(AddonPackage::parse(&bytes).is_err());
    }
    #[test]
    fn validates_syntax_without_executing_scripts_or_reading_settings() {
        let mut files = fixture(manifest());
        files
            .iter_mut()
            .find(|(name, _)| name == "index.js")
            .unwrap()
            .1 =
            b"throw new Error('must not execute'); nova.settings.set('secret','changed');".to_vec();
        assert!(AddonPackage::parse(&zip(&files)).is_ok());
        files
            .iter_mut()
            .find(|(name, _)| name == "index.js")
            .unwrap()
            .1 = b"function broken( {".to_vec();
        assert!(AddonPackage::parse(&zip(&files)).is_err());
        let mut m = manifest();
        m["configuration"] = serde_json::json!([{"key":"wrong","label":"Wrong","type":"boolean","default":"not a boolean"}]);
        assert!(AddonPackage::parse(&zip(&fixture(m))).is_err());
        let mut m = manifest();
        m["configuration"] = serde_json::Value::Array((0..129).map(|n| serde_json::json!({
            "key": format!("flag{n}"), "label": "Flag", "type": "boolean", "default": false
        })).collect());
        assert!(AddonPackage::parse(&zip(&fixture(m))).is_err());
    }
}
