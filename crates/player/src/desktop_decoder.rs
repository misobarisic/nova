//! Desktop decoder selection, independent of the window/context implementation.
use libmpv2::Mpv;
use nova_config::DesktopHwdec;

pub(super) fn requested_mode(preference: DesktopHwdec) -> &'static str {
    // Let mpv probe compatible direct interop first. Explicitly retain copy-back
    // fallback: a host OpenGL context need not support Windows decoder surfaces.
    match preference {
        DesktopHwdec::HwPlus => "auto,auto-copy",
        DesktopHwdec::Hw => "auto-copy",
        DesktopHwdec::Sw => "no",
    }
}

pub(super) fn configure(mpv: &Mpv, preference: DesktopHwdec) -> libmpv2::Result<()> {
    mpv.set_property("hwdec", requested_mode(preference))
}

pub(super) fn active(current: &str) -> Option<DesktopHwdec> {
    match current {
        "" => None, // No decoder yet; do not claim a successful direct path.
        "no" => Some(DesktopHwdec::Sw),
        value if value.ends_with("-copy") => Some(DesktopHwdec::Hw),
        _ => Some(DesktopHwdec::HwPlus),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_are_accepted_and_new_stream_restores_saved_preference() {
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        let saved = nova_config::CacheSettings::default();
        assert_eq!(saved.desktop_hwdec, DesktopHwdec::default());
        for (index, expected) in [(0, "auto,auto-copy"), (2, "no"), (1, "auto-copy")] {
            configure(&mpv, DesktopHwdec::from_index(index)).unwrap();
            assert_eq!(mpv.get_property::<String>("hwdec").unwrap(), expected);
            // The same configuration runs at every new play, dropping the
            // session pick without touching the persisted preference.
            configure(&mpv, saved.desktop_hwdec).unwrap();
            assert_eq!(
                mpv.get_property::<String>("hwdec").unwrap(),
                requested_mode(saved.desktop_hwdec)
            );
        }
    }

    #[test]
    fn badge_reports_fallback_instead_of_requested_direct_mode() {
        for current in [
            "d3d11va", "dxva2", "nvdec", "cuda", "vaapi", "vdpau", "vulkan",
        ] {
            assert_eq!(active(current), Some(DesktopHwdec::HwPlus));
        }
        for current in [
            "d3d11va-copy",
            "dxva2-copy",
            "nvdec-copy",
            "cuda-copy",
            "vaapi-copy",
            "vulkan-copy",
        ] {
            assert_eq!(active(current), Some(DesktopHwdec::Hw));
        }
        assert_eq!(active("no"), Some(DesktopHwdec::Sw));
        assert_eq!(active(""), None);
    }
}
