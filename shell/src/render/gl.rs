//! Raw GLES2 helpers for the render kit: programs, textures, offscreen targets. Everything here runs with
//! the renderer's EGL context current (inside `GlesRenderer::with_context` or a frame's `with_context`).
//!
//! State contract with Smithay's GlesRenderer (checked against sources/smithay, gles/mod.rs): Smithay sets
//! program, texture binding + filters, array buffer and vertex attributes on every draw, keeps BLEND on with
//! (ONE, ONE_MINUS_SRC_ALPHA) and SCISSOR_TEST on with the full output as the box. So we may change program,
//! texture and buffer bindings freely, but must disable our vertex attributes, unbind the element array
//! buffer, and restore the scissor box, blend state, viewport and framebuffer binding when we change them.
//! Where instancing exists (desktop GPUs, not grate) Smithay also leaves attribute divisors at 1.

use smithay::backend::renderer::gles::ffi::{self, Gles2};

/// Attribute locations shared by every UI program (ui.vert; pass.vert uses a_pos only).
pub const A_POS: u32 = 0;
pub const A_A: u32 = 1;
pub const A_B: u32 = 2;

pub struct Program {
    pub id: u32,
    pub u_proj: i32,
    pub u_tex: i32,
    pub u_step: i32,
    pub u_mul: i32,
    pub u_add: i32,
}

unsafe fn compile_shader(gl: &Gles2, kind: u32, src: &str) -> Result<u32, String> {
    unsafe {
        let s = gl.CreateShader(kind);
        let ptr = src.as_ptr() as *const ffi::types::GLchar;
        let len = src.len() as i32;
        gl.ShaderSource(s, 1, &ptr, &len);
        gl.CompileShader(s);
        let mut ok = 0;
        gl.GetShaderiv(s, ffi::COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let log = shader_log(gl, s, false);
            gl.DeleteShader(s);
            return Err(log);
        }
        Ok(s)
    }
}

unsafe fn shader_log(gl: &Gles2, obj: u32, program: bool) -> String {
    unsafe {
        let mut buf = vec![0u8; 2048];
        let mut len = 0;
        if program {
            gl.GetProgramInfoLog(obj, buf.len() as i32, &mut len, buf.as_mut_ptr() as *mut _);
        } else {
            gl.GetShaderInfoLog(obj, buf.len() as i32, &mut len, buf.as_mut_ptr() as *mut _);
        }
        buf.truncate(len.max(0) as usize);
        String::from_utf8_lossy(&buf).trim().to_string()
    }
}

/// Compile and link; attributes are bound to the fixed locations above. grate rejects shaders that do not
/// fit (too many EXECs) at link or first draw; link errors come back here with the driver's message.
pub unsafe fn program(gl: &Gles2, name: &str, vs: &str, fs: &str) -> Result<Program, String> {
    unsafe {
        let v = compile_shader(gl, ffi::VERTEX_SHADER, vs).map_err(|e| format!("{name}: vertex shader: {e}"))?;
        let f = compile_shader(gl, ffi::FRAGMENT_SHADER, fs).map_err(|e| format!("{name}: fragment shader: {e}"))?;
        let p = gl.CreateProgram();
        gl.AttachShader(p, v);
        gl.AttachShader(p, f);
        for (loc, attr) in [(A_POS, c"a_pos"), (A_A, c"a_a"), (A_B, c"a_b")] {
            gl.BindAttribLocation(p, loc, attr.as_ptr());
        }
        gl.LinkProgram(p);
        gl.DeleteShader(v);
        gl.DeleteShader(f);
        let mut ok = 0;
        gl.GetProgramiv(p, ffi::LINK_STATUS, &mut ok);
        if ok == 0 {
            let log = shader_log(gl, p, true);
            gl.DeleteProgram(p);
            return Err(format!("{name}: link: {log}"));
        }
        let prog = Program {
            id: p,
            u_proj: gl.GetUniformLocation(p, c"u_proj".as_ptr()),
            u_tex: gl.GetUniformLocation(p, c"u_tex".as_ptr()),
            u_step: gl.GetUniformLocation(p, c"u_step".as_ptr()),
            u_mul: gl.GetUniformLocation(p, c"u_mul".as_ptr()),
            u_add: gl.GetUniformLocation(p, c"u_add".as_ptr()),
        };
        gl.UseProgram(p);
        if prog.u_tex >= 0 {
            gl.Uniform1i(prog.u_tex, 0);
        }
        Ok(prog)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Texture {
    pub id: u32,
    pub w: i32,
    pub h: i32,
}

/// Pixel layout of uploads: RGBA (tiny-skia, image) or BGRA (render targets, readback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Rgba,
    Bgra,
}

impl Format {
    fn gl(self) -> u32 {
        match self {
            Format::Rgba => ffi::RGBA,
            Format::Bgra => ffi::BGRA_EXT,
        }
    }
}

/// RGBA -> BGRA copy. Every texture is stored as BGRA: grate samples a full-screen RGBA texture ~4x
/// slower than a BGRA one (measured with the render kit's wallpaper, DESKTOP-SHELL.md Log 2026-11-11).
fn to_bgra(data: &[u8]) -> Vec<u8> {
    let mut v = data.to_vec();
    for p in v.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    v
}

/// A 2D texture (stored BGRA), clamped, linear or nearest filtered. `data` (tightly packed rows, in
/// `format`) may be None.
pub unsafe fn texture(gl: &Gles2, w: i32, h: i32, format: Format, linear: bool, data: Option<&[u8]>) -> Texture {
    unsafe {
        let mut id = 0;
        gl.GenTextures(1, &mut id);
        gl.BindTexture(ffi::TEXTURE_2D, id);
        let filter = if linear { ffi::LINEAR } else { ffi::NEAREST } as i32;
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, filter);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, filter);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE as i32);
        let f = Format::Bgra.gl();
        let converted = match (format, data) {
            (Format::Rgba, Some(d)) => Some(to_bgra(d)),
            _ => None,
        };
        let data = converted.as_deref().or(data);
        let ptr = data.map(|d| d.as_ptr() as *const _).unwrap_or(std::ptr::null());
        gl.TexImage2D(ffi::TEXTURE_2D, 0, f as i32, w, h, 0, f, ffi::UNSIGNED_BYTE, ptr);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        Texture { id, w, h }
    }
}

/// Upload a tightly packed sub-rectangle (`format` describes `data`; the texture is BGRA).
pub unsafe fn upload(gl: &Gles2, tex: Texture, x: i32, y: i32, w: i32, h: i32, format: Format, data: &[u8]) {
    debug_assert!(data.len() >= (w * h * 4) as usize);
    let converted = (format == Format::Rgba).then(|| to_bgra(&data[..(w * h * 4) as usize]));
    let data = converted.as_deref().unwrap_or(data);
    unsafe {
        gl.BindTexture(ffi::TEXTURE_2D, tex.id);
        gl.TexSubImage2D(ffi::TEXTURE_2D, 0, x, y, w, h, Format::Bgra.gl(), ffi::UNSIGNED_BYTE, data.as_ptr() as *const _);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
    }
}

pub unsafe fn delete_texture(gl: &Gles2, tex: Texture) {
    unsafe { gl.DeleteTextures(1, &tex.id) };
}

/// A texture plus a framebuffer object rendering into it (BGRA, the safest render target on grate).
pub struct Target {
    pub tex: Texture,
    pub fbo: u32,
}

pub unsafe fn target(gl: &Gles2, w: i32, h: i32) -> Result<Target, String> {
    unsafe {
        let tex = texture(gl, w, h, Format::Bgra, true, None);
        let mut fbo = 0;
        gl.GenFramebuffers(1, &mut fbo);
        let prev = current_framebuffer(gl);
        gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
        gl.FramebufferTexture2D(ffi::FRAMEBUFFER, ffi::COLOR_ATTACHMENT0, ffi::TEXTURE_2D, tex.id, 0);
        let status = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER);
        gl.BindFramebuffer(ffi::FRAMEBUFFER, prev);
        if status != ffi::FRAMEBUFFER_COMPLETE {
            gl.DeleteFramebuffers(1, &fbo);
            delete_texture(gl, tex);
            return Err(format!("framebuffer {w}x{h} incomplete: {status:#x}"));
        }
        Ok(Target { tex, fbo })
    }
}

pub unsafe fn delete_target(gl: &Gles2, t: Target) {
    unsafe {
        gl.DeleteFramebuffers(1, &t.fbo);
        delete_texture(gl, t.tex);
    }
}

pub unsafe fn current_framebuffer(gl: &Gles2) -> u32 {
    let mut v = 0;
    unsafe { gl.GetIntegerv(ffi::FRAMEBUFFER_BINDING, &mut v) };
    v as u32
}

pub unsafe fn viewport(gl: &Gles2) -> [i32; 4] {
    let mut v = [0i32; 4];
    unsafe { gl.GetIntegerv(ffi::VIEWPORT, v.as_mut_ptr()) };
    v
}
