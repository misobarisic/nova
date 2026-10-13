//! HW+ captures stay on the render thread until their RGBA pixels are readable.
//! Encoding and publication stay on the screenshot worker, outside the GL context.
use crate::{ScreenshotError, gl_state, mpv_opengl_fbo, mpv_render_param};
use glow::HasContext;
use std::path::Path;

pub(super) struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Frame {
    pub(super) fn write_image(
        &self,
        path: &Path,
        format: nova_config::ScreenshotFormat,
        quality: u8,
    ) -> Result<(), ScreenshotError> {
        crate::screenshot::write_image(path, self.width, self.height, &self.pixels, format, quality)
    }
}

pub(super) fn dimensions(
    width: i64,
    height: i64,
    rotation: i64,
) -> Result<(u32, u32), ScreenshotError> {
    // Bound CPU/GPU allocation to an 8K frame, including malformed metadata.
    if width <= 0 || height <= 0 || width > 8192 || height > 8192 || width * height > 33_554_432 {
        return Err(ScreenshotError::Unavailable);
    }
    let (width, height) = if rotation.rem_euclid(180) == 90 {
        (height, width)
    } else {
        (width, height)
    };
    Ok((width as u32, height as u32))
}

#[cfg(any(target_os = "android", test))]
mod queue {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::{Duration, Instant};

    pub(crate) struct Request {
        session: u64,
        width: u32,
        height: u32,
        cancelled: Arc<AtomicBool>,
        reply: mpsc::SyncSender<Result<Frame, ScreenshotError>>,
    }
    pub(crate) struct Pending {
        cancelled: Arc<AtomicBool>,
        reply: mpsc::Receiver<Result<Frame, ScreenshotError>>,
    }
    impl Request {
        pub(crate) fn new(session: u64, width: u32, height: u32) -> (Self, Pending) {
            let (tx, rx) = mpsc::sync_channel(1);
            let cancelled = Arc::new(AtomicBool::new(false));
            (
                Self {
                    session,
                    width,
                    height,
                    cancelled: cancelled.clone(),
                    reply: tx,
                },
                Pending {
                    cancelled,
                    reply: rx,
                },
            )
        }
        pub(crate) fn cancelled(&self) -> bool {
            self.cancelled.load(Ordering::SeqCst)
        }
        /// Only called with the player's original GL context current.
        pub(crate) unsafe fn complete(
            self,
            session: u64,
            started: bool,
            gl: &glow::Context,
            ctx: *mut crate::mpv_render_context,
        ) {
            if self.cancelled() {
                return;
            }
            let result = if self.session != session {
                Err(ScreenshotError::StaleSession)
            } else if !started {
                Err(ScreenshotError::Unavailable)
            } else {
                unsafe { capture(gl, ctx, self.width, self.height) }
            };
            let _ = self.reply.send(result);
        }
    }
    impl Pending {
        pub(crate) fn wait(&self, stale: impl Fn() -> bool) -> Result<Frame, ScreenshotError> {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                if stale() {
                    return Err(ScreenshotError::StaleSession);
                }
                if Instant::now() >= deadline {
                    return Err(ScreenshotError::TimedOut);
                }
                match self.reply.recv_timeout(Duration::from_millis(100)) {
                    Ok(result) => return result,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(ScreenshotError::Unavailable);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        }
    }
    impl Drop for Pending {
        fn drop(&mut self) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }
}
#[cfg(any(target_os = "android", test))]
pub(super) use queue::{Pending, Request};

struct Target<'a> {
    gl: &'a glow::Context,
    fbo: glow::Framebuffer,
    color: glow::Renderbuffer,
}
impl Drop for Target<'_> {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_framebuffer(self.fbo);
            self.gl.delete_renderbuffer(self.color);
        }
    }
}

// GLES3 pack row/skip/PBO state can otherwise reinterpret the slice passed to
// glReadPixels. The shared Android guard currently only owns pack alignment.
struct PackState<'a> {
    gl: &'a glow::Context,
    buffer: Option<glow::Buffer>,
    values: [i32; 3],
}
const PACK_PARAMS: [u32; 3] = [
    glow::PACK_ROW_LENGTH,
    glow::PACK_SKIP_ROWS,
    glow::PACK_SKIP_PIXELS,
];
impl Drop for PackState<'_> {
    fn drop(&mut self) {
        unsafe {
            self.gl.bind_buffer(glow::PIXEL_PACK_BUFFER, self.buffer);
            for (param, value) in PACK_PARAMS.into_iter().zip(self.values) {
                self.gl.pixel_store_i32(param, value);
            }
        }
    }
}

/// Render the video/subtitles to a private native-size target. No Slint pixels
/// enter this target. The caller restores mpv's window size by rendering again.
pub(super) unsafe fn capture(
    gl: &glow::Context,
    ctx: *mut crate::mpv_render_context,
    width: u32,
    height: u32,
) -> Result<Frame, ScreenshotError> {
    use crate::{MPV_RENDER_PARAM_FLIP_Y, MPV_RENDER_PARAM_INVALID, MPV_RENDER_PARAM_OPENGL_FBO};
    dimensions(width as i64, height as i64, 0)?;
    unsafe {
        let _guard = gl_state::Guard::new(gl);
        let buffer =
            std::num::NonZeroU32::new(gl.get_parameter_i32(glow::PIXEL_PACK_BUFFER_BINDING) as u32)
                .map(glow::NativeBuffer);
        let _pack = PackState {
            gl,
            buffer,
            values: PACK_PARAMS.map(|param| gl.get_parameter_i32(param)),
        };
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
        for param in PACK_PARAMS {
            gl.pixel_store_i32(param, 0);
        }
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
        let max_size = gl.get_parameter_i32(glow::MAX_RENDERBUFFER_SIZE);
        if width as i32 > max_size || height as i32 > max_size {
            return Err(ScreenshotError::CaptureFailed);
        }
        let fbo = gl
            .create_framebuffer()
            .map_err(|_| ScreenshotError::CaptureFailed)?;
        let color = match gl.create_renderbuffer() {
            Ok(color) => color,
            Err(_) => {
                gl.delete_framebuffer(fbo);
                return Err(ScreenshotError::CaptureFailed);
            }
        };
        let target = Target { gl, fbo, color };
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target.fbo));
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(target.color));
        gl.renderbuffer_storage(glow::RENDERBUFFER, glow::RGBA8, width as i32, height as i32);
        gl.framebuffer_renderbuffer(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::RENDERBUFFER,
            Some(target.color),
        );
        if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
            return Err(ScreenshotError::CaptureFailed);
        }
        gl.disable(glow::SCISSOR_TEST);
        gl.color_mask(true, true, true, true);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        let mut fbo = mpv_opengl_fbo {
            fbo: target.fbo.0.get() as i32,
            w: width as i32,
            h: height as i32,
            internal_format: glow::RGBA8 as i32,
        };
        // mpv's normal offscreen output reads back top-first. The window's
        // flip_y=1 is for presentation and would invert the saved image.
        let mut flip = 0_i32;
        let mut block = 0_i32;
        let mut params = [
            mpv_render_param {
                type_: MPV_RENDER_PARAM_OPENGL_FBO,
                data: (&mut fbo as *mut mpv_opengl_fbo).cast(),
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_FLIP_Y,
                data: (&mut flip as *mut i32).cast(),
            },
            // A capture must not add another audio-sync wait to this frame.
            mpv_render_param {
                type_: crate::MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                data: (&mut block as *mut i32).cast(),
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        if crate::mpv_render_context_render(ctx, params.as_mut_ptr()) < 0 {
            return Err(ScreenshotError::CaptureFailed);
        }
        let mut pixels = Vec::new();
        let bytes = width as usize * height as usize * 4;
        pixels
            .try_reserve_exact(bytes)
            .map_err(|_| ScreenshotError::CaptureFailed)?;
        pixels.resize(bytes, 0);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(target.fbo));
        gl.read_buffer(glow::COLOR_ATTACHMENT0);
        gl.read_pixels(
            0,
            0,
            width as i32,
            height as i32,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixels)),
        );
        if gl.get_error() != glow::NO_ERROR {
            return Err(ScreenshotError::CaptureFailed);
        }
        Ok(Frame {
            width,
            height,
            pixels,
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::gl_state::tests::HeadlessGl;
    use std::fs::File;
    use std::time::{Duration, Instant};

    struct RenderContext(*mut crate::mpv_render_context);
    impl Drop for RenderContext {
        fn drop(&mut self) {
            unsafe {
                crate::mpv_render_context_free(self.0);
            }
        }
    }

    #[test]
    fn dimensions_preserve_rotation_and_reject_unbounded_allocations() {
        assert_eq!(dimensions(1920, 1080, 0), Ok((1920, 1080)));
        assert_eq!(dimensions(1920, 1080, 90), Ok((1080, 1920)));
        assert_eq!(dimensions(1920, 1080, 270), Ok((1080, 1920)));
        for (w, h) in [(0, 1080), (-1, 1080), (i64::MAX, 1080), (8192, 8192)] {
            assert_eq!(dimensions(w, h, 0), Err(ScreenshotError::Unavailable));
        }
    }

    #[test]
    fn gpu_capture_preserves_video_orientation_subtitles_and_host_state() {
        unsafe {
            let context = HeadlessGl::new();
            let gl = context.gl();
            let mpv = libmpv2::Mpv::with_initializer(|init| {
                init.set_property("config", false)?;
                init.set_property("vo", "libmpv")?;
                init.set_property("ao", "null")?;
                init.set_property("hwdec", "no")?;
                init.set_property("gpu-sw", true)?;
                init.set_property("pause", true)
            })
            .unwrap();
            let loader = |name: &std::ffi::CStr| context.get_proc(name);
            let loader_ref: &dyn Fn(&std::ffi::CStr) -> *const std::ffi::c_void = &loader;
            let mut init = crate::mpv_opengl_init_params {
                get_proc_address: Some(crate::get_proc_address_trampoline),
                get_proc_address_ctx: (&loader_ref as *const _ as *mut std::ffi::c_void),
                extra_exts: std::ptr::null(),
            };
            let mut params = [
                mpv_render_param {
                    type_: crate::MPV_RENDER_PARAM_API_TYPE,
                    data: c"opengl".as_ptr().cast_mut().cast(),
                },
                mpv_render_param {
                    type_: crate::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                    data: (&mut init as *mut crate::mpv_opengl_init_params).cast(),
                },
                mpv_render_param {
                    type_: 0,
                    data: std::ptr::null_mut(),
                },
            ];
            let mut ctx = std::ptr::null_mut();
            assert_eq!(
                crate::mpv_render_context_create(
                    &mut ctx,
                    mpv.ctx.as_ptr().cast(),
                    params.as_mut_ptr()
                ),
                0
            );
            let _render_context = RenderContext(ctx);
            let directory =
                std::env::temp_dir().join(format!("nova-gpu-capture-{}", std::process::id()));
            std::fs::create_dir_all(&directory).unwrap();
            let source = directory.join("video.y4m");
            let mut video = b"YUV4MPEG2 W192 H128 F30:1 Ip A1:1 C420jpeg\n".to_vec();
            for _ in 0..60 {
                video.extend(b"FRAME\n");
                video.extend(vec![235; 192 * 64]);
                video.extend(vec![16; 192 * 64]);
                video.extend(vec![128; 192 * 128 / 2]);
            }
            std::fs::write(&source, video).unwrap();
            let subs = directory.join("subtitles.srt");
            std::fs::write(&subs, "1\n00:00:00,000 --> 00:00:02,000\nGPU subtitle\n").unwrap();
            crate::mpv_load::load_file_at(&mpv, source.to_str().unwrap(), 0.5).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let plain = loop {
                let frame = capture(&gl, ctx, 192, 128).unwrap();
                if frame.pixels[(192 * 32 + 96) * 4] > 200
                    && mpv.get_property::<f64>("time-pos").is_ok()
                {
                    break frame;
                }
                assert!(
                    Instant::now() < deadline,
                    "fixture never rendered: top={:?}, bottom={:?}, position={:?}",
                    &frame.pixels[(192 * 32 + 96) * 4..][..4],
                    &frame.pixels[(192 * 96 + 96) * 4..][..4],
                    mpv.get_property::<f64>("time-pos")
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            assert!(
                plain.pixels[(192 * 96 + 96) * 4] < 30,
                "readback must retain top/bottom orientation"
            );
            let position = mpv.get_property::<f64>("time-pos").unwrap();
            mpv.command("sub-add", &[subs.to_str().unwrap(), "select"])
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let subtitled = loop {
                let frame = capture(&gl, ctx, 192, 128).unwrap();
                if frame.pixels[192 * 64 * 4..]
                    .iter()
                    .zip(&plain.pixels[192 * 64 * 4..])
                    .any(|(a, b)| a.abs_diff(*b) > 50)
                {
                    break frame;
                }
                assert!(Instant::now() < deadline, "subtitle never rendered");
                std::thread::sleep(Duration::from_millis(20));
            };
            let image = directory.join("frame.png");
            subtitled
                .write_image(&image, nova_config::ScreenshotFormat::Png, 85)
                .unwrap();
            let mut reader =
                png::Decoder::new(std::io::BufReader::new(File::open(&image).unwrap()))
                    .read_info()
                    .unwrap();
            let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
            let info = reader.next_frame(&mut decoded).unwrap();
            assert_eq!((info.width, info.height), (192, 128));
            assert_eq!(decoded, subtitled.pixels);

            // Poison host transfer/clip state. Capturing must still work and
            // restore the exact state Slint will assume for its next draw.
            let buffer = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(buffer));
            gl.buffer_data_size(glow::PIXEL_PACK_BUFFER, 1024, glow::STREAM_READ);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 7);
            gl.pixel_store_i32(glow::PACK_SKIP_ROWS, 2);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(0, 0, 0, 0);
            gl.color_mask(false, false, false, false);
            let before = gl_state::save(&gl);
            let captured = capture(&gl, ctx, 192, 128).unwrap();
            assert_eq!(captured.pixels, subtitled.pixels);
            assert_eq!(gl_state::save(&gl), before);
            assert!(mpv.get_property::<bool>("pause").unwrap());
            assert!((mpv.get_property::<f64>("time-pos").unwrap() - position).abs() < 0.05);
            assert_eq!(mpv.get_property::<String>("hwdec").unwrap(), "no");

            // Cancelled/replaced captures must never dereference a stale render
            // context. A null context makes accidental rendering fail loudly.
            let (request, pending): (Request, Pending) = Request::new(1, 192, 128);
            request.complete(2, true, &gl, std::ptr::null_mut());
            assert!(matches!(
                pending.wait(|| false),
                Err(ScreenshotError::StaleSession)
            ));
            let (request, pending) = Request::new(1, 192, 128);
            drop(pending);
            assert!(request.cancelled());
            request.complete(1, true, &gl, std::ptr::null_mut());
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            gl.delete_buffer(buffer);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
}
