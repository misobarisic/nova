//! Bounded v1 metainfo inspection, independent of the playback torrent engine.
//! Hash the original encoded info dictionary: re-encoding it can change identity.
use aws_lc_rs::digest::{SHA1_FOR_LEGACY_USE_ONLY, digest};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, ops::Range};

use crate::ProviderError;

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_NODES: usize = 32_768;
const MAX_DEPTH: usize = 32;
const MAX_FILES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentMetadata {
    pub info_hash: String,
    pub files: Vec<TorrentMetadataFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentMetadataFile {
    pub index: u32,
    pub path: String,
    pub size: u64,
}

enum Value<'a> {
    Bytes(&'a [u8]),
    Integer(i64),
    List(Vec<Node<'a>>),
    Dictionary(Vec<(&'a [u8], Node<'a>)>),
}
struct Node<'a> {
    value: Value<'a>,
    encoded: Range<usize>,
}
impl<'a> Node<'a> {
    fn get(&self, key: &[u8]) -> Option<&Node<'a>> {
        match &self.value {
            Value::Dictionary(values) => values.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn bytes(&self) -> Result<&'a [u8], ProviderError> {
        match self.value {
            Value::Bytes(bytes) => Ok(bytes),
            _ => Err(invalid()),
        }
    }
    fn unsigned(&self) -> Result<u64, ProviderError> {
        match self.value {
            Value::Integer(value) => value.try_into().map_err(|_| invalid()),
            _ => Err(invalid()),
        }
    }
    fn list(&self) -> Result<&[Node<'a>], ProviderError> {
        match &self.value {
            Value::List(values) => Ok(values),
            _ => Err(invalid()),
        }
    }
}
fn invalid() -> ProviderError {
    ProviderError::InvalidData("invalid or oversized v1 torrent metadata".into())
}
struct Parser<'a> {
    input: &'a [u8],
    position: usize,
    nodes: usize,
}
impl<'a> Parser<'a> {
    fn bytes(&mut self) -> Result<&'a [u8], ProviderError> {
        let start = self.position;
        while self
            .input
            .get(self.position)
            .is_some_and(u8::is_ascii_digit)
        {
            self.position += 1;
        }
        if self.position == start
            || self.input.get(self.position) != Some(&b':')
            || (self.position > start + 1 && self.input[start] == b'0')
        {
            return Err(invalid());
        }
        let length: usize = std::str::from_utf8(&self.input[start..self.position])
            .map_err(|_| invalid())?
            .parse()
            .map_err(|_| invalid())?;
        self.position += 1;
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.input.len())
            .ok_or_else(invalid)?;
        let bytes = &self.input[self.position..end];
        self.position = end;
        Ok(bytes)
    }
    fn node(&mut self, depth: usize) -> Result<Node<'a>, ProviderError> {
        self.nodes += 1;
        if depth > MAX_DEPTH || self.nodes > MAX_NODES {
            return Err(invalid());
        }
        let start = self.position;
        let value = match self.input.get(self.position).copied().ok_or_else(invalid)? {
            b'0'..=b'9' => Value::Bytes(self.bytes()?),
            b'i' => {
                self.position += 1;
                let integer_start = self.position;
                while self
                    .input
                    .get(self.position)
                    .is_some_and(|byte| *byte != b'e')
                {
                    self.position += 1;
                }
                let raw = std::str::from_utf8(&self.input[integer_start..self.position])
                    .map_err(|_| invalid())?;
                let digits = raw.strip_prefix('-').unwrap_or(raw);
                if digits.is_empty()
                    || !digits.bytes().all(|b| b.is_ascii_digit())
                    || (digits.len() > 1 && digits.starts_with('0'))
                    || raw == "-0"
                {
                    return Err(invalid());
                }
                let integer = raw.parse().map_err(|_| invalid())?;
                if self.input.get(self.position) != Some(&b'e') {
                    return Err(invalid());
                }
                self.position += 1;
                Value::Integer(integer)
            }
            b'l' => {
                self.position += 1;
                let mut values = Vec::new();
                while self.input.get(self.position) != Some(&b'e') {
                    values.push(self.node(depth + 1)?);
                }
                self.position += 1;
                Value::List(values)
            }
            b'd' => {
                self.position += 1;
                let mut values = Vec::new();
                let mut previous: Option<&[u8]> = None;
                while self.input.get(self.position) != Some(&b'e') {
                    let key = self.bytes()?;
                    if previous.is_some_and(|old| old >= key) {
                        return Err(invalid());
                    }
                    previous = Some(key);
                    values.push((key, self.node(depth + 1)?));
                }
                self.position += 1;
                Value::Dictionary(values)
            }
            _ => return Err(invalid()),
        };
        Ok(Node {
            value,
            encoded: start..self.position,
        })
    }
}
fn path_component(raw: &[u8]) -> Result<&str, ProviderError> {
    let component = std::str::from_utf8(raw).map_err(|_| invalid())?;
    if component.is_empty()
        || component.len() > 255
        || component == "."
        || component == ".."
        || component.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        || component.ends_with([' ', '.'])
    {
        return Err(invalid());
    }
    // The same metadata may later be downloaded on Windows. Device names must
    // not become platform-specific filesystem targets after a synced selection.
    let base = component
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || base
            .strip_prefix("COM")
            .or_else(|| base.strip_prefix("LPT"))
            .is_some_and(|number| number.len() == 1 && matches!(number.as_bytes()[0], b'1'..=b'9'))
    {
        return Err(invalid());
    }
    Ok(component)
}

pub fn inspect_torrent_metadata(bytes: &[u8]) -> Result<TorrentMetadata, ProviderError> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(invalid());
    }
    let mut parser = Parser {
        input: bytes,
        position: 0,
        nodes: 0,
    };
    let root = parser.node(0)?;
    if parser.position != bytes.len() {
        return Err(invalid());
    }
    let info = root.get(b"info").ok_or_else(invalid)?;
    if info.get(b"meta version").is_some()
        || info.get(b"symlink path").is_some()
        || info
            .get(b"attr")
            .map(Node::bytes)
            .transpose()?
            .is_some_and(|attr| attr.contains(&b'l'))
    {
        return Err(invalid());
    }
    let name = path_component(info.get(b"name").ok_or_else(invalid)?.bytes()?)?;
    if let Some(alternate) = info.get(b"name.utf-8")
        && path_component(alternate.bytes()?)? != name
    {
        return Err(invalid());
    }
    let piece_length = info.get(b"piece length").ok_or_else(invalid)?.unsigned()?;
    let pieces = info.get(b"pieces").ok_or_else(invalid)?.bytes()?;
    if piece_length == 0 || pieces.len() % 20 != 0 {
        return Err(invalid());
    }
    let mut files = Vec::new();
    let mut paths = BTreeSet::<String>::new();
    if let Some(multiple) = info.get(b"files") {
        if info.get(b"length").is_some() {
            return Err(invalid());
        }
        let values = multiple.list()?;
        if values.is_empty() || values.len() > MAX_FILES {
            return Err(invalid());
        }
        for (index, file) in values.iter().enumerate() {
            if file.get(b"symlink path").is_some()
                || file
                    .get(b"attr")
                    .map(Node::bytes)
                    .transpose()?
                    .is_some_and(|attr| attr.contains(&b'l'))
            {
                return Err(invalid());
            }
            let parts = file.get(b"path").ok_or_else(invalid)?.list()?;
            if parts.is_empty() || parts.len() > MAX_DEPTH {
                return Err(invalid());
            }
            if let Some(alternate) = file.get(b"path.utf-8") {
                let alternate = alternate.list()?;
                if alternate.len() != parts.len() {
                    return Err(invalid());
                }
                for (left, right) in parts.iter().zip(alternate) {
                    if path_component(left.bytes()?)? != path_component(right.bytes()?)? {
                        return Err(invalid());
                    }
                }
            }
            let mut path = name.to_owned();
            for part in parts {
                path.push('/');
                path.push_str(path_component(part.bytes()?)?);
            }
            let identity = path.to_ascii_lowercase();
            let descendant = format!("{identity}/");
            let has_descendant = paths
                .range(descendant.clone()..)
                .next()
                .is_some_and(|other| other.starts_with(&descendant));
            let has_ancestor = identity
                .match_indices('/')
                .any(|(end, _)| paths.contains(&identity[..end]));
            if path.len() > 4096 || paths.contains(&identity) || has_descendant || has_ancestor {
                return Err(invalid());
            }
            paths.insert(identity);
            files.push(TorrentMetadataFile {
                index: index as u32,
                path,
                size: file.get(b"length").ok_or_else(invalid)?.unsigned()?,
            });
        }
    } else {
        files.push(TorrentMetadataFile {
            index: 0,
            path: name.to_owned(),
            size: info.get(b"length").ok_or_else(invalid)?.unsigned()?,
        });
    }
    let total = files
        .iter()
        .try_fold(0u64, |sum, file| sum.checked_add(file.size))
        .ok_or_else(invalid)?;
    if total == 0 || total.div_ceil(piece_length) != (pieces.len() / 20) as u64 {
        return Err(invalid());
    }
    let hash = digest(&SHA1_FOR_LEGACY_USE_ONLY, &bytes[info.encoded.clone()]);
    let info_hash = hash
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(TorrentMetadata { info_hash, files })
}

#[cfg(test)]
mod tests {
    use super::*;
    const SINGLE_INFO: &[u8] =
        b"d6:lengthi4e4:name9:movie.mkv12:piece lengthi4e6:pieces20:01234567890123456789e";
    fn wrap(info: &[u8]) -> Vec<u8> {
        [b"d4:info".as_slice(), info, b"e"].concat()
    }

    #[test]
    fn hashes_original_info_and_returns_exact_file_index() {
        let metadata = inspect_torrent_metadata(&wrap(SINGLE_INFO)).unwrap();
        assert_eq!(
            metadata.info_hash,
            "28d017588ce9fe1e1ad97470e75561fcacd7eb82"
        );
        assert_eq!(
            metadata.files,
            vec![TorrentMetadataFile {
                index: 0,
                path: "movie.mkv".into(),
                size: 4
            }]
        );
        let multi = b"d5:filesld6:lengthi2e4:pathl7:one.mkveed6:lengthi2e4:pathl7:two.mkveee4:name4:show12:piece lengthi4e6:pieces20:01234567890123456789e";
        let metadata = inspect_torrent_metadata(&wrap(multi)).unwrap();
        assert_eq!(metadata.files[0].path, "show/one.mkv");
        assert_eq!(
            metadata.files[1],
            TorrentMetadataFile {
                index: 1,
                path: "show/two.mkv".into(),
                size: 2
            }
        );
    }

    #[test]
    fn rejects_malformed_unsafe_and_unsupported_metainfo() {
        for info in [
            b"d6:lengthi4e4:name2:..12:piece lengthi4e6:pieces20:01234567890123456789e".as_slice(),
            b"d6:lengthi4e4:name7:CON.mkv12:piece lengthi4e6:pieces20:01234567890123456789e",
            b"d6:lengthi4e4:name7:a/b.mkv12:piece lengthi4e6:pieces20:01234567890123456789e",
            b"d6:lengthi4e4:name9:movie.mkv12:piece lengthi0e6:pieces20:01234567890123456789e",
            b"d6:lengthi9e4:name9:movie.mkv12:piece lengthi4e6:pieces20:01234567890123456789e",
            b"d6:lengthi-1e4:name9:movie.mkv12:piece lengthi4e6:pieces20:01234567890123456789e",
            b"d6:lengthi04e4:name9:movie.mkv12:piece lengthi4e6:pieces20:01234567890123456789e",
            b"d6:lengthi4e6:lengthi4e4:name9:movie.mkv12:piece lengthi4e6:pieces20:01234567890123456789e",
            b"d6:lengthi4e12:meta versioni2e4:name9:movie.mkv12:piece lengthi4e6:pieces20:01234567890123456789e",
        ] { assert!(inspect_torrent_metadata(&wrap(info)).is_err()); }
        let multi = "d5:filesld6:lengthi2e4:pathl7:one.mkveed6:lengthi2e4:pathl7:two.mkveee4:name4:show12:piece lengthi4e6:pieces20:01234567890123456789e";
        for unsafe_info in [
            multi.replace("two.mkv", "one.mkv"),
            multi.replace("one.mkv", "bad/one"),
            multi.replace("d6:lengthi2e", "d4:attr1:l6:lengthi2e"),
        ] {
            assert!(inspect_torrent_metadata(&wrap(unsafe_info.as_bytes())).is_err());
        }
        let excessive_files = format!(
            "d5:filesl{}e4:name4:show12:piece lengthi4e6:pieces20:01234567890123456789e",
            "d6:lengthi2e4:pathl7:one.mkvee".repeat(MAX_FILES + 1)
        );
        assert!(inspect_torrent_metadata(&wrap(excessive_files.as_bytes())).is_err());
        assert!(inspect_torrent_metadata(&[b'l'; MAX_DEPTH + 2]).is_err());
        assert!(inspect_torrent_metadata(&vec![0; MAX_BYTES + 1]).is_err());
        let valid = wrap(SINGLE_INFO);
        for end in 0..valid.len() {
            assert!(inspect_torrent_metadata(&valid[..end]).is_err());
        }
        let mut trailing = valid;
        trailing.push(b'e');
        assert!(inspect_torrent_metadata(&trailing).is_err());
    }
}
