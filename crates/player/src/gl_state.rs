//! GL state isolation for mpv in Slint's shared context.
// Thin FFI wrapper: every `glow::HasContext` call is unsafe by contract
// (the GL context must be current), so the whole module opts out of the
// per-call `unsafe {}` requirement rather than nesting blocks around each.
#![allow(unsafe_op_in_unsafe_fn)]

use glow::HasContext;
use std::num::NonZeroU32;

// External GLES textures have distinct bind and query enums; glow exports
// neither. Do not query these Android-only enums on desktop OpenGL.
#[cfg(target_os = "android")]
const TEXTURE_EXTERNAL_OES: u32 = 0x8D65;
#[cfg(target_os = "android")]
const TEXTURE_BINDING_EXTERNAL_OES: u32 = 0x8D67;

// Desktop pixel-transfer state is not reset by mpv's frame upload path.
// Nonzero row/skip values or a bound PBO reinterpret CPU frame pointers.
#[cfg(any(target_os = "windows", all(test, target_os = "linux")))]
const TRANSFER_PARAMS: [u32; 14] = [
    glow::UNPACK_ROW_LENGTH,
    glow::UNPACK_SKIP_PIXELS,
    glow::UNPACK_SKIP_ROWS,
    glow::UNPACK_IMAGE_HEIGHT,
    glow::UNPACK_SKIP_IMAGES,
    glow::UNPACK_SWAP_BYTES,
    glow::UNPACK_LSB_FIRST,
    glow::PACK_ROW_LENGTH,
    glow::PACK_SKIP_PIXELS,
    glow::PACK_SKIP_ROWS,
    glow::PACK_IMAGE_HEIGHT,
    glow::PACK_SKIP_IMAGES,
    glow::PACK_SWAP_BYTES,
    glow::PACK_LSB_FIRST,
];

// Covers what Skia/mpv use on GLES3; bounds the per-frame query cost.
const MAX_UNITS_SAVED: i32 = 16;

#[derive(Debug, PartialEq)]
struct Unit {
    tex_2d: i32,
    tex_cube: i32,
    tex_2d_array: i32,
    #[cfg(target_os = "android")]
    tex_external: i32,
    sampler: i32,
}

#[derive(Debug, PartialEq)]
pub struct Saved {
    viewport: [i32; 4],
    scissor_box: [i32; 4],
    scissor_test: bool,
    clear_color: [f32; 4],
    color_mask: [i32; 4],
    program: i32,
    active_texture: i32,
    array_buffer: i32,
    element_buffer: i32,
    vertex_array: i32,
    draw_fbo: i32,
    read_fbo: i32,
    renderbuffer: i32,
    blend: bool,
    blend_src_rgb: i32,
    blend_dst_rgb: i32,
    blend_src_alpha: i32,
    blend_dst_alpha: i32,
    blend_eq_rgb: i32,
    blend_eq_alpha: i32,
    depth_test: bool,
    depth_func: i32,
    depth_mask: i32,
    cull_face: bool,
    cull_mode: i32,
    front_face: i32,
    stencil_test: bool,
    stencil_front: [i32; 3],
    stencil_front_op: [i32; 3],
    stencil_front_mask: i32,
    stencil_back: [i32; 3],
    stencil_back_op: [i32; 3],
    stencil_back_mask: i32,
    dither: bool,
    unpack_alignment: i32,
    pack_alignment: i32,
    #[cfg(any(target_os = "windows", all(test, target_os = "linux")))]
    transfer: [i32; 14],
    #[cfg(any(target_os = "windows", all(test, target_os = "linux")))]
    unpack_buffer: i32,
    #[cfg(any(target_os = "windows", all(test, target_os = "linux")))]
    pack_buffer: i32,
    units: Vec<Unit>,
}

/// Zero is "unbound" for every GL object, so map 0 to `None`.
fn id<T>(value: i32, wrap: impl FnOnce(NonZeroU32) -> T) -> Option<T> {
    NonZeroU32::new(value as u32).map(wrap)
}

unsafe fn set_cap(gl: &glow::Context, cap: u32, on: bool) {
    if on {
        gl.enable(cap);
    } else {
        gl.disable(cap);
    }
}

/// Snapshot the host renderer's state before mpv is allowed to touch it.
pub unsafe fn save(gl: &glow::Context) -> Saved {
    let max_units = gl
        .get_parameter_i32(glow::MAX_COMBINED_TEXTURE_IMAGE_UNITS)
        .clamp(1, MAX_UNITS_SAVED);
    let active_texture = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);

    let mut viewport = [0i32; 4];
    gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
    let mut scissor_box = [0i32; 4];
    gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut scissor_box);
    let mut clear_color = [0.0f32; 4];
    gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut clear_color);
    let mut color_mask = [0i32; 4];
    gl.get_parameter_i32_slice(glow::COLOR_WRITEMASK, &mut color_mask);

    // Texture-unit bindings are per unit, so walk them from a known unit and
    // restore the original active unit afterwards.
    let mut units = Vec::with_capacity(max_units as usize);
    for unit in 0..max_units {
        gl.active_texture(glow::TEXTURE0 + unit as u32);
        units.push(Unit {
            tex_2d: gl.get_parameter_i32(glow::TEXTURE_BINDING_2D),
            tex_cube: gl.get_parameter_i32(glow::TEXTURE_BINDING_CUBE_MAP),
            tex_2d_array: gl.get_parameter_i32(glow::TEXTURE_BINDING_2D_ARRAY),
            #[cfg(target_os = "android")]
            tex_external: gl.get_parameter_i32(TEXTURE_BINDING_EXTERNAL_OES),
            sampler: gl.get_parameter_i32(glow::SAMPLER_BINDING),
        });
    }
    gl.active_texture(active_texture as u32);

    Saved {
        viewport,
        scissor_box,
        scissor_test: gl.is_enabled(glow::SCISSOR_TEST),
        clear_color,
        color_mask,
        program: gl.get_parameter_i32(glow::CURRENT_PROGRAM),
        active_texture,
        array_buffer: gl.get_parameter_i32(glow::ARRAY_BUFFER_BINDING),
        element_buffer: gl.get_parameter_i32(glow::ELEMENT_ARRAY_BUFFER_BINDING),
        vertex_array: gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING),
        draw_fbo: gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING),
        read_fbo: gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING),
        renderbuffer: gl.get_parameter_i32(glow::RENDERBUFFER_BINDING),
        blend: gl.is_enabled(glow::BLEND),
        blend_src_rgb: gl.get_parameter_i32(glow::BLEND_SRC_RGB),
        blend_dst_rgb: gl.get_parameter_i32(glow::BLEND_DST_RGB),
        blend_src_alpha: gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
        blend_dst_alpha: gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
        blend_eq_rgb: gl.get_parameter_i32(glow::BLEND_EQUATION_RGB),
        blend_eq_alpha: gl.get_parameter_i32(glow::BLEND_EQUATION_ALPHA),
        depth_test: gl.is_enabled(glow::DEPTH_TEST),
        depth_func: gl.get_parameter_i32(glow::DEPTH_FUNC),
        depth_mask: gl.get_parameter_i32(glow::DEPTH_WRITEMASK),
        cull_face: gl.is_enabled(glow::CULL_FACE),
        cull_mode: gl.get_parameter_i32(glow::CULL_FACE_MODE),
        front_face: gl.get_parameter_i32(glow::FRONT_FACE),
        stencil_test: gl.is_enabled(glow::STENCIL_TEST),
        stencil_front: [
            gl.get_parameter_i32(glow::STENCIL_FUNC),
            gl.get_parameter_i32(glow::STENCIL_REF),
            gl.get_parameter_i32(glow::STENCIL_VALUE_MASK),
        ],
        stencil_front_op: [
            gl.get_parameter_i32(glow::STENCIL_FAIL),
            gl.get_parameter_i32(glow::STENCIL_PASS_DEPTH_FAIL),
            gl.get_parameter_i32(glow::STENCIL_PASS_DEPTH_PASS),
        ],
        stencil_front_mask: gl.get_parameter_i32(glow::STENCIL_WRITEMASK),
        stencil_back: [
            gl.get_parameter_i32(glow::STENCIL_BACK_FUNC),
            gl.get_parameter_i32(glow::STENCIL_BACK_REF),
            gl.get_parameter_i32(glow::STENCIL_BACK_VALUE_MASK),
        ],
        stencil_back_op: [
            gl.get_parameter_i32(glow::STENCIL_BACK_FAIL),
            gl.get_parameter_i32(glow::STENCIL_BACK_PASS_DEPTH_FAIL),
            gl.get_parameter_i32(glow::STENCIL_BACK_PASS_DEPTH_PASS),
        ],
        stencil_back_mask: gl.get_parameter_i32(glow::STENCIL_BACK_WRITEMASK),
        dither: gl.is_enabled(glow::DITHER),
        unpack_alignment: gl.get_parameter_i32(glow::UNPACK_ALIGNMENT),
        pack_alignment: gl.get_parameter_i32(glow::PACK_ALIGNMENT),
        #[cfg(any(target_os = "windows", all(test, target_os = "linux")))]
        transfer: TRANSFER_PARAMS.map(|param| gl.get_parameter_i32(param)),
        #[cfg(any(target_os = "windows", all(test, target_os = "linux")))]
        unpack_buffer: gl.get_parameter_i32(glow::PIXEL_UNPACK_BUFFER_BINDING),
        #[cfg(any(target_os = "windows", all(test, target_os = "linux")))]
        pack_buffer: gl.get_parameter_i32(glow::PIXEL_PACK_BUFFER_BINDING),
        units,
    }
}

// mpv's OpenGL API expects standard defaults on entry; restoring state after
// rendering alone does not protect mpv from blend, stencil or upload state
// left behind by the host. Only Windows uses this extra entry preparation.
#[cfg(any(target_os = "windows", all(test, target_os = "linux")))]
pub unsafe fn prepare(gl: &glow::Context) {
    for cap in [
        glow::BLEND,
        glow::SCISSOR_TEST,
        glow::STENCIL_TEST,
        glow::DEPTH_TEST,
        glow::CULL_FACE,
    ] {
        gl.disable(cap);
    }
    gl.color_mask(true, true, true, true);
    gl.depth_mask(true);
    gl.use_program(None);
    gl.bind_vertex_array(None);
    gl.bind_buffer(glow::ARRAY_BUFFER, None);
    gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
    gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
    for param in TRANSFER_PARAMS {
        gl.pixel_store_i32(param, 0);
    }
    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
    gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
    gl.active_texture(glow::TEXTURE0);
}

/// Put the context back as the host renderer left it.
pub unsafe fn restore(gl: &glow::Context, s: &Saved) {
    gl.use_program(id(s.program, glow::NativeProgram));

    gl.bind_vertex_array(id(s.vertex_array, glow::NativeVertexArray));
    gl.bind_buffer(glow::ARRAY_BUFFER, id(s.array_buffer, glow::NativeBuffer));
    // A core desktop context does not permit changing the element buffer
    // while VAO 0 is bound. There is no binding to restore in that case.
    if s.vertex_array != 0 {
        gl.bind_buffer(
            glow::ELEMENT_ARRAY_BUFFER,
            id(s.element_buffer, glow::NativeBuffer),
        );
    }

    for (unit, state) in s.units.iter().enumerate() {
        let slot = glow::TEXTURE0 + unit as u32;
        gl.active_texture(slot);
        gl.bind_texture(glow::TEXTURE_2D, id(state.tex_2d, glow::NativeTexture));
        gl.bind_texture(
            glow::TEXTURE_CUBE_MAP,
            id(state.tex_cube, glow::NativeTexture),
        );
        gl.bind_texture(
            glow::TEXTURE_2D_ARRAY,
            id(state.tex_2d_array, glow::NativeTexture),
        );
        #[cfg(target_os = "android")]
        gl.bind_texture(
            TEXTURE_EXTERNAL_OES,
            id(state.tex_external, glow::NativeTexture),
        );
        // glBindSampler takes a unit index, not the GL_TEXTURE0 enum.
        gl.bind_sampler(unit as u32, id(state.sampler, glow::NativeSampler));
    }
    gl.active_texture(s.active_texture as u32);

    gl.bind_framebuffer(
        glow::DRAW_FRAMEBUFFER,
        id(s.draw_fbo, glow::NativeFramebuffer),
    );
    gl.bind_framebuffer(
        glow::READ_FRAMEBUFFER,
        id(s.read_fbo, glow::NativeFramebuffer),
    );
    gl.bind_renderbuffer(
        glow::RENDERBUFFER,
        id(s.renderbuffer, glow::NativeRenderbuffer),
    );

    gl.viewport(s.viewport[0], s.viewport[1], s.viewport[2], s.viewport[3]);
    gl.scissor(
        s.scissor_box[0],
        s.scissor_box[1],
        s.scissor_box[2],
        s.scissor_box[3],
    );
    set_cap(gl, glow::SCISSOR_TEST, s.scissor_test);

    gl.clear_color(
        s.clear_color[0],
        s.clear_color[1],
        s.clear_color[2],
        s.clear_color[3],
    );
    gl.color_mask(
        s.color_mask[0] != 0,
        s.color_mask[1] != 0,
        s.color_mask[2] != 0,
        s.color_mask[3] != 0,
    );

    set_cap(gl, glow::BLEND, s.blend);
    gl.blend_func_separate(
        s.blend_src_rgb as u32,
        s.blend_dst_rgb as u32,
        s.blend_src_alpha as u32,
        s.blend_dst_alpha as u32,
    );
    gl.blend_equation_separate(s.blend_eq_rgb as u32, s.blend_eq_alpha as u32);

    set_cap(gl, glow::DEPTH_TEST, s.depth_test);
    gl.depth_func(s.depth_func as u32);
    gl.depth_mask(s.depth_mask != 0);

    set_cap(gl, glow::CULL_FACE, s.cull_face);
    gl.cull_face(s.cull_mode as u32);
    gl.front_face(s.front_face as u32);

    set_cap(gl, glow::STENCIL_TEST, s.stencil_test);
    gl.stencil_func_separate(
        glow::FRONT,
        s.stencil_front[0] as u32,
        s.stencil_front[1],
        s.stencil_front[2] as u32,
    );
    gl.stencil_op_separate(
        glow::FRONT,
        s.stencil_front_op[0] as u32,
        s.stencil_front_op[1] as u32,
        s.stencil_front_op[2] as u32,
    );
    gl.stencil_mask_separate(glow::FRONT, s.stencil_front_mask as u32);
    gl.stencil_func_separate(
        glow::BACK,
        s.stencil_back[0] as u32,
        s.stencil_back[1],
        s.stencil_back[2] as u32,
    );
    gl.stencil_op_separate(
        glow::BACK,
        s.stencil_back_op[0] as u32,
        s.stencil_back_op[1] as u32,
        s.stencil_back_op[2] as u32,
    );
    gl.stencil_mask_separate(glow::BACK, s.stencil_back_mask as u32);

    set_cap(gl, glow::DITHER, s.dither);
    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, s.unpack_alignment);
    gl.pixel_store_i32(glow::PACK_ALIGNMENT, s.pack_alignment);
    #[cfg(any(target_os = "windows", all(test, target_os = "linux")))]
    {
        for (param, value) in TRANSFER_PARAMS.into_iter().zip(s.transfer) {
            gl.pixel_store_i32(param, value);
        }
        gl.bind_buffer(
            glow::PIXEL_UNPACK_BUFFER,
            id(s.unpack_buffer, glow::NativeBuffer),
        );
        gl.bind_buffer(
            glow::PIXEL_PACK_BUFFER,
            id(s.pack_buffer, glow::NativeBuffer),
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::ffi::{CStr, c_void};

    type Handle = *mut c_void;
    type GetDisplay = unsafe extern "C" fn(u32, Handle, *const isize) -> Handle;
    type Initialize = unsafe extern "C" fn(Handle, *mut i32, *mut i32) -> u32;
    type BindApi = unsafe extern "C" fn(u32) -> u32;
    type ChooseConfig = unsafe extern "C" fn(Handle, *const i32, *mut Handle, i32, *mut i32) -> u32;
    type CreateSurface = unsafe extern "C" fn(Handle, Handle, *const i32) -> Handle;
    type CreateContext = unsafe extern "C" fn(Handle, Handle, Handle, *const i32) -> Handle;
    type MakeCurrent = unsafe extern "C" fn(Handle, Handle, Handle, Handle) -> u32;
    type Destroy = unsafe extern "C" fn(Handle, Handle) -> u32;
    type Terminate = unsafe extern "C" fn(Handle) -> u32;
    type GetProc = unsafe extern "C" fn(*const std::ffi::c_char) -> *const c_void;

    struct HeadlessGl {
        egl: libloading::Library,
        display: Handle,
        surface: Handle,
        context: Handle,
    }

    impl HeadlessGl {
        unsafe fn function<T: Copy>(&self, name: &CStr) -> T {
            *self.egl.get::<T>(name.to_bytes_with_nul()).unwrap()
        }

        unsafe fn new() -> Self {
            let mut result = Self {
                egl: libloading::Library::new("libEGL.so.1").unwrap(),
                display: std::ptr::null_mut(),
                surface: std::ptr::null_mut(),
                context: std::ptr::null_mut(),
            };
            // Mesa's surfaceless EGL platform needs no X server or GPU.
            result.display = result.function::<GetDisplay>(c"eglGetPlatformDisplay")(
                0x31DD,
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            assert!(!result.display.is_null());
            assert_ne!(
                result.function::<Initialize>(c"eglInitialize")(
                    result.display,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ),
                0
            );
            assert_ne!(result.function::<BindApi>(c"eglBindAPI")(0x30A2), 0);
            // RGBA8, pbuffer, OpenGL; then a 4x4 surface and GL 3.3 core.
            let attrs = [
                0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 8, 0x3033, 1, 0x3040, 8, 0x3038,
            ];
            let mut config = std::ptr::null_mut();
            let mut count = 0;
            assert_ne!(
                result.function::<ChooseConfig>(c"eglChooseConfig")(
                    result.display,
                    attrs.as_ptr(),
                    &mut config,
                    1,
                    &mut count,
                ),
                0
            );
            assert_eq!(count, 1);
            result.surface = result.function::<CreateSurface>(c"eglCreatePbufferSurface")(
                result.display,
                config,
                [0x3057, 4, 0x3056, 4, 0x3038].as_ptr(),
            );
            result.context = result.function::<CreateContext>(c"eglCreateContext")(
                result.display,
                config,
                std::ptr::null_mut(),
                [0x3098, 3, 0x30FB, 3, 0x30FD, 1, 0x3038].as_ptr(),
            );
            assert!(!result.surface.is_null() && !result.context.is_null());
            assert_ne!(
                result.function::<MakeCurrent>(c"eglMakeCurrent")(
                    result.display,
                    result.surface,
                    result.surface,
                    result.context,
                ),
                0
            );
            result
        }

        unsafe fn gl(&self) -> glow::Context {
            let get_proc = self.function::<GetProc>(c"eglGetProcAddress");
            glow::Context::from_loader_function_cstr(|name| get_proc(name.as_ptr()))
        }
    }

    impl Drop for HeadlessGl {
        fn drop(&mut self) {
            unsafe {
                let none = std::ptr::null_mut();
                self.function::<MakeCurrent>(c"eglMakeCurrent")(self.display, none, none, none);
                self.function::<Destroy>(c"eglDestroyContext")(self.display, self.context);
                self.function::<Destroy>(c"eglDestroySurface")(self.display, self.surface);
                self.function::<Terminate>(c"eglTerminate")(self.display);
            }
        }
    }

    #[test]
    fn clean_draw_and_host_state_round_trip() {
        unsafe {
            let context = HeadlessGl::new();
            let gl = context.gl();
            let vao = gl.create_vertex_array().unwrap();
            gl.bind_vertex_array(Some(vao));
            let buffer = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
            gl.buffer_data_size(glow::ARRAY_BUFFER, 4096, glow::STREAM_DRAW);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(buffer));
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(buffer));
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(buffer));
            let sampler = gl.create_sampler().unwrap();
            gl.bind_sampler(3, Some(sampler));
            gl.active_texture(glow::TEXTURE0 + 3);
            for cap in [
                glow::BLEND,
                glow::SCISSOR_TEST,
                glow::STENCIL_TEST,
                glow::DEPTH_TEST,
                glow::CULL_FACE,
            ] {
                gl.enable(cap);
            }
            gl.scissor(0, 0, 0, 0);
            gl.color_mask(false, false, false, false);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            for (param, value) in TRANSFER_PARAMS
                .into_iter()
                .zip([7, 2, 1, 5, 1, 1, 1, 9, 3, 2, 6, 1, 1, 1])
            {
                gl.pixel_store_i32(param, value);
            }
            let before = save(&gl);
            assert_eq!(gl.get_error(), glow::NO_ERROR);
            prepare(&gl);
            assert!(!gl.is_enabled(glow::BLEND));
            assert!(!gl.is_enabled(glow::STENCIL_TEST));
            assert_eq!(gl.get_parameter_i32(glow::PIXEL_UNPACK_BUFFER_BINDING), 0);
            for param in TRANSFER_PARAMS {
                assert_eq!(gl.get_parameter_i32(param), 0);
            }
            // Incoming clip/masks/PBO state would prevent or redirect this
            // draw/readback. Exercise a real GL framebuffer, not a mock.
            gl.clear_color(1.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            let mut pixel = [0; 4];
            gl.read_pixels(
                0,
                0,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixel)),
            );
            assert_eq!(pixel, [255, 0, 0, 255]);
            restore(&gl, &before);
            assert_eq!(save(&gl), before);
            assert_eq!(gl.get_error(), glow::NO_ERROR);
            gl.delete_sampler(sampler);
            gl.delete_buffer(buffer);
            gl.delete_vertex_array(vao);
        }
    }
}
