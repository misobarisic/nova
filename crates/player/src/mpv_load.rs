//! Version-independent per-file start options. mpv 0.38 inserted an index
//! before positional loadfile options; named arguments work on both sides.

use libmpv2::Mpv;
use libmpv2_sys::{mpv_node, mpv_node__bindgen_ty_1, mpv_node_list};
use std::ffi::CString;

pub(super) fn load_file_at(mpv: &Mpv, url: &str, position: f64) -> libmpv2::Result<()> {
    let strings = [
        "loadfile".to_owned(),
        url.to_owned(),
        "replace".to_owned(),
        format!("start={position}"),
    ]
    .into_iter()
    .map(CString::new)
    .collect::<Result<Vec<_>, _>>()?;
    let mut values = strings
        .iter()
        .map(|value| mpv_node {
            format: libmpv2::mpv_format::String,
            u: mpv_node__bindgen_ty_1 {
                string: value.as_ptr().cast_mut(),
            },
        })
        .collect::<Vec<_>>();
    let mut keys = [c"name", c"url", c"flags", c"options"].map(|key| key.as_ptr().cast_mut());
    let mut list = mpv_node_list {
        num: values.len() as _,
        values: values.as_mut_ptr(),
        keys: keys.as_mut_ptr(),
    };
    let mut command = mpv_node {
        format: libmpv2::mpv_format::Map,
        u: mpv_node__bindgen_ty_1 { list: &mut list },
    };
    // mpv copies the arguments synchronously. All node/string storage stays
    // alive for this call; no result is requested, so none needs freeing.
    let status = unsafe {
        libmpv2_sys::mpv_command_node(mpv.ctx.as_ptr().cast(), &mut command, std::ptr::null_mut())
    };
    if status < 0 {
        Err(libmpv2::Error::Raw(status))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn named_loadfile_starts_and_reloads_at_position_while_paused() {
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        // Three seconds of silence, avoiding network and device dependencies.
        let samples = vec![0u8; 3 * 8000 * 2];
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend((36 + samples.len() as u32).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(16000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend((samples.len() as u32).to_le_bytes());
        wav.extend(samples);
        let path =
            std::env::temp_dir().join(format!("nova-loadfile-test-{}.wav", std::process::id()));
        std::fs::write(&path, wav).unwrap();

        // A rejected command must not make subsequent loads unusable.
        assert!(load_file_at(&mpv, "invalid\0source", 0.0).is_err());
        for at in [0.75, 1.25] {
            mpv.set_property("pause", true).unwrap();
            load_file_at(&mpv, path.to_str().unwrap(), at).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if mpv
                    .get_property::<f64>("time-pos")
                    .is_ok_and(|pos| (pos - at).abs() < 0.1)
                {
                    break;
                }
                assert!(Instant::now() < deadline, "file did not open at {at}s");
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(mpv.get_property::<bool>("pause").unwrap());
            mpv.command("stop", &[]).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !mpv.get_property::<bool>("idle-active").unwrap_or(false) {
                assert!(Instant::now() < deadline, "stop did not finish");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        std::fs::remove_file(path).unwrap();
    }
}
