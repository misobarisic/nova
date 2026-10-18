//! Standalone app information and license navigation.
use super::*;

impl Bridge {
    pub(super) fn show_about_page(&self) {
        if let Some(app) = self.app() {
            app.set_show_home(false);
            app.set_show_library(false);
            app.set_show_settings(false);
            app.set_show_about(true);
        }
    }
}

pub(super) fn prepare_license_text(app: &AppWindow) {
    let weak = app.as_weak();
    thread::spawn(move || {
        let chunks = match decode_license_catalog() {
            Ok(catalog) => license_text_chunks(&catalog),
            Err(error) => {
                eprintln!("Nova: could not decode bundled license catalog: {error}");
                return;
            }
        };
        let _ = crate::app::profiles::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_license_text_chunks(Rc::new(VecModel::from(chunks)).into());
            }
        });
    });
}

fn decode_license_catalog() -> std::io::Result<String> {
    // Allocate once at the compile-time size, and pass the same bound to zstd.
    // No guessed capacity or growing output buffer is needed for bundled data.
    let mut bytes = vec![0; OPEN_SOURCE_LICENSE_CATALOG_SIZE];
    let written =
        zstd::bulk::decompress_to_buffer(OPEN_SOURCE_LICENSE_CATALOG_ZSTD, &mut bytes[..])?;
    if written != bytes.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bundled license catalog size mismatch",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

// Bound every Text layout independently, including catalogs with unbroken
// license identifiers. Slice only at Unicode scalar boundaries and retain all
// characters so the model remains an exact representation of the legal text.
pub(super) fn license_text_chunks(catalog: &str) -> Vec<SharedString> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < catalog.len() {
        let (mut end, mut last_newline, mut last_space, mut lines) = (start, None, None, 0);
        for (offset, ch) in catalog[start..].char_indices().take(512) {
            end = start + offset + ch.len_utf8();
            if ch.is_whitespace() {
                last_space = Some(end);
            }
            if ch == '\n' {
                last_newline = Some(end);
                lines += 1;
                if lines == 16 {
                    break;
                }
            }
        }
        // Prefer a complete line, then a word boundary. Hard-split only
        // unbroken tokens; every split remains within the same work bound.
        let boundary = if end == catalog.len() {
            end
        } else {
            last_newline.or(last_space).unwrap_or(end)
        };
        chunks.push(catalog[start..boundary].into());
        start = boundary;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::{decode_license_catalog, license_text_chunks};

    #[test]
    fn bundled_license_catalog_decodes_without_changing_legal_text() {
        let original = include_str!(concat!(env!("OUT_DIR"), "/nova_license_catalog.txt"));
        let decoded = decode_license_catalog().unwrap();
        assert_eq!(decoded, original);
        assert_eq!(decoded.len(), super::OPEN_SOURCE_LICENSE_CATALOG_SIZE);
        assert!(super::OPEN_SOURCE_LICENSE_CATALOG_ZSTD.len() < decoded.len() / 2);
        assert_eq!(
            license_text_chunks(&decoded)
                .iter()
                .map(|s| s.as_str())
                .collect::<String>(),
            original
        );
    }

    #[test]
    fn license_chunks_preserve_utf8_content_and_bound_text_layout_work() {
        for catalog in [
            String::new(),
            "ž漢🙂".repeat(1000),
            "line\n".repeat(1000),
            format!("{}\n{}", "GPL ž🙂\n".repeat(20000), "X".repeat(10000)),
        ] {
            let chunks = license_text_chunks(&catalog);
            assert_eq!(
                chunks.iter().map(|s| s.as_str()).collect::<String>(),
                catalog
            );
            for chunk in chunks {
                assert!(!chunk.is_empty());
                assert!(chunk.chars().count() <= 512);
                assert!(chunk.chars().filter(|&ch| ch == '\n').count() <= 16);
            }
        }
    }
}
