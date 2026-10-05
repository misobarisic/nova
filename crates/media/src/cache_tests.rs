use super::*;
#[cfg(test)]
mod image_cache_tests {
    use super::*;

    fn solid_rgba(w: u32, h: u32) -> SharedPixelBuffer<Rgba8Pixel> {
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
        for b in buf.make_mut_bytes().iter_mut() {
            *b = 128;
        }
        buf
    }

    #[test]
    fn downscale_dims_math() {
        assert_eq!(downscale_dims(1200, 600), Some((1024, 512)));
        assert_eq!(downscale_dims(500, 300), None);
        assert_eq!(downscale_dims(1024, 768), None);
        let (w, h) = downscale_dims(3000, 2000).unwrap();
        assert!(w <= nova_config::IMAGE_DOWNSCALE_MAX && h <= nova_config::IMAGE_DOWNSCALE_MAX);
    }

    #[test]
    fn display_resize_keeps_dimensions_consistent_with_pixel_storage() {
        // TVDB's 680×1000 posters previously produced a 326×479 image in a
        // buffer labelled 326×480. Skia rejects that undersized pixel data.
        for (w, h) in [(680, 1000), (1000, 680), (813, 1446), (1, 2000), (300, 450)] {
            let pixels = downscale_for_display(&solid_rgba(w, h), DISPLAY_POSTER_SIDE);
            assert_eq!(
                pixels.as_bytes().len(),
                pixels.width() as usize * pixels.height() as usize * 4,
                "invalid display buffer for {w}×{h}"
            );
            assert!(pixels.width() <= DISPLAY_POSTER_SIDE);
            assert!(pixels.height() <= DISPLAY_POSTER_SIDE);
            if w.max(h) <= DISPLAY_POSTER_SIDE {
                assert_eq!((pixels.width(), pixels.height()), (w, h));
            }
        }
    }

    #[test]
    fn encode_disabled_is_passthrough() {
        let settings = CacheSettings {
            enabled: false,
            ..CacheSettings::default()
        };
        assert!(encode_for_cache(&settings, &solid_rgba(16, 16)).is_none());
    }

    #[test]
    fn webp_encode_has_webp_magic() {
        let settings = CacheSettings {
            enabled: true,
            format: CacheImageFormat::Webp,
            quality: 75,
            downscale: true,
            ..CacheSettings::default()
        };
        let bytes = encode_for_cache(&settings, &solid_rgba(40, 30)).expect("webp bytes");
        assert!(bytes.starts_with(b"RIFF") && bytes.windows(4).any(|w| w == b"WEBP"));
    }

    #[test]
    fn jpeg_encode_has_jpeg_magic() {
        let settings = CacheSettings {
            enabled: true,
            format: CacheImageFormat::Jpeg,
            quality: 75,
            downscale: true,
            ..CacheSettings::default()
        };
        let bytes = encode_for_cache(&settings, &solid_rgba(40, 30)).expect("jpeg bytes");
        assert!(bytes.starts_with(&[0xFF, 0xD8, 0xFF]));
    }
}

#[cfg(test)]
mod rewrite_cache_tests {
    use super::*;
    use std::io::Cursor;

    fn scratch(sub: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nova-rewrite-test-{}-{sub}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn png_bytes() -> Vec<u8> {
        let mut buf = Vec::new();
        let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([200u8, 40, 30, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn artwork_download_retries_transient_http_errors_before_decoding() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            time::{Duration, Instant},
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/art.png", listener.local_addr().unwrap());
        let bytes = png_bytes();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            for (status, body) in [("503 Service Unavailable", Vec::new()), ("200 OK", bytes)] {
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "expected image retry");
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("image fixture: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = [0; 2048];
                let received = stream.read(&mut request).unwrap();
                assert!(received > 0, "expected an image GET request");
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let image = download_image_fresh(&url, None).expect("second response provides artwork");
        assert_eq!((image.pixels.width(), image.pixels.height()), (8, 8));
        server.join().unwrap();
    }

    fn jpeg_settings() -> CacheSettings {
        CacheSettings {
            enabled: true,
            format: CacheImageFormat::Jpeg,
            quality: 75,
            downscale: false,
            ..CacheSettings::default()
        }
    }

    #[test]
    fn rewrites_unencoded_entries_and_is_idempotent() {
        let dir = scratch("sweep");
        let png = png_bytes();
        fs::write(dir.join("1111111111111111.img"), &png).unwrap();
        fs::write(dir.join("2222222222222222.img"), &png).unwrap();
        // An entry whose sidecar already matches the target config is skipped.
        let settings = jpeg_settings();
        let key = settings.config_key();
        fs::write(dir.join("3333333333333333.img"), &png).unwrap();
        fs::write(dir.join("3333333333333333.cfg"), &key).unwrap();

        assert_eq!(rewrite_cache_dir_to_format(&dir, &settings), 2);
        for n in ["1111111111111111", "2222222222222222"] {
            let bytes = fs::read(dir.join(format!("{n}.img"))).unwrap();
            assert!(bytes.starts_with(&[0xFF, 0xD8, 0xFF]), "jpeg magic for {n}");
            assert_eq!(
                fs::read_to_string(dir.join(format!("{n}.cfg"))).unwrap(),
                key
            );
        }
        // The already-matching entry keeps its original bytes.
        assert_eq!(fs::read(dir.join("3333333333333333.img")).unwrap(), png);
        // A second pass has nothing left to rewrite.
        assert_eq!(rewrite_cache_dir_to_format(&dir, &settings), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_reencoding_rewrites_nothing() {
        let dir = scratch("disabled");
        let png = png_bytes();
        fs::write(dir.join("1111111111111111.img"), &png).unwrap();
        let settings = CacheSettings {
            enabled: false,
            ..jpeg_settings()
        };
        assert_eq!(rewrite_cache_dir_to_format(&dir, &settings), 0);
        assert_eq!(fs::read(dir.join("1111111111111111.img")).unwrap(), png);
        assert!(!dir.join("1111111111111111.cfg").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}

mod parity {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::AtomicUsize;

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "nova-cache-parity-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            w,
            h,
            image::Rgba([20, 40, 80, 255]),
        ))
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
        out.into_inner()
    }
    fn encoding(format: CacheImageFormat, downscale: bool) -> CacheSettings {
        CacheSettings {
            enabled: true,
            lazy_reencode: false,
            format,
            downscale,
            ..CacheSettings::default()
        }
    }

    #[test]
    fn both_encoders_decode_and_honor_downscale_on_new_downloads_without_lazy() {
        let dir = Scratch::new();
        let original = png(1400, 700);
        for format in [CacheImageFormat::Jpeg, CacheImageFormat::Webp] {
            for downscale in [false, true] {
                let settings = encoding(format, downscale);
                store_download(&dir.0, "poster", &original, &settings).unwrap();
                let bytes = read_poster_bytes(&dir.0, "poster").unwrap();
                let image = image::load_from_memory(&bytes).unwrap();
                assert_eq!(
                    (image.width(), image.height()),
                    if downscale { (1024, 512) } else { (1400, 700) }
                );
                assert_eq!(
                    image::guess_format(&bytes).unwrap(),
                    if format == CacheImageFormat::Jpeg {
                        image::ImageFormat::Jpeg
                    } else {
                        image::ImageFormat::WebP
                    }
                );
                assert_eq!(
                    fs::read_to_string(
                        poster_cache_path_in(&dir.0, "poster").with_extension("cfg")
                    )
                    .unwrap(),
                    settings.config_key()
                );
            }
        }
    }

    #[test]
    fn raw_replacements_remove_sidecars_and_invalid_downloads_keep_valid_entries() {
        let dir = Scratch::new();
        let original = png(24, 12);
        let path = poster_cache_path_in(&dir.0, "raw");
        store_download(
            &dir.0,
            "raw",
            &original,
            &encoding(CacheImageFormat::Jpeg, false),
        )
        .unwrap();
        store_download(&dir.0, "raw", &original, &CacheSettings::default()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!path.with_extension("cfg").exists());
        assert!(store_download(&dir.0, "raw", b"corrupt", &CacheSettings::default()).is_err());
        write_poster_bytes(&dir.0, "raw", b"corrupt");
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn lazy_access_invalidates_memory_hits_and_derivatives_after_configuration_change() {
        let dir = Scratch::new();
        let url = "https://nova-test/lazy-hit";
        let source = png(1200, 600);
        store_download(&dir.0, url, &source, &CacheSettings::default()).unwrap();
        let key = sized_cache_key(url, Some(480));
        decoded_cache_insert(&key, decode_image_bytes(&png(480, 240)).unwrap());
        let derivative = dir
            .0
            .join(format!("{:016x}.d480.jpg", fnv1a(url.as_bytes())));
        fs::write(&derivative, &source).unwrap();
        let settings = CacheSettings {
            lazy_reencode: true,
            ..encoding(CacheImageFormat::Jpeg, false)
        };
        ensure_lazy_conversion_in(&dir.0, url, &settings);
        assert!(decoded_cache_get(&key).is_none());
        assert!(!derivative.exists());
        let bytes = read_poster_bytes(&dir.0, url).unwrap();
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 1200);
        let next = CacheSettings {
            quality: 55,
            downscale: true,
            ..settings
        };
        ensure_lazy_conversion_in(&dir.0, url, &next);
        assert_eq!(
            image::load_from_memory(&read_poster_bytes(&dir.0, url).unwrap())
                .unwrap()
                .width(),
            1024
        );
        assert_eq!(
            fs::read_to_string(poster_cache_path_in(&dir.0, url).with_extension("cfg")).unwrap(),
            next.config_key()
        );
    }

    #[test]
    fn bulk_reports_progress_cancellation_corruption_and_idempotence() {
        let dir = Scratch::new();
        for i in 0..4 {
            write_poster_bytes(&dir.0, &format!("poster-{i}"), &png(16, 16));
        }
        fs::write(dir.0.join("corrupt.img"), b"bad").unwrap();
        let settings = encoding(CacheImageFormat::Webp, false);
        fs::write(dir.0.join("corrupt.cfg"), settings.config_key()).unwrap();
        let cancel = AtomicBool::new(false);
        let mut events = Vec::new();
        let stopped = rewrite_cache_dir_with_progress(&dir.0, &settings, &cancel, |p| {
            events.push(p);
            if p.processed == 2 {
                cancel.store(true, Ordering::Release);
            }
        });
        assert!(stopped.cancelled);
        assert_eq!(stopped.processed, 2);
        assert_eq!(stopped.total, 5);
        assert!(events.windows(2).all(|p| p[0].processed <= p[1].processed));
        let finished =
            rewrite_cache_dir_with_progress(&dir.0, &settings, &AtomicBool::new(false), |_| {});
        assert_eq!(finished.processed, 5);
        assert_eq!(finished.failed, 1);
        assert_eq!(finished.converted + finished.skipped, 4);
        let repeated =
            rewrite_cache_dir_with_progress(&dir.0, &settings, &AtomicBool::new(false), |_| {});
        assert_eq!(
            (repeated.converted, repeated.skipped, repeated.failed),
            (0, 4, 1)
        );
        assert_eq!(fs::read(dir.0.join("corrupt.img")).unwrap(), b"bad");
    }

    #[test]
    fn failed_atomic_write_preserves_valid_entry_and_does_not_claim_configuration() {
        let dir = Scratch::new();
        let original = png(16, 16);
        write_poster_bytes(&dir.0, "blocked", &original);
        let path = poster_cache_path_in(&dir.0, "blocked");
        fs::create_dir(path.with_extension("img.tmp")).unwrap();
        assert!(
            store_download(
                &dir.0,
                "blocked",
                &original,
                &encoding(CacheImageFormat::Jpeg, true)
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!path.with_extension("cfg").exists());
    }

    #[test]
    fn concurrent_raw_writes_and_conversions_keep_files_valid_and_markers_consistent() {
        let dir = Scratch::new();
        let original = png(64, 32);
        write_poster_bytes(&dir.0, "shared", &original);
        std::thread::scope(|scope| {
            for i in 0..8 {
                let dir = &dir.0;
                let original = &original;
                scope.spawn(move || {
                    for _ in 0..8 {
                        if i % 2 == 0 {
                            write_poster_bytes(dir, "shared", original);
                        } else {
                            let settings = CacheSettings {
                                lazy_reencode: true,
                                ..encoding(CacheImageFormat::Jpeg, false)
                            };
                            ensure_lazy_conversion_in(dir, "shared", &settings);
                        }
                        image::load_from_memory(&read_poster_bytes(dir, "shared").unwrap())
                            .unwrap();
                    }
                });
            }
        });
        let path = poster_cache_path_in(&dir.0, "shared");
        let bytes = fs::read(&path).unwrap();
        if path.with_extension("cfg").exists() {
            assert_eq!(
                image::guess_format(&bytes).unwrap(),
                image::ImageFormat::Jpeg
            );
        } else {
            assert_eq!(bytes, original);
        }
    }
}

mod source_pipeline {
    use super::*;
    use std::io::Cursor;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            w,
            h,
            image::Rgba([10, 20, 30, 255]),
        ))
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
        out.into_inner()
    }
    #[test]
    fn lazy_conversion_on_a_memory_hit_uses_original_not_display_thumbnail() {
        let dir = std::env::temp_dir().join(format!("nova-source-memory-{}", std::process::id()));
        let url = "https://nova-test/original-on-memory-hit";
        let original = png(1400, 700);
        let raw = CacheSettings::default();
        let first =
            cached_pixels_with(&dir, url, Some(480), &raw, || Some(original.clone())).unwrap();
        assert_eq!(first.width(), 480);
        assert_eq!(read_poster_bytes(&dir, url).unwrap(), original);
        let compressed = CacheSettings {
            enabled: true,
            lazy_reencode: true,
            downscale: false,
            format: CacheImageFormat::Jpeg,
            ..raw
        };
        let second = cached_pixels_with(&dir, url, Some(480), &compressed, || {
            panic!("disk hit must not download")
        })
        .unwrap();
        assert_eq!(second.width(), 480);
        let stored = read_poster_bytes(&dir, url).unwrap();
        assert_eq!(
            image::guess_format(&stored).unwrap(),
            image::ImageFormat::Jpeg
        );
        assert_eq!(image::load_from_memory(&stored).unwrap().width(), 1400);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_native_encoding_preserves_valid_download_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "nova-source-encoding-failure-{}",
            std::process::id()
        ));
        // libwebp cannot represent a longest side above 16383 pixels.
        let original = png(17000, 2);
        let settings = CacheSettings {
            enabled: true,
            downscale: false,
            format: CacheImageFormat::Webp,
            ..CacheSettings::default()
        };
        store_download(&dir, "too-wide", &original, &settings).unwrap();
        assert_eq!(read_poster_bytes(&dir, "too-wide").unwrap(), original);
        assert!(
            !poster_cache_path_in(&dir, "too-wide")
                .with_extension("cfg")
                .exists()
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_source_is_refetched_and_new_compression_does_not_require_lazy_mode() {
        let dir = std::env::temp_dir().join(format!("nova-source-refetch-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let url = "https://nova-test/corrupt-source-refetch";
        fs::write(poster_cache_path_in(&dir, url), b"corrupt").unwrap();
        let settings = CacheSettings {
            enabled: true,
            lazy_reencode: false,
            downscale: false,
            format: CacheImageFormat::Webp,
            ..CacheSettings::default()
        };
        let pixels =
            cached_pixels_with(&dir, url, Some(480), &settings, || Some(png(1400, 700))).unwrap();
        assert_eq!(pixels.width(), 480);
        let bytes = read_poster_bytes(&dir, url).unwrap();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::WebP
        );
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 1400);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bulk_api_serializes_competing_jobs() {
        let root = std::env::temp_dir().join(format!("nova-source-jobs-{}", std::process::id()));
        let active = std::sync::atomic::AtomicUsize::new(0);
        let peak = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for i in 0..2 {
                let dir = root.join(i.to_string());
                write_poster_bytes(&dir, "job", &png(16, 16));
                let active = &active;
                let peak = &peak;
                scope.spawn(move || {
                    let mut started = false;
                    let mut finished = false;
                    rewrite_cache_dir_with_progress(
                        &dir,
                        &CacheSettings::default(),
                        &AtomicBool::new(false),
                        |p| {
                            if !started {
                                started = true;
                                let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                                peak.fetch_max(n, Ordering::SeqCst);
                                std::thread::sleep(std::time::Duration::from_millis(20));
                            }
                            if p.processed == p.total && !finished {
                                finished = true;
                                active.fetch_sub(1, Ordering::SeqCst);
                            }
                        },
                    );
                });
            }
        });
        assert_eq!(peak.load(Ordering::SeqCst), 1);
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn stale_decode_cannot_repopulate_memory_after_source_replacement() {
        let dir = std::env::temp_dir().join(format!("nova-source-stale-{}", std::process::id()));
        let url = "stale-decoder-after-conversion";
        let old = png(80, 80);
        let new = png(40, 40);
        write_poster_bytes(&dir, url, &old);
        let pixels = decode_image_bytes(&old).unwrap();
        write_poster_bytes(&dir, url, &new);
        cache_source_pixels(&dir, url, url, &pixels, &old, true);
        assert!(decoded_cache_get(url).is_none());
        let _ = fs::remove_dir_all(dir);
    }
}
