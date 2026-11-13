//! The render kit's GPU side: the UI programs (shaders/*.frag, all checked on grate with
//! build/gltest/rt/run-rt.sh), one stream VBO, a static quad index buffer, and `draw` which replays a frame's
//! batches inside Smithay's frame, scissored to each damage rectangle.

use std::cell::Cell;

use smithay::backend::renderer::gles::ffi::{self, Gles2};
use tracing::info;

use super::{
    batch::{Batch, Pipe, Style, VERTEX_SIZE, Vertex},
    gl::{self, A_A, A_B, A_POS, Program},
};

const UI_VERT: &str = include_str!("../../shaders/ui.vert");
const FILL_FRAG: &str = include_str!("../../shaders/fill.frag");
const RRECT_FRAG: &str = include_str!("../../shaders/rrect.frag");
const TEX_FRAG: &str = include_str!("../../shaders/tex.frag");
const TEXEDGE_FRAG: &str = include_str!("../../shaders/texedge.frag");

/// RTSHELL_UI_TIMING=2: per-batch GPU times of the last frame (pipe, quads, scissor, microseconds).
pub static BATCH_TIMES: std::sync::Mutex<Vec<(Pipe, u32, [i32; 4], u32)>> = std::sync::Mutex::new(Vec::new());

fn batch_timing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RTSHELL_UI_TIMING").is_ok_and(|v| v == "2"))
}

/// u16 indices: at most 16384 quads (65536 vertices) per draw call; longer batches are split.
const MAX_QUADS_PER_DRAW: u32 = 16384;

pub struct Kit {
    fill: Program,
    rrect: Program,
    tex: Program,
    texedge: Program,
    vbo: u32,
    ibo: u32,
    /// Size of the VBO's storage in bytes (grown, never shrunk).
    vbo_size: Cell<usize>,
}

impl Kit {
    pub unsafe fn new(gl: &Gles2) -> Result<Kit, String> {
        unsafe {
            let fill = gl::program(gl, "fill", UI_VERT, FILL_FRAG)?;
            let rrect = gl::program(gl, "rrect", UI_VERT, RRECT_FRAG)?;
            let tex = gl::program(gl, "tex", UI_VERT, TEX_FRAG)?;
            let texedge = gl::program(gl, "texedge", UI_VERT, TEXEDGE_FRAG)?;
            let mut bufs = [0u32; 2];
            gl.GenBuffers(2, bufs.as_mut_ptr());
            let indices: Vec<u16> = (0..MAX_QUADS_PER_DRAW as u16)
                .flat_map(|i| {
                    let b = i.wrapping_mul(4);
                    [b, b + 1, b + 2, b + 2, b + 1, b + 3]
                })
                .collect();
            gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, bufs[1]);
            gl.BufferData(
                ffi::ELEMENT_ARRAY_BUFFER,
                (indices.len() * 2) as isize,
                indices.as_ptr() as *const _,
                ffi::STATIC_DRAW,
            );
            gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, 0);
            gl.UseProgram(0);
            info!("render kit ready (fill, rrect, tex, texedge programs)");
            Ok(Kit { fill, rrect, tex, texedge, vbo: bufs[0], ibo: bufs[1], vbo_size: Cell::new(0) })
        }
    }

    /// Draw `batches` (vertices in batch order) with projection `proj` (Smithay's frame projection), once per
    /// scissor box (framebuffer coordinates). Restores the state listed in gl.rs. `instancing`: the context
    /// has instanced arrays (not grate); Smithay then leaves attribute divisors set, which we must clear.
    pub unsafe fn draw(&self, gl: &Gles2, proj: &[f32; 9], vertices: &[Vertex], batches: &[Batch], scissors: &[[i32; 4]], instancing: bool) {
        if vertices.is_empty() || batches.is_empty() || scissors.is_empty() {
            return;
        }
        unsafe {
            gl.BindBuffer(ffi::ARRAY_BUFFER, self.vbo);
            let bytes = vertices.len() * VERTEX_SIZE;
            if bytes > self.vbo_size.get() {
                let size = bytes.next_power_of_two();
                gl.BufferData(ffi::ARRAY_BUFFER, size as isize, std::ptr::null(), ffi::STREAM_DRAW);
                self.vbo_size.set(size);
            }
            gl.BufferSubData(ffi::ARRAY_BUFFER, 0, bytes as isize, vertices.as_ptr() as *const _);
            gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, self.ibo);
            let stride = VERTEX_SIZE as i32;
            let attribs: [(u32, i32, u32, u8, usize); 3] = [
                (A_POS, 2, ffi::FLOAT, ffi::FALSE, 0),
                (A_A, 4, ffi::FLOAT, ffi::FALSE, 8),
                (A_B, 4, ffi::FLOAT, ffi::FALSE, 24),
            ];
            for (loc, n, ty, norm, off) in attribs {
                gl.EnableVertexAttribArray(loc);
                gl.VertexAttribPointer(loc, n, ty, norm, stride, off as *const _);
                if instancing {
                    gl.VertexAttribDivisor(loc, 0);
                }
            }
            for p in [&self.fill, &self.rrect, &self.tex, &self.texedge] {
                gl.UseProgram(p.id);
                gl.UniformMatrix3fv(p.u_proj, 1, ffi::FALSE, proj.as_ptr());
            }
            gl.ActiveTexture(ffi::TEXTURE0);
            let detail = batch_timing();
            let mut times = Vec::new();
            if detail {
                // Absorb any wait for the target buffer (implicit fences) before timing batches.
                gl.Scissor(0, 0, 1, 1);
                gl.UseProgram(self.fill.id);
                gl.DrawElements(ffi::TRIANGLES, 6, ffi::UNSIGNED_SHORT, std::ptr::null());
                gl.Finish();
            }
            for s in scissors {
                gl.Scissor(s[0], s[1], s[2], s[3]);
                let (mut program, mut texture, mut blend) = (0u32, u32::MAX, true);
                // Uniform colours last set on the tex and texedge programs.
                let mut styles: [Option<Style>; 2] = [None, None];
                for b in batches {
                    let (p, t, style) = match b.pipe {
                        Pipe::Fill => (&self.fill, None, None),
                        Pipe::RRect => (&self.rrect, None, None),
                        Pipe::Tex(id, s) => (&self.tex, Some(id), Some((0, s))),
                        Pipe::TexOpaque(id, s) => (&self.tex, Some(id), Some((0, s))),
                        Pipe::TexEdge(id, s) => (&self.texedge, Some(id), Some((1, s))),
                    };
                    let opaque = matches!(b.pipe, Pipe::TexOpaque(..));
                    if opaque == blend {
                        if opaque {
                            gl.Disable(ffi::BLEND);
                        } else {
                            gl.Enable(ffi::BLEND);
                        }
                        blend = !opaque;
                    }
                    if p.id != program {
                        gl.UseProgram(p.id);
                        program = p.id;
                    }
                    if let Some((slot, st)) = style
                        && styles[slot] != Some(st)
                    {
                        let (m, a) = (st.mul_f(), st.add_f());
                        gl.Uniform4f(p.u_mul, m[0], m[1], m[2], m[3]);
                        gl.Uniform4f(p.u_add, a[0], a[1], a[2], a[3]);
                        styles[slot] = Some(st);
                    }
                    if let Some(t) = t
                        && t != texture
                    {
                        gl.BindTexture(ffi::TEXTURE_2D, t);
                        texture = t;
                    }
                    let t0 = detail.then(|| {
                        gl.Finish();
                        std::time::Instant::now()
                    });
                    let mut first = b.first;
                    let mut left = b.count;
                    while left > 0 {
                        let n = left.min(MAX_QUADS_PER_DRAW);
                        if first + n <= MAX_QUADS_PER_DRAW {
                            // Index i*6 of the static buffer addresses vertex 4*i: offset into it.
                            let off = (first * 6 * 2) as usize;
                            gl.DrawElements(ffi::TRIANGLES, (n * 6) as i32, ffi::UNSIGNED_SHORT, off as *const _);
                        } else {
                            // Past the u16 range: rebase the attribute pointers for this chunk.
                            let base = first as usize * 4 * VERTEX_SIZE;
                            for (loc, k, ty, norm, off) in attribs {
                                gl.VertexAttribPointer(loc, k, ty, norm, stride, (base + off) as *const _);
                            }
                            gl.DrawElements(ffi::TRIANGLES, (n * 6) as i32, ffi::UNSIGNED_SHORT, std::ptr::null());
                            for (loc, k, ty, norm, off) in attribs {
                                gl.VertexAttribPointer(loc, k, ty, norm, stride, off as *const _);
                            }
                        }
                        first += n;
                        left -= n;
                    }
                    if let Some(t0) = t0 {
                        gl.Finish();
                        times.push((b.pipe, b.count, *s, t0.elapsed().as_micros() as u32));
                    }
                }
                if !blend {
                    gl.Enable(ffi::BLEND);
                }
            }
            if detail {
                *BATCH_TIMES.lock().unwrap() = times;
            }
            for (loc, ..) in attribs {
                gl.DisableVertexAttribArray(loc);
            }
            gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, 0);
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            gl.UseProgram(0);
        }
    }
}
