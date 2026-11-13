//! The glass texture: the wallpaper, downsampled and Gaussian-blurred on the GPU.
//!
//! Halve the wallpaper `levels` times (one bilinear tap per output pixel averages each 2x2 block exactly),
//! then run `passes` rounds of a horizontal + vertical 9-tap blur (17 texels, sigma from the theme) at that
//! size. At 1/8 size and sigma 3 one round equals a ~24 px blur at full size; rounds add in quadrature.
//! The 9-tap shader is 52 EXECs on grate (4 rows), the 13-tap variant is rejected (88), so larger blurs use
//! more rounds or a smaller texture. Panels sample the result at screen UV with linear filtering.
//!
//! Fallback: if a blur program does not compile or link, `cpu_blur` produces the same texture on the CPU.

use smithay::backend::renderer::gles::ffi::{self, Gles2};
use tracing::{info, warn};

use super::gl::{self, Program, Target, Texture};

const PASS_VERT: &str = include_str!("../../shaders/pass.vert");
const COPY_FRAG: &str = include_str!("../../shaders/copy.frag");
const BLUR_FRAG: &str = include_str!("../../shaders/blur.frag");
pub const TAPS: usize = 9;

/// (offset in texels, weight) per bilinear tap: the centre texel, then pairs of texels merged into one
/// tap between them (same as build/gltest/rt/mkcases.py).
pub fn blur_taps(sigma: f32, taps: usize) -> Vec<(f32, f32)> {
    let n = taps - 1; // texels per side
    let w: Vec<f32> = (0..=n).map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp()).collect();
    let total = w[0] + 2.0 * w[1..].iter().sum::<f32>();
    let w: Vec<f32> = w.iter().map(|x| x / total).collect();
    let mut out = vec![(0.0, w[0])];
    let mut i = 1;
    while i <= n {
        let a = w[i];
        let b = if i < n { w[i + 1] } else { 0.0 };
        let off = (i as f32 * a + (i + 1) as f32 * b) / (a + b);
        out.push((off, a + b));
        out.push((-off, a + b));
        i += 2;
    }
    out
}

pub fn blur_source(sigma: f32) -> String {
    let lines: Vec<String> = blur_taps(sigma, TAPS)
        .iter()
        .map(|&(o, w)| {
            if o == 0.0 {
                format!("    s += texture2D(u_tex, v_uv) * {w:.6};")
            } else {
                format!("    s += texture2D(u_tex, v_uv + u_step * {o:.6}) * {w:.6};")
            }
        })
        .collect();
    BLUR_FRAG.replace("@TAPS@", &lines.join("\n"))
}

pub struct GlassBlur {
    copy: Program,
    blur: Program,
    sigma: f32,
}

impl GlassBlur {
    pub unsafe fn new(gl: &Gles2, sigma: f32) -> Result<GlassBlur, String> {
        unsafe {
            Ok(GlassBlur {
                copy: gl::program(gl, "copy", PASS_VERT, COPY_FRAG)?,
                blur: gl::program(gl, "blur", PASS_VERT, &blur_source(sigma))?,
                sigma,
            })
        }
    }

    /// Blur `src` (any size) into a new texture of src / 2^levels. The caller owns the result.
    pub unsafe fn run(&self, gl: &Gles2, src: Texture, levels: u32, passes: u32) -> Result<Target, String> {
        unsafe {
            let prev_fb = gl::current_framebuffer(gl);
            let prev_vp = gl::viewport(gl);
            gl.Disable(ffi::BLEND);
            gl.Disable(ffi::SCISSOR_TEST);
            let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            gl.EnableVertexAttribArray(gl::A_POS);
            gl.VertexAttribPointer(gl::A_POS, 2, ffi::FLOAT, ffi::FALSE, 0, quad.as_ptr() as *const _);
            gl.ActiveTexture(ffi::TEXTURE0);

            let draw = |prog: &Program, from: Texture, to: &Target, step: Option<[f32; 2]>| {
                gl.BindFramebuffer(ffi::FRAMEBUFFER, to.fbo);
                gl.Viewport(0, 0, to.tex.w, to.tex.h);
                gl.UseProgram(prog.id);
                if let Some(s) = step {
                    gl.Uniform2f(prog.u_step, s[0], s[1]);
                }
                gl.BindTexture(ffi::TEXTURE_2D, from.id);
                gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
            };

            let mut result: Result<Target, String> = (|| {
                let (mut w, mut h) = (src.w, src.h);
                let mut cur: Option<Target> = None;
                for _ in 0..levels {
                    w = (w / 2).max(1);
                    h = (h / 2).max(1);
                    let t = gl::target(gl, w, h)?;
                    draw(&self.copy, cur.as_ref().map(|c| c.tex).unwrap_or(src), &t, None);
                    if let Some(old) = cur.replace(t) {
                        gl::delete_target(gl, old);
                    }
                }
                let a = match cur {
                    Some(t) => t,
                    None => {
                        let t = gl::target(gl, w, h)?;
                        draw(&self.copy, src, &t, None);
                        t
                    }
                };
                let b = gl::target(gl, w, h)?;
                for _ in 0..passes {
                    draw(&self.blur, a.tex, &b, Some([1.0 / w as f32, 0.0]));
                    draw(&self.blur, b.tex, &a, Some([0.0, 1.0 / h as f32]));
                }
                gl::delete_target(gl, b);
                Ok(a)
            })();
            let err = gl.GetError();
            if err != ffi::NO_ERROR && result.is_ok() {
                result = Err(format!("GL error {err:#x} in blur passes"));
            }

            gl.DisableVertexAttribArray(gl::A_POS);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            gl.UseProgram(0);
            gl.BindFramebuffer(ffi::FRAMEBUFFER, prev_fb);
            gl.Viewport(prev_vp[0], prev_vp[1], prev_vp[2], prev_vp[3]);
            gl.Enable(ffi::SCISSOR_TEST);
            gl.Enable(ffi::BLEND);
            gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
            if result.is_ok() {
                info!(src = ?(src.w, src.h), levels, passes, sigma = self.sigma, "glass blurred on the GPU");
            }
            result
        }
    }
}

/// CPU fallback: box-downsample RGBA `px` (w x h) by 2^levels, then 3 box blurs per pass (~Gaussian).
pub fn cpu_blur(px: &[u8], w: u32, h: u32, levels: u32, radius: u32, passes: u32) -> (Vec<u8>, u32, u32) {
    warn!("blurring the glass texture on the CPU (GPU path unavailable)");
    let f = 1u32 << levels;
    let (sw, sh) = ((w / f).max(1), (h / f).max(1));
    let mut img = vec![0f32; (sw * sh * 4) as usize];
    for y in 0..sh {
        for x in 0..sw {
            let mut acc = [0f32; 4];
            for yy in 0..f {
                for xx in 0..f {
                    let i = (((y * f + yy).min(h - 1) * w + (x * f + xx).min(w - 1)) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += px[i + c] as f32;
                    }
                }
            }
            let o = ((y * sw + x) * 4) as usize;
            for c in 0..4 {
                img[o + c] = acc[c] / (f * f) as f32;
            }
        }
    }
    let r = radius as i32;
    let mut tmp = img.clone();
    for _ in 0..passes * 3 {
        for horizontal in [true, false] {
            let (src, dst) = (&img, &mut tmp);
            for y in 0..sh as i32 {
                for x in 0..sw as i32 {
                    let mut acc = [0f32; 4];
                    for d in -r..=r {
                        let (sx, sy) = if horizontal {
                            ((x + d).clamp(0, sw as i32 - 1), y)
                        } else {
                            (x, (y + d).clamp(0, sh as i32 - 1))
                        };
                        let i = ((sy as u32 * sw + sx as u32) * 4) as usize;
                        for c in 0..4 {
                            acc[c] += src[i + c];
                        }
                    }
                    let o = ((y as u32 * sw + x as u32) * 4) as usize;
                    for c in 0..4 {
                        dst[o + c] = acc[c] / (2 * r + 1) as f32;
                    }
                }
            }
            std::mem::swap(&mut img, &mut tmp);
        }
    }
    (img.iter().map(|v| v.round().clamp(0.0, 255.0) as u8).collect(), sw, sh)
}

#[cfg(test)]
mod tests {
    #[test]
    fn taps_are_normalized_and_symmetric() {
        let t = super::blur_taps(3.0, super::TAPS);
        assert_eq!(t.len(), super::TAPS);
        let sum: f32 = t.iter().map(|(_, w)| w).sum();
        assert!((sum - 1.0).abs() < 1e-5);
        assert!((t[1].0 + t[2].0).abs() < 1e-6);
        // Same numbers as the gltest case (rt-blur9: 1.458430 0.233308).
        assert!((t[1].0 - 1.458430).abs() < 1e-4 && (t[1].1 - 0.233308).abs() < 1e-4);
    }
}
