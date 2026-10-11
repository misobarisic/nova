use super::*;
use std::{sync::atomic::AtomicUsize, time::Duration};

static TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn serial() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap()
}
fn scratch(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "nova-media-{}-{label}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}
fn source_image(w: u32, h: u32, transparent: bool) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([
            ((x * 17 + y * 13) % 251) as u8,
            ((x * 7 + y * 29) % 253) as u8,
            ((x * 31 + y * 3) % 249) as u8,
            if transparent && (x + y) % 2 == 0 {
                96
            } else {
                255
            },
        ])
    });
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}
fn settings() -> CacheSettings {
    CacheSettings::default()
}
fn disk_variants(dir: &Path) -> Vec<(PathBuf, DiskVariant)> {
    directory_variants(dir).collect()
}

#[test]
fn presets_fixed_override_original_aspect_ratio_no_upscale_and_buffer_lengths() {
    let _serial = serial();
    let settings = settings();
    assert_eq!(
        ImageRecipe::resolve(ImageUse::DetailPoster, &settings).max_side,
        Some(480)
    );
    assert_eq!(
        ImageRecipe::resolve(ImageUse::DetailBackdrop, &settings).max_side,
        Some(960)
    );
    assert_eq!(
        ImageRecipe::resolve(ImageUse::EpisodeThumbnail, &settings).max_side,
        Some(360)
    );
    assert_eq!(
        ImageRecipe::resolve(ImageUse::PlayerEpisodeBackdrop, &settings).max_side,
        None
    );
    assert_eq!(
        ImageRecipe::resolve(ImageUse::Fixed(Some(77)), &settings).max_side,
        Some(77)
    );
    assert_eq!(
        ImageRecipe::resolve(ImageUse::Fixed(None), &settings).max_side,
        None
    );

    let preset_source = source_image(2000, 1000, false);
    let presets = [240, 360, 480, 720, 960, 1024, 1280, 1920, 2560];
    for side in presets {
        for image_use in [
            ImageUse::DetailPoster,
            ImageUse::DetailBackdrop,
            ImageUse::EpisodeThumbnail,
            ImageUse::PlayerEpisodeBackdrop,
        ] {
            let local = match image_use {
                ImageUse::DetailPoster => CacheSettings {
                    cache_images: false,
                    detail_poster_max_side: Some(side),
                    ..settings.clone()
                },
                ImageUse::DetailBackdrop => CacheSettings {
                    cache_images: false,
                    detail_backdrop_max_side: Some(side),
                    ..settings.clone()
                },
                ImageUse::EpisodeThumbnail => CacheSettings {
                    cache_images: false,
                    episode_thumbnail_max_side: Some(side),
                    ..settings.clone()
                },
                ImageUse::PlayerEpisodeBackdrop => CacheSettings {
                    cache_images: false,
                    player_episode_backdrop_max_side: Some(side),
                    ..settings.clone()
                },
                ImageUse::Fixed(_) => unreachable!(),
            };
            assert_eq!(ImageRecipe::resolve(image_use, &local).max_side, Some(side));
            let pixels = cached_pixels_with(
                Path::new("/unused"),
                &format!("preset-{side}-{image_use:?}"),
                image_use,
                &local,
                || Some(source_image(200, 100, false)),
            )
            .unwrap();
            assert_eq!(
                (pixels.width(), pixels.height()),
                (200, 100),
                "preset {side} must not upscale"
            );
            assert_eq!(
                pixels.as_bytes().len(),
                pixels.width() as usize * pixels.height() as usize * 4
            );
        }
    }
    for image_use in [
        ImageUse::DetailPoster,
        ImageUse::DetailBackdrop,
        ImageUse::EpisodeThumbnail,
        ImageUse::PlayerEpisodeBackdrop,
    ] {
        let local = match image_use {
            ImageUse::DetailPoster => CacheSettings {
                cache_images: false,
                detail_poster_max_side: None,
                ..settings.clone()
            },
            ImageUse::DetailBackdrop => CacheSettings {
                cache_images: false,
                detail_backdrop_max_side: None,
                ..settings.clone()
            },
            ImageUse::EpisodeThumbnail => CacheSettings {
                cache_images: false,
                episode_thumbnail_max_side: None,
                ..settings.clone()
            },
            ImageUse::PlayerEpisodeBackdrop => CacheSettings {
                cache_images: false,
                player_episode_backdrop_max_side: None,
                ..settings.clone()
            },
            ImageUse::Fixed(_) => unreachable!(),
        };
        let pixels = cached_pixels_with(
            Path::new("/unused"),
            &format!("preset-original-{image_use:?}"),
            image_use,
            &local,
            || Some(preset_source.clone()),
        )
        .unwrap();
        assert_eq!((pixels.width(), pixels.height()), (2000, 1000));
    }

    for (url, w, h, side, expected) in [
        ("portrait", 680, 1000, 480, (326, 480)),
        ("landscape", 1000, 680, 480, (480, 326)),
        ("square-ish", 813, 1446, 360, (202, 360)),
        ("no-upscale", 300, 200, 480, (300, 200)),
    ] {
        let dir = scratch(url);
        let local = CacheSettings {
            cache_images: false,
            ..settings.clone()
        };
        let bytes = source_image(w, h, false);
        let pixels = cached_pixels_with(&dir, url, ImageUse::Fixed(Some(side)), &local, || {
            Some(bytes)
        })
        .unwrap();
        assert_eq!((pixels.width(), pixels.height()), expected, "{url}");
        assert_eq!(
            pixels.as_bytes().len(),
            pixels.width() as usize * pixels.height() as usize * 4
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn cold_memory_and_disk_hits_return_identical_processed_codec_pixels() {
    let _serial = serial();
    let mut jpeg_low = None;
    let mut jpeg_high = None;
    let mut webp_low = None;
    let mut webp_high = None;
    for (format, quality) in [
        (CacheImageFormat::Jpeg, 25),
        (CacheImageFormat::Jpeg, 91),
        (CacheImageFormat::Webp, 37),
        (CacheImageFormat::Webp, 88),
    ] {
        let dir = scratch("codec");
        let url = format!("https://unit.test/{format:?}/{quality}");
        let bytes = source_image(113, 71, false);
        let local = CacheSettings {
            enabled: true,
            format,
            quality,
            detail_poster_max_side: Some(63),
            ..settings()
        };
        let first = cached_pixels_with(&dir, &url, ImageUse::DetailPoster, &local, || {
            Some(bytes.clone())
        })
        .unwrap();
        let recipe = ImageRecipe::resolve(ImageUse::DetailPoster, &local);
        let path = recipe_disk_path(&dir, &url, recipe);
        let variant = read_variant(&path).expect("processed encoded variant");
        assert!(variant_pixels(&variant, &url, recipe).is_some());
        match (format, quality) {
            (CacheImageFormat::Jpeg, 25) => jpeg_low = Some(variant.encoded.clone()),
            (CacheImageFormat::Jpeg, 91) => jpeg_high = Some(variant.encoded.clone()),
            (CacheImageFormat::Webp, 37) => webp_low = Some(variant.encoded.clone()),
            _ => webp_high = Some(variant.encoded.clone()),
        }
        let memory = cached_pixels_with(&dir, &url, ImageUse::DetailPoster, &local, || {
            panic!("memory hit should not fetch")
        })
        .unwrap();
        assert!(image_buffers_equal(&first, &memory));
        decoded_cache_clear();
        let disk = cached_pixels_with(&dir, &url, ImageUse::DetailPoster, &local, || {
            panic!("disk hit should avoid download")
        })
        .unwrap();
        assert!(image_buffers_equal(&first, &disk));
        fs::remove_dir_all(dir).unwrap();
    }
    assert_ne!(
        jpeg_low, jpeg_high,
        "JPEG quality must affect encoded bytes"
    );
    assert_ne!(
        webp_low, webp_high,
        "WebP quality must affect encoded bytes"
    );
}

#[test]
fn jpeg_alpha_falls_back_to_png_and_compression_off_stores_resized_png() {
    let _serial = serial();
    let dir = scratch("alpha");
    let url = "https://unit.test/alpha";
    let bytes = source_image(39, 22, true);
    let jpeg = CacheSettings {
        enabled: true,
        format: CacheImageFormat::Jpeg,
        quality: 70,
        detail_poster_max_side: Some(22),
        ..settings()
    };
    let _pixels = cached_pixels_with(&dir, url, ImageUse::DetailPoster, &jpeg, || {
        Some(bytes.clone())
    })
    .unwrap();
    let recipe = ImageRecipe::resolve(ImageUse::DetailPoster, &jpeg);
    let path = recipe_disk_path(&dir, url, recipe);
    let encoded = read_variant(&path).unwrap().encoded;
    assert!(encoded.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(
        image::load_from_memory(&encoded)
            .unwrap()
            .into_rgba8()
            .pixels()
            .any(|pixel| pixel.0[3] < 255)
    );

    let plain = CacheSettings {
        enabled: false,
        detail_poster_max_side: Some(18),
        ..settings()
    };
    let plain_url = "https://unit.test/plain";
    cached_pixels_with(&dir, plain_url, ImageUse::DetailPoster, &plain, || {
        Some(bytes)
    })
    .unwrap();
    let variant = read_variant(&recipe_disk_path(
        &dir,
        plain_url,
        ImageRecipe::resolve(ImageUse::DetailPoster, &plain),
    ))
    .unwrap();
    assert!(variant.encoded.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!((variant.metadata.width, variant.metadata.height), (18, 10));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn disk_off_performs_no_reads_or_writes_but_keeps_memory_cache() {
    let _serial = serial();
    let dir = scratch("disk-off");
    let url = "https://unit.test/no-disk";
    let source = source_image(32, 20, false);
    let local = CacheSettings {
        cache_images: false,
        enabled: true,
        format: CacheImageFormat::Webp,
        detail_poster_max_side: Some(16),
        ..settings()
    };
    let first =
        cached_pixels_with(&dir, url, ImageUse::DetailPoster, &local, || Some(source)).unwrap();
    assert!(fs::read_dir(&dir).unwrap().next().is_none());
    let again = cached_pixels_with(&dir, url, ImageUse::DetailPoster, &local, || {
        panic!("memory result expected")
    })
    .unwrap();
    assert!(image_buffers_equal(&first, &again));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_recipe_roles_persist_together_and_settings_change_retires_use_history() {
    let _serial = serial();
    let dir = scratch("roles");
    let url = "https://unit.test/roles";
    let bytes = source_image(900, 600, false);
    let original = settings();
    // DetailBackdrop and a fixed 960 override resolve to the same recipe.
    let pixels = cached_pixels_with(&dir, url, ImageUse::DetailBackdrop, &original, || {
        Some(bytes.clone())
    })
    .unwrap();
    let same = cached_pixels_with(&dir, url, ImageUse::Fixed(Some(960)), &original, || {
        panic!("same recipe memory hit")
    })
    .unwrap();
    assert!(image_buffers_equal(&pixels, &same));
    let recipe = ImageRecipe::resolve(ImageUse::DetailBackdrop, &original);
    let path = recipe_disk_path(&dir, url, recipe);
    let roles = read_variant(&path).unwrap().metadata.uses;
    assert!(
        roles.contains(&ImageUse::DetailBackdrop) && roles.contains(&ImageUse::Fixed(Some(960)))
    );

    let changed = CacheSettings {
        detail_backdrop_max_side: Some(720),
        ..original
    };
    let fetches = AtomicUsize::new(0);
    let changed_pixels = cached_pixels_with(&dir, url, ImageUse::DetailBackdrop, &changed, || {
        fetches.fetch_add(1, Ordering::SeqCst);
        Some(bytes.clone())
    })
    .unwrap();
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        1,
        "a new recipe must download again"
    );
    assert_eq!(changed_pixels.width().max(changed_pixels.height()), 720);
    let variants = disk_variants(&dir);
    assert_eq!(
        variants.len(),
        2,
        "the old variant remains for the fixed-960 use"
    );
    assert!(
        variants
            .iter()
            .any(|(_, v)| v.metadata.uses == vec![ImageUse::Fixed(Some(960))])
    );
    assert!(
        variants
            .iter()
            .any(|(_, v)| v.metadata.uses.contains(&ImageUse::DetailBackdrop))
    );

    // The old fixed role is migrated independently; once it has a replacement,
    // the history file is removed and the new role remains registered.
    cached_pixels_with(&dir, url, ImageUse::Fixed(Some(960)), &changed, || {
        Some(bytes)
    })
    .unwrap();
    assert_eq!(disk_variants(&dir).len(), 2);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn corrupt_envelopes_checksum_and_encoded_bytes_refetch() {
    let _serial = serial();
    let dir = scratch("corrupt");
    let url = "https://unit.test/corrupt";
    let source = source_image(50, 30, false);
    let local = settings();
    let use_ = ImageUse::Fixed(Some(40));
    let recipe = ImageRecipe::resolve(use_, &local);
    cached_pixels_with(&dir, url, use_, &local, || Some(source.clone())).unwrap();
    let path = recipe_disk_path(&dir, url, recipe);
    for corruption in 0..4 {
        let valid = fs::read(&path).unwrap();
        let mut damaged = valid.clone();
        match corruption {
            0 => damaged[0] = b'X',
            1 | 2 => {
                let meta_len = u32::from_le_bytes(damaged[8..12].try_into().unwrap()) as usize;
                let mut metadata: VariantMetadata =
                    serde_json::from_slice(&damaged[12..12 + meta_len]).unwrap();
                if corruption == 1 {
                    metadata.url.push_str("/tampered");
                } else {
                    metadata.checksum ^= 1;
                }
                let bytes = serde_json::to_vec(&metadata).unwrap();
                damaged.truncate(0);
                damaged.extend_from_slice(MAGIC);
                damaged.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                damaged.extend_from_slice(&bytes);
                damaged.extend_from_slice(&valid[12 + meta_len..]);
            }
            _ => *damaged.last_mut().unwrap() ^= 0x55,
        }
        fs::write(&path, damaged).unwrap();
        decoded_cache_clear();
        let calls = AtomicUsize::new(0);
        cached_pixels_with(&dir, url, use_, &local, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(source.clone())
        })
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(variant_pixels(&read_variant(&path).unwrap(), url, recipe).is_some());
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_encoder_returns_source_pixels_without_claiming_recipe_or_overwriting_valid_variant() {
    let _serial = serial();
    let dir = scratch("encoder-failure");
    let url = "https://unit.test/encoder-failure";
    let bytes = source_image(46, 30, false);
    let recipe = ImageRecipe::resolve(ImageUse::Fixed(Some(30)), &settings());
    let initial = process_source(bytes.clone(), recipe).unwrap();
    let path = recipe_disk_path(&dir, url, recipe);
    persist_processed(&dir, url, &[ImageUse::Fixed(Some(30))], recipe, &initial).unwrap();
    let previous = fs::read(&path).unwrap();
    decoded_cache_clear();
    let output = cached_pixels_with(&dir, url, ImageUse::Fixed(Some(30)), &settings(), || {
        Some(bytes.clone())
    })
    .unwrap();
    // Force an encoder failure through the injectable core seam; the decoded
    // source remains usable, while no cache recipe is published or replaced.
    let failed = process_source_with(bytes, recipe, |_, _| None).unwrap();
    assert!(failed.encoded.is_none());
    assert_eq!(failed.pixels.width().max(failed.pixels.height()), 30);
    assert!(output.width() > 0);
    assert!(persist_processed(&dir, url, &[ImageUse::Fixed(Some(30))], recipe, &failed).is_err());
    assert_eq!(fs::read(path).unwrap(), previous);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn atomic_write_failure_preserves_previous_valid_variant() {
    let _serial = serial();
    let dir = scratch("atomic-failure");
    let url = "https://unit.test/atomic";
    let recipe = ImageRecipe::resolve(ImageUse::Fixed(Some(20)), &settings());
    let path = recipe_disk_path(&dir, url, recipe);
    let source = process_source(source_image(20, 20, false), recipe).unwrap();
    persist_processed(&dir, url, &[ImageUse::Fixed(Some(20))], recipe, &source).unwrap();
    let before = fs::read(&path).unwrap();
    let sequence = TEMP_SEQUENCE.load(Ordering::Relaxed);
    let blocker = path.with_file_name(format!(
        "{}.tmp-{}-{sequence}",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ));
    fs::create_dir(&blocker).unwrap();
    assert!(write_variant(&path, &disk_variants(&dir)[0].1).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn concurrent_identical_success_and_failure_coalesce_but_distinct_recipes_do_not() {
    let _serial = serial();
    for fail in [false, true] {
        let dir = scratch("flight");
        let url = if fail {
            "https://unit.test/flight-fail"
        } else {
            "https://unit.test/flight-ok"
        };
        let local = CacheSettings {
            cache_images: false,
            ..settings()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let dir = dir.clone();
            let calls = calls.clone();
            let gate = gate.clone();
            let local = local.clone();
            workers.push(std::thread::spawn(move || {
                gate.wait();
                cached_pixels_with(&dir, url, ImageUse::Fixed(Some(24)), &local, || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(40));
                    if fail {
                        None
                    } else {
                        Some(source_image(40, 30, false))
                    }
                })
            }));
        }
        gate.wait();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            results.iter().filter(|result| result.is_some()).count(),
            if fail { 0 } else { 2 }
        );
        fs::remove_dir_all(dir).unwrap();
    }
    let dir = scratch("distinct-flights");
    let url = "https://unit.test/distinct";
    let local = CacheSettings {
        cache_images: false,
        ..settings()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for side in [17, 23] {
        let dir = dir.clone();
        let calls = calls.clone();
        let gate = gate.clone();
        let local = local.clone();
        workers.push(std::thread::spawn(move || {
            gate.wait();
            cached_pixels_with(&dir, url, ImageUse::Fixed(Some(side)), &local, || {
                calls.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(30));
                Some(source_image(40, 30, false))
            })
        }));
    }
    gate.wait();
    for worker in workers {
        assert!(worker.join().unwrap().is_some());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn clear_during_blocked_download_discards_completion_and_leaves_no_cache_files() {
    let _serial = serial();
    decoded_cache_clear();
    let dir = scratch("clear-flight");
    let url = "https://unit.test/clear-flight";
    let local = settings();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let thread_dir = dir.clone();
    let worker = std::thread::spawn(move || {
        cached_pixels_with(&thread_dir, url, ImageUse::Fixed(Some(28)), &local, || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(source_image(44, 28, false))
        })
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    clear_poster_cache_dir(&dir);
    release_tx.send(()).unwrap();
    assert!(worker.join().unwrap().is_none());
    assert_eq!(decoded_cache_stats().0, 0);
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn maintenance_fetches_each_url_once_rebuilds_registered_uses_and_counts_cancel_or_failure() {
    let _serial = serial();
    let dir = scratch("maintenance");
    let url = "https://unit.test/maintenance";
    let bytes = source_image(1000, 600, false);
    let initial = settings();
    for image_use in [
        ImageUse::Fixed(Some(320)),
        ImageUse::DetailBackdrop,
        ImageUse::Fixed(None),
    ] {
        cached_pixels_with(&dir, url, image_use, &initial, || Some(bytes.clone())).unwrap();
    }
    let changed = CacheSettings {
        detail_backdrop_max_side: Some(720),
        ..initial
    };
    let calls = AtomicUsize::new(0);
    let progress = rewrite_entries_with(
        &dir,
        &changed,
        &AtomicBool::new(false),
        |_| {},
        |seen| {
            assert_eq!(seen, url);
            calls.fetch_add(1, Ordering::SeqCst);
            Some(bytes.clone())
        },
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(progress.processed, 1);
    assert_eq!(progress.failed, 0);
    assert_eq!(progress.converted, 3);
    let variants = disk_variants(&dir);
    assert!(variants.iter().any(
        |(_, v)| v.metadata.uses.contains(&ImageUse::Fixed(Some(320))) && v.metadata.width == 320
    ));
    assert!(variants.iter().any(
        |(_, v)| v.metadata.uses.contains(&ImageUse::DetailBackdrop) && v.metadata.width == 720
    ));
    assert!(
        variants
            .iter()
            .any(|(_, v)| v.metadata.uses.contains(&ImageUse::Fixed(None))
                && v.metadata.width == 1000)
    );
    let failed = rewrite_entries_with(&dir, &changed, &AtomicBool::new(false), |_| {}, |_| None);
    assert_eq!(failed.failed, 1);
    let cancel = AtomicBool::new(true);
    let cancelled = rewrite_entries_with(
        &dir,
        &changed,
        &cancel,
        |_| {},
        |_| panic!("cancelled before fetching"),
    );
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.processed, 0);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn maintenance_reencodes_current_quality_and_size_for_jpeg_and_webp() {
    let _serial = serial();
    for format in [CacheImageFormat::Jpeg, CacheImageFormat::Webp] {
        let dir = scratch("maintenance-codec");
        let url = format!("https://unit.test/maintenance-codec/{format:?}");
        let bytes = source_image(1000, 600, false);
        let initial = CacheSettings {
            detail_poster_max_side: Some(480),
            ..settings()
        };
        cached_pixels_with(&dir, &url, ImageUse::DetailPoster, &initial, || {
            Some(bytes.clone())
        })
        .unwrap();
        let target = CacheSettings {
            enabled: true,
            format,
            quality: 63,
            detail_poster_max_side: Some(720),
            ..initial
        };
        let calls = AtomicUsize::new(0);
        let progress = rewrite_entries_with(
            &dir,
            &target,
            &AtomicBool::new(false),
            |_| {},
            |seen| {
                assert_eq!(seen, url);
                calls.fetch_add(1, Ordering::SeqCst);
                Some(bytes.clone())
            },
        );
        assert_eq!(progress.converted, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let recipe = ImageRecipe::resolve(ImageUse::DetailPoster, &target);
        let variant = read_variant(&recipe_disk_path(&dir, &url, recipe)).unwrap();
        assert_eq!(
            (variant.metadata.width, variant.metadata.height),
            (720, 432)
        );
        let expected = process_source(bytes, recipe).unwrap();
        let actual = variant_pixels(&variant, &url, recipe).unwrap();
        assert!(image_buffers_equal(&expected.pixels, &actual));
        if format == CacheImageFormat::Jpeg {
            assert!(variant.encoded.starts_with(&[0xff, 0xd8, 0xff]));
        } else {
            assert!(
                variant.encoded.starts_with(b"RIFF")
                    && variant.encoded.windows(4).any(|part| part == b"WEBP")
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn decoded_lru_evicts_by_byte_budget() {
    let _serial = serial();
    let local = CacheSettings {
        cache_images: false,
        ..settings()
    };
    set_decoded_cache_budget(1);
    decoded_cache_clear();
    let mut returned = Vec::new();
    for index in 0..100 {
        let url = format!("https://unit.test/catalog/{index}");
        returned.push(
            cached_pixels_with(
                Path::new("/unused"),
                &url,
                ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                &local,
                || Some(source_image(256, 256, false)),
            )
            .unwrap(),
        );
    }
    // The removed desktop store kept up to 400 grid buffers outside this
    // budget. This 100-poster workload fits that store; compare its retained
    // pixels with the sole shared cache after scrolling those cards away.
    let previous_store_bytes: usize = returned.iter().map(|p| p.as_bytes().len()).sum();
    let catalog = (decoded_cache_stats().0, decoded_cache_stats().1);
    assert!(catalog.0 > 0 && catalog.1 <= 1024 * 1024);
    let detail_url = "https://unit.test/detail-large";
    let detail = cached_pixels_with(
        Path::new("/unused"),
        detail_url,
        ImageUse::DetailBackdrop,
        &local,
        || Some(source_image(2000, 1000, false)),
    )
    .unwrap();
    assert_eq!(detail.width().max(detail.height()), 960);
    let player_url = "https://unit.test/player-original";
    let player = cached_pixels_with(
        Path::new("/unused"),
        player_url,
        ImageUse::PlayerEpisodeBackdrop,
        &local,
        || Some(source_image(1800, 1200, false)),
    )
    .unwrap();
    assert_eq!((player.width(), player.height()), (1800, 1200));
    let after = decoded_cache_stats();
    eprintln!(
        "100-poster retention: previous separate store {previous_store_bytes} bytes; shared cache after scroll {} bytes; after Detail/player {} bytes (1 MiB cache budget; visible buffers excluded)",
        catalog.1, after.1
    );
    assert!(after.1 < previous_store_bytes);
    assert!(after.1 <= 1024 * 1024);
    assert!(after.1 < catalog.1 + detail.as_bytes().len() + player.as_bytes().len());
    // Slint buffers held by the UI remain usable after their LRU entries are evicted.
    assert!(
        returned.iter().all(|pixels| pixels.as_bytes().len()
            == pixels.width() as usize * pixels.height() as usize * 4)
    );
    assert_eq!(
        detail.as_bytes().len(),
        detail.width() as usize * detail.height() as usize * 4
    );
    assert_eq!(
        player.as_bytes().len(),
        player.width() as usize * player.height() as usize * 4
    );
    decoded_cache_clear();
    set_decoded_cache_budget(1);
    decoded_cache_insert("lru-a", SharedPixelBuffer::<Rgba8Pixel>::new(512, 256));
    decoded_cache_insert("lru-b", SharedPixelBuffer::<Rgba8Pixel>::new(512, 256));
    assert!(decoded_cache_get("lru-a").is_some());
    decoded_cache_insert("lru-c", SharedPixelBuffer::<Rgba8Pixel>::new(512, 256));
    assert!(decoded_cache_contains("lru-a"));
    assert!(!decoded_cache_contains("lru-b"));
    assert!(decoded_cache_contains("lru-c"));
    set_decoded_cache_budget(0);
    assert_eq!(decoded_cache_stats(), (0, 0));
    decoded_cache_clear();
    set_decoded_cache_budget(128);
}

#[test]
fn identical_refreshes_share_download_and_register_each_role() {
    let _serial = serial();
    let dir = scratch("refresh-flight");
    decoded_cache_clear();
    let local = CacheSettings {
        cache_images: true,
        enabled: false,
        detail_poster_max_side: Some(240),
        ..settings()
    };
    let url = "https://unit.test/refresh-shared";
    let epoch = LoadEpoch::capture();
    let bytes = source_image(300, 150, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut joins = Vec::new();
    for image_use in [ImageUse::DetailPoster, ImageUse::Fixed(Some(240))] {
        let dir = dir.clone();
        let bytes = bytes.clone();
        let calls = calls.clone();
        let barrier = barrier.clone();
        let local = local.clone();
        joins.push(std::thread::spawn(move || {
            barrier.wait();
            refresh_image_if_changed_with(&dir, url, image_use, &local, epoch, || {
                calls.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(80));
                Some(bytes)
            })
        }));
    }
    barrier.wait();
    let results: Vec<_> = joins.into_iter().map(|join| join.join().unwrap()).collect();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(results.iter().all(Option::is_some));
    let recipe = ImageRecipe::resolve(ImageUse::DetailPoster, &local);
    let variant = read_variant(&recipe_disk_path(&dir, url, recipe)).unwrap();
    assert!(variant.metadata.uses.contains(&ImageUse::DetailPoster));
    assert!(variant.metadata.uses.contains(&ImageUse::Fixed(Some(240))));

    let second_calls = AtomicUsize::new(0);
    let unchanged =
        refresh_image_if_changed_with(&dir, url, ImageUse::DetailPoster, &local, epoch, || {
            second_calls.fetch_add(1, Ordering::SeqCst);
            Some(bytes)
        });
    assert!(
        unchanged.is_none(),
        "same processed pixels should not refresh UI"
    );
    assert_eq!(second_calls.load(Ordering::SeqCst), 1);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_refresh_flight_completes_every_waiter() {
    let _serial = serial();
    let dir = scratch("refresh-failure");
    decoded_cache_clear();
    let local = CacheSettings {
        cache_images: false,
        ..settings()
    };
    let url = "https://unit.test/refresh-failure";
    let epoch = LoadEpoch::capture();
    let calls = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut joins = Vec::new();
    for _ in 0..2 {
        let dir = dir.clone();
        let calls = calls.clone();
        let barrier = barrier.clone();
        let local = local.clone();
        joins.push(std::thread::spawn(move || {
            barrier.wait();
            refresh_image_if_changed_with(
                &dir,
                url,
                ImageUse::Fixed(Some(240)),
                &local,
                epoch,
                || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(80));
                    None
                },
            )
        }));
    }
    barrier.wait();
    assert!(joins.into_iter().all(|join| join.join().unwrap().is_none()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(decoded_cache_stats().0, 0);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn settings_revision_discards_blocked_normal_and_refresh_completions() {
    let _serial = serial();
    let original = nova_config::image_settings_snapshot().0;
    decoded_cache_clear();
    let initial = CacheSettings {
        cache_images: true,
        enabled: true,
        format: CacheImageFormat::Jpeg,
        quality: 71,
        detail_poster_max_side: Some(480),
        ..settings()
    };
    nova_config::set_cache_settings(initial.clone());
    let dir = scratch("stale-settings-normal");
    let url = "https://unit.test/stale-settings-normal";
    let old_key =
        image_recipe_cache_key(url, ImageRecipe::resolve(ImageUse::DetailPoster, &initial));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let local = initial.clone();
    let worker_dir = dir.clone();
    let worker = std::thread::spawn(move || {
        cached_pixels_with(&worker_dir, url, ImageUse::DetailPoster, &local, || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(source_image(900, 450, false))
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    nova_config::set_cache_settings(CacheSettings {
        detail_poster_max_side: Some(720),
        ..initial.clone()
    });
    release_tx.send(()).unwrap();
    let normal_result = worker.join().unwrap();
    let normal_variant_count = disk_variants(&dir).len();
    let old_memory = decoded_cache_get(&old_key).is_some();

    // Repeat through the always-refresh path: an old recipe completion must
    // not write or publish pixels after the settings revision changes either.
    nova_config::set_cache_settings(initial.clone());
    let refresh_dir = scratch("stale-settings-refresh");
    let refresh_url = "https://unit.test/stale-settings-refresh";
    let epoch = LoadEpoch::capture();
    let (refresh_entered_tx, refresh_entered_rx) = std::sync::mpsc::channel();
    let (refresh_release_tx, refresh_release_rx) = std::sync::mpsc::channel();
    let refresh_local = initial.clone();
    let worker_dir = refresh_dir.clone();
    let refresh_worker = std::thread::spawn(move || {
        refresh_image_if_changed_with(
            &worker_dir,
            refresh_url,
            ImageUse::DetailPoster,
            &refresh_local,
            epoch,
            || {
                refresh_entered_tx.send(()).unwrap();
                refresh_release_rx.recv().unwrap();
                Some(source_image(900, 450, false))
            },
        )
    });
    refresh_entered_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    nova_config::set_cache_settings(CacheSettings {
        detail_poster_max_side: Some(720),
        ..initial
    });
    refresh_release_tx.send(()).unwrap();
    let refresh_result = refresh_worker.join().unwrap();
    let refresh_variant_count = disk_variants(&refresh_dir).len();

    nova_config::set_cache_settings(original);
    decoded_cache_clear();
    assert!(normal_result.is_none());
    assert_eq!(normal_variant_count, 0);
    assert!(!old_memory);
    assert!(refresh_result.is_none());
    assert_eq!(refresh_variant_count, 0);
    fs::remove_dir_all(dir).unwrap();
    fs::remove_dir_all(refresh_dir).unwrap();
}
