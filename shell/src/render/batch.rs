//! Draw lists: the shell UI as axis-aligned quads, merged into a few draw calls per frame.
//!
//! Every primitive becomes quads with one vertex layout (ui.vert): a position and two generic vec4s. Painter's
//! order is the order of `DrawList::quads`; `batches()` then reorders them into runs of the same pipeline
//! (program + texture + per-batch uniforms) as long as that cannot change the picture: a quad may join an
//! earlier batch only if no batch drawn after that one overlaps it. It joins the earliest such batch, and a
//! quad that needs a new batch gets it right after the last batch it overlaps (not at the end), which leaves
//! later batches open for the quads that follow. A grid of cards (shadow, card, icon, label each) thus costs
//! one draw per kind, not per card.
//!
//! Cost model (grate, full screen 1366x768, measured with build/gltest/fillbench "rt v2"/"rt kitvar"): a program
//! reading one varying costs 2-3 ms (4 blended); a second varying 7-11 ms, a third 16 ms; the coverage
//! (rounded edge) programs 20-55 ms. So large areas only use `fill` (varying colour) and `tex` (texture times
//! per-batch uniform colours: opacity and tint are uniforms, so a fading panel's quads simply form their
//! own batches), and a rounded rect is cut into its four corner squares and four 2 px edge bands (coverage
//! programs) around a plus-shaped interior (fill / tex). In the coverage pieces q = |p - centre| - (half
//! size - r) per axis is linear because no piece crosses the centre, and its values stay small near the
//! edges, where FP20 precision matters; edge bands pin the other axis to "far inside".

use super::gl::Texture;

/// Premultiplied RGBA in 0..1.
pub type Rgba = [f32; 4];

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 2],
    /// fill/rrect: colour; tex/texedge: (u, v, 0, 0).
    pub a: [f32; 4],
    /// rrect/texedge: (qx, qy, r, 0).
    pub b: [f32; 4],
}

pub const VERTEX_SIZE: usize = std::mem::size_of::<Vertex>();
const _: () = assert!(VERTEX_SIZE == 40);

/// Per-batch colours of the textured programs: fragment = texel * mul + add (premultiplied, 8 bit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Style {
    pub mul: [u8; 4],
    pub add: [u8; 4],
}

impl Style {
    pub fn new(mul: Rgba, add: Rgba) -> Style {
        Style { mul: to_u8(mul), add: to_u8(add) }
    }
    pub fn mul_f(&self) -> Rgba {
        self.mul.map(|v| v as f32 / 255.0)
    }
    pub fn add_f(&self) -> Rgba {
        self.add.map(|v| v as f32 / 255.0)
    }
}

/// Which program, texture and uniforms draw a quad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pipe {
    /// Varying colour, no coverage: insides of solid shapes, dim overlays.
    Fill,
    /// Solid rounded-rect edge pieces (coverage).
    RRect,
    /// Texture * style: glyphs, icons, shadow slices, insides of images and glass.
    Tex(u32, Style),
    /// Textured rounded edge pieces (coverage).
    TexEdge(u32, Style),
    /// Texture * style drawn with blending off (opaque backdrop at full opacity).
    TexOpaque(u32, Style),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        Rect::new(x, y, (self.right().min(o.right()) - x).max(0.0), (self.bottom().min(o.bottom()) - y).max(0.0))
    }
    pub fn overlaps(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d)
    }
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    /// Smallest whole-pixel rect containing this one.
    pub fn round_out(&self) -> Rect {
        let x = self.x.floor();
        let y = self.y.floor();
        Rect::new(x, y, self.right().ceil() - x, self.bottom().ceil() - y)
    }
}

/// One quad: corners top-left, top-right, bottom-left, bottom-right. Attributes vary linearly in x and y.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    pub pipe: Pipe,
    pub v: [Vertex; 4],
}

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

impl Quad {
    pub fn bounds(&self) -> Rect {
        let a = self.v[0].pos;
        let b = self.v[3].pos;
        Rect::new(a[0], a[1], b[0] - a[0], b[1] - a[1])
    }

    /// Cut the quad to `clip`, interpolating every attribute. None if nothing is left.
    fn clipped(&self, clip: &Rect) -> Option<Quad> {
        let b = self.bounds();
        let c = b.intersect(clip);
        if c.is_empty() {
            return None;
        }
        if c == b {
            return Some(*self);
        }
        let at = |fx: f32, fy: f32| -> Vertex {
            let [tl, tr, bl, br] = &self.v;
            Vertex {
                pos: [b.x + b.w * fx, b.y + b.h * fy],
                a: lerp4(lerp4(tl.a, tr.a, fx), lerp4(bl.a, br.a, fx), fy),
                b: lerp4(lerp4(tl.b, tr.b, fx), lerp4(bl.b, br.b, fx), fy),
            }
        };
        let fx0 = (c.x - b.x) / b.w;
        let fx1 = (c.right() - b.x) / b.w;
        let fy0 = (c.y - b.y) / b.h;
        let fy1 = (c.bottom() - b.y) / b.h;
        Some(Quad { pipe: self.pipe, v: [at(fx0, fy0), at(fx1, fy0), at(fx0, fy1), at(fx1, fy1)] })
    }
}

fn to_u8(c: Rgba) -> [u8; 4] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

pub fn scale(c: Rgba, k: f32) -> Rgba {
    c.map(|v| v * k)
}

/// b value that gives full coverage everywhere.
const FAR: f32 = -4096.0;
/// Rounded shapes are extended by this much outside their rect so the anti-aliased edge has pixels to land on.
const AA: f32 = 1.0;

/// What a primitive's pieces are drawn with: interior and edge pipes, and how `a` is computed per position.
#[derive(Clone, Copy)]
enum Fillish {
    /// Solid colour (premultiplied, opacity applied).
    Color(Rgba),
    /// Texture coordinates from a linear map: uv = (u0 + du*x, v0 + dv*y) on layout positions.
    Uv { u0: f32, du: f32, v0: f32, dv: f32 },
    /// Texture coordinates from screen positions (glass): uv = (screen - origin) / size.
    ScreenUv { xf: Xf, screen: Rect },
}

impl Fillish {
    fn a(&self, x: f32, y: f32) -> [f32; 4] {
        match *self {
            Fillish::Color(c) => c,
            Fillish::Uv { u0, du, v0, dv } => [u0 + du * x, v0 + dv * y, 0.0, 0.0],
            Fillish::ScreenUv { xf, screen } => {
                let (sx, sy) = xf.apply(x, y);
                [(sx - screen.x) / screen.w, (sy - screen.y) / screen.h, 0.0, 0.0]
            }
        }
    }
}

/// Uniform scale + translation: screen = s * p + t.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xf {
    pub s: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Xf {
    pub const IDENTITY: Xf = Xf { s: 1.0, tx: 0.0, ty: 0.0 };

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (self.s * x + self.tx, self.s * y + self.ty)
    }

    pub fn rect(&self, r: Rect) -> Rect {
        let (x, y) = self.apply(r.x, r.y);
        Rect::new(x, y, r.w * self.s, r.h * self.s)
    }

    /// self after `inner` (inner applies first).
    pub fn then(&self, inner: Xf) -> Xf {
        Xf { s: self.s * inner.s, tx: self.s * inner.tx + self.tx, ty: self.s * inner.ty + self.ty }
    }

    pub fn invert(&self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.tx) / self.s, (y - self.ty) / self.s)
    }
}

/// A frame's quads in painter's order. Primitives are given in layout coordinates and pass through the
/// current transform and opacity (`save`/`restore` around `transform`/`fade`), then the clip stack.
pub struct DrawList {
    pub quads: Vec<Quad>,
    clips: Vec<Rect>,
    xf: Xf,
    opacity: f32,
    /// Colour multiplier (rgb only) for a black scrim drawn later: see `darken`.
    shade: f32,
    saved: Vec<(Xf, f32, f32)>,
}

impl Default for DrawList {
    fn default() -> Self {
        DrawList { quads: Vec::new(), clips: Vec::new(), xf: Xf::IDENTITY, opacity: 1.0, shade: 1.0, saved: Vec::new() }
    }
}

impl DrawList {
    pub fn clear(&mut self) {
        self.quads.clear();
        self.clips.clear();
        self.saved.clear();
        self.xf = Xf::IDENTITY;
        self.opacity = 1.0;
        self.shade = 1.0;
    }

    pub fn save(&mut self) {
        self.saved.push((self.xf, self.opacity, self.shade));
    }

    pub fn restore(&mut self) {
        if let Some((xf, o, d)) = self.saved.pop() {
            self.xf = xf;
            self.opacity = o;
            self.shade = d;
        }
    }

    /// Compose a local transform. At scale 1 translations are rounded to whole pixels, so text and icons
    /// stay sharp while things slide (shapes still anti-alias their edges).
    pub fn transform(&mut self, local: Xf) {
        let mut xf = self.xf.then(local);
        if (xf.s - 1.0).abs() < 1e-4 {
            xf = Xf { s: 1.0, tx: xf.tx.round(), ty: xf.ty.round() };
        }
        self.xf = xf;
    }

    #[allow(dead_code)]
    pub fn xf(&self) -> Xf {
        self.xf
    }

    pub fn fade(&mut self, opacity: f32) {
        self.opacity *= opacity;
    }

    #[allow(dead_code)]
    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    /// Multiply the rgb of everything drawn from now on by `k`. Over an opaque backdrop this is exactly a
    /// black scrim of alpha 1 - k drawn on top afterwards (premultiplied source-over is linear), without a
    /// full-screen blended pass (~4 ms on grate per frame while a scrim fades).
    pub fn darken(&mut self, k: f32) {
        self.shade *= k.clamp(0.0, 1.0);
    }

    /// Opacity applied to all channels, shade to rgb.
    fn tone(&self, c: Rgba) -> Rgba {
        let o = self.opacity;
        let d = self.shade * o;
        [c[0] * d, c[1] * d, c[2] * d, c[3] * o]
    }

    /// Clip to `r` (layout coordinates, through the current transform).
    pub fn push_clip(&mut self, r: Rect) {
        let r = self.xf.rect(r);
        let r = match self.clips.last() {
            Some(c) => c.intersect(&r),
            None => r,
        };
        self.clips.push(r);
    }

    pub fn pop_clip(&mut self) {
        self.clips.pop();
    }

    /// Transform positions (and the pixel-valued q) to screen space, then clip.
    fn push(&mut self, mut q: Quad) {
        if self.xf != Xf::IDENTITY {
            let s = self.xf.s;
            for v in &mut q.v {
                let (x, y) = self.xf.apply(v.pos[0], v.pos[1]);
                v.pos = [x, y];
                v.b = [v.b[0] * s, v.b[1] * s, v.b[2] * s, v.b[3]];
            }
        }
        match self.clips.last() {
            Some(c) => {
                if let Some(q) = q.clipped(c) {
                    self.quads.push(q);
                }
            }
            None => self.quads.push(q),
        }
    }

    /// One quad over `r` with `a` from `fill`, full coverage.
    fn plain(&mut self, pipe: Pipe, r: Rect, fill: Fillish) {
        if r.is_empty() {
            return;
        }
        let v = |x: f32, y: f32| Vertex { pos: [x, y], a: fill.a(x, y), b: [FAR, FAR, 0.0, 0.0] };
        self.push(Quad { pipe, v: [v(r.x, r.y), v(r.right(), r.y), v(r.x, r.bottom()), v(r.right(), r.bottom())] });
    }

    /// A rounded rect grown by AA outward: coverage pieces (corners, edge bands) with `edge`, the interior
    /// with `inner`.
    fn rounded(&mut self, edge: Pipe, inner: Pipe, r: Rect, radius: f32, fill: Fillish) {
        if r.is_empty() {
            return;
        }
        let hw = r.w * 0.5;
        let hh = r.h * 0.5;
        let rad = radius.clamp(0.0, hw.min(hh));
        let (cx, cy) = (r.x + hw, r.y + hh);
        let (ix, iy) = (hw - rad, hh - rad);
        let aa = AA / self.xf.s.max(1e-3);
        let qx = move |x: f32| (x - cx).abs() - ix;
        let qy = move |y: f32| (y - cy).abs() - iy;
        let piece = |this: &mut Self, pipe: Pipe, a: (f32, f32), b: (f32, f32), q: &dyn Fn(f32, f32) -> [f32; 4]| {
            if b.0 - a.0 <= 0.0 || b.1 - a.1 <= 0.0 {
                return;
            }
            let v = |x: f32, y: f32| Vertex { pos: [x, y], a: fill.a(x, y), b: q(x, y) };
            this.push(Quad { pipe, v: [v(a.0, a.1), v(b.0, a.1), v(a.0, b.1), v(b.0, b.1)] });
        };
        let corner = |x: f32, y: f32| [qx(x), qy(y), rad, 0.0];
        let (x0, x1, y0, y1) = (r.x, r.right(), r.y, r.bottom());
        if hw < 1.5 || hh < 1.5 {
            // Too thin to cut up: four quadrants, all with coverage (no piece crosses the centre).
            piece(self, edge, (x0 - aa, y0 - aa), (cx, cy), &corner);
            piece(self, edge, (cx, y0 - aa), (x1 + aa, cy), &corner);
            piece(self, edge, (x0 - aa, cy), (cx, y1 + aa), &corner);
            piece(self, edge, (cx, cy), (x1 + aa, y1 + aa), &corner);
            return;
        }
        // Corner squares reach c into the rect; the interior is inset by 1 px from straight edges.
        let c = rad.max(1.0);
        let horiz = |_: f32, y: f32| [FAR, qy(y), rad, 0.0];
        let vert = |x: f32, _: f32| [qx(x), FAR, rad, 0.0];
        let inside = |_: f32, _: f32| [FAR, FAR, 0.0, 0.0];
        piece(self, edge, (x0 - aa, y0 - aa), (x0 + c, y0 + c), &corner);
        piece(self, edge, (x1 - c, y0 - aa), (x1 + aa, y0 + c), &corner);
        piece(self, edge, (x0 - aa, y1 - c), (x0 + c, y1 + aa), &corner);
        piece(self, edge, (x1 - c, y1 - c), (x1 + aa, y1 + aa), &corner);
        piece(self, edge, (x0 + c, y0 - aa), (x1 - c, y0 + 1.0), &horiz);
        piece(self, edge, (x0 + c, y1 - 1.0), (x1 - c, y1 + aa), &horiz);
        piece(self, edge, (x0 - aa, y0 + c), (x0 + 1.0, y1 - c), &vert);
        piece(self, edge, (x1 - 1.0, y0 + c), (x1 + aa, y1 - c), &vert);
        // Interior (full coverage): the middle column, then the left and right columns between the corners.
        piece(self, inner, (x0 + c, y0 + 1.0), (x1 - c, y1 - 1.0), &inside);
        piece(self, inner, (x0 + 1.0, y0 + c), (x0 + c, y1 - c), &inside);
        piece(self, inner, (x1 - c, y0 + c), (x1 - 1.0, y1 - c), &inside);
    }

    /// Solid colour; rounded and anti-aliased when `radius` > 0 or the rect is not on whole pixels.
    pub fn rect(&mut self, r: Rect, radius: f32, color: Rgba) {
        if color[3] <= 0.0 || self.opacity <= 0.0 {
            return;
        }
        let fill = Fillish::Color(self.tone(color));
        let sr = self.xf.rect(r);
        if radius <= 0.0 && sr.round_out() == sr {
            self.plain(Pipe::Fill, r, fill);
        } else {
            self.rounded(Pipe::RRect, Pipe::Fill, r, radius, fill);
        }
    }

    fn uv_map(r: Rect, uv: [f32; 4]) -> Fillish {
        let du = (uv[2] - uv[0]) / r.w;
        let dv = (uv[3] - uv[1]) / r.h;
        Fillish::Uv { u0: uv[0] - du * r.x, du, v0: uv[1] - dv * r.y, dv }
    }

    /// A texture region `uv` (u0, v0, u1, v1) stretched over `r`: fragment = (texel * mul + add) * coverage.
    pub fn texture(&mut self, tex: Texture, r: Rect, uv: [f32; 4], radius: f32, mul: Rgba, add: Rgba) {
        if self.opacity <= 0.0 || r.is_empty() {
            return;
        }
        let style = Style::new(self.tone(mul), self.tone(add));
        let fill = Self::uv_map(r, uv);
        if radius <= 0.0 {
            self.plain(Pipe::Tex(tex.id, style), r, fill);
        } else {
            self.rounded(Pipe::TexEdge(tex.id, style), Pipe::Tex(tex.id, style), r, radius, fill);
        }
    }

    /// An opaque texture covering `r` (wallpaper): drawn without blending. Only for content that is opaque
    /// and drawn at full opacity; otherwise falls back to the blended path.
    pub fn opaque_texture(&mut self, tex: Texture, r: Rect, uv: [f32; 4]) {
        if self.opacity < 1.0 {
            return self.texture(tex, r, uv, 0.0, [1.0; 4], [0.0; 4]);
        }
        let style = Style::new(self.tone([1.0; 4]), [0.0; 4]);
        self.plain(Pipe::TexOpaque(tex.id, style), r, Self::uv_map(r, uv));
    }

    /// Glass: the blurred wallpaper (`glass`, covering `screen`) sampled at the screen position, tinted.
    pub fn glass(&mut self, glass: Texture, screen: Rect, r: Rect, radius: f32, tint: Rgba, opacity: f32) {
        let o = opacity * self.opacity;
        if o <= 0.0 || r.is_empty() {
            return;
        }
        let style = Style::new(self.tone(scale([1.0 - tint[3]; 4], opacity)), self.tone(scale(tint, opacity)));
        // UVs from screen positions (after the transform): the panel shows what is behind it.
        let fill = Fillish::ScreenUv { xf: self.xf, screen };
        if radius <= 0.0 {
            self.plain(Pipe::Tex(glass.id, style), r, fill);
        } else {
            self.rounded(Pipe::TexEdge(glass.id, style), Pipe::Tex(glass.id, style), r, radius, fill);
        }
    }

    /// 9-slice: `src` is the atlas region of a square slice image whose centre texel is the stretchable
    /// part; `k` is the corner size in pixels (texels). `outer` is the destination. `hollow`: skip the
    /// centre slice (a shadow's centre is under its card).
    pub fn nine_slice(&mut self, tex: Texture, src: Rect, k: f32, outer: Rect, color: Rgba, hollow: bool) {
        if self.opacity <= 0.0 {
            return;
        }
        let pipe = Pipe::Tex(tex.id, Style::new(self.tone(color), [0.0; 4]));
        let (tw, th) = (tex.w as f32, tex.h as f32);
        let kx = k.min(outer.w * 0.5);
        let ky = k.min(outer.h * 0.5);
        let dx = [outer.x, outer.x + kx, outer.right() - kx, outer.right()];
        let dy = [outer.y, outer.y + ky, outer.bottom() - ky, outer.bottom()];
        // Source columns/rows: corner, the centre texel only, corner.
        let (mx, my) = (src.x + src.w * 0.5, src.y + src.h * 0.5);
        let scol = [(src.x, src.x + kx), (mx, mx), (src.right() - kx, src.right())];
        let srow = [(src.y, src.y + ky), (my, my), (src.bottom() - ky, src.bottom())];
        for j in 0..3 {
            for i in 0..3 {
                if hollow && i == 1 && j == 1 {
                    continue;
                }
                let d = Rect::new(dx[i], dy[j], dx[i + 1] - dx[i], dy[j + 1] - dy[j]);
                if d.is_empty() {
                    continue;
                }
                let uv = [scol[i].0 / tw, srow[j].0 / th, scol[i].1 / tw, srow[j].1 / th];
                self.plain(pipe, d, Self::uv_map(d, uv));
            }
        }
    }

    /// Bounds of everything drawn, or None.
    #[allow(dead_code)]
    pub fn bounds(&self) -> Option<Rect> {
        self.quads.iter().map(|q| q.bounds()).reduce(|a, b| a.union(&b))
    }
}

/// One draw call: `count` quads starting at quad `first` of the reordered vertex array.
#[derive(Debug, Clone, Copy)]
pub struct Batch {
    pub pipe: Pipe,
    pub first: u32,
    pub count: u32,
}

/// Merge quads into batches (see the module comment), returning the batches and the vertices in batch order.
pub fn batches(quads: &[Quad], vertices: &mut Vec<Vertex>) -> Vec<Batch> {
    struct Open {
        pipe: Pipe,
        bounds: Rect,
        members: Vec<u32>,
    }
    let mut open: Vec<Open> = Vec::new();
    // How far back to search; keeps the worst case linear-ish for long lists.
    const LOOKBACK: usize = 48;
    for (i, q) in quads.iter().enumerate() {
        let b = q.bounds();
        let mut candidate = None;
        // Where a new batch may go: after the last batch this quad overlaps (or the search window).
        let mut insert_at = open.len().saturating_sub(LOOKBACK);
        for k in (insert_at..open.len()).rev() {
            let o = &open[k];
            if o.pipe == q.pipe {
                candidate = Some(k);
            }
            if o.bounds.overlaps(&b) {
                insert_at = k + 1;
                break;
            }
        }
        match candidate {
            Some(k) => {
                let o = &mut open[k];
                o.bounds = o.bounds.union(&b);
                o.members.push(i as u32);
            }
            None => open.insert(insert_at, Open { pipe: q.pipe, bounds: b, members: vec![i as u32] }),
        }
    }
    vertices.clear();
    vertices.reserve(quads.len() * 4);
    let mut out = Vec::with_capacity(open.len());
    for o in open {
        let first = (vertices.len() / 4) as u32;
        for &m in &o.members {
            vertices.extend_from_slice(&quads[m as usize].v);
        }
        out.push(Batch { pipe: o.pipe, first, count: o.members.len() as u32 });
    }
    out
}

/// What changed between two frames' quad lists: the bounds of the differing middle part (after the common
/// prefix and suffix), in both lists. None when identical.
pub fn diff_damage(old: &[Quad], new: &[Quad]) -> Option<Rect> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old.iter().rev().zip(new.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    let mut r = Rect::new(0.0, 0.0, 0.0, 0.0);
    for q in old[prefix..old.len() - suffix].iter().chain(&new[prefix..new.len() - suffix]) {
        r = r.union(&q.bounds());
    }
    Some(r.round_out())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tex(id: u32) -> Texture {
        Texture { id, w: 16, h: 16 }
    }

    #[test]
    fn rounded_rect_pieces_tile_the_grown_rect() {
        let mut l = DrawList::default();
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        l.rect(r, 12.0, [1.0; 4]);
        assert_eq!(l.quads.len(), 11);
        // Areas add up to the rect grown by AA on each side (no gaps, no overlaps).
        let area: f32 = l.quads.iter().map(|q| q.bounds().w * q.bounds().h).sum();
        assert!((area - 102.0 * 52.0).abs() < 1e-2, "{area}");
        let (cx, cy) = (60.0, 45.0);
        for q in l.quads.iter().filter(|q| q.pipe == Pipe::RRect) {
            for v in &q.v {
                // Coverage pieces: q = |p - c| - inner half size, or "far inside" on the pinned axis.
                assert!(v.b[0] == FAR || ((v.pos[0] - cx).abs() - 38.0 - v.b[0]).abs() < 1e-4);
                assert!(v.b[1] == FAR || ((v.pos[1] - cy).abs() - 13.0 - v.b[1]).abs() < 1e-4);
            }
        }
        // Only a thin ring runs the coverage shader.
        let edge: f32 = l.quads.iter().filter(|q| q.pipe == Pipe::RRect).map(|q| q.bounds().w * q.bounds().h).sum();
        assert!(edge < 0.5 * area, "{edge} of {area}");
    }

    #[test]
    fn clipping_interpolates() {
        let mut l = DrawList::default();
        l.push_clip(Rect::new(0.0, 0.0, 50.0, 100.0));
        l.texture(tex(1), Rect::new(0.0, 0.0, 100.0, 100.0), [0.0, 0.0, 1.0, 1.0], 0.0, [1.0; 4], [0.0; 4]);
        let q = l.quads[0];
        assert_eq!(q.v[1].pos, [50.0, 0.0]);
        assert_eq!(&q.v[1].a[..2], &[0.5, 0.0]);
    }

    #[test]
    fn opacity_and_colour_go_into_the_batch_key() {
        let mut l = DrawList::default();
        let t = tex(3);
        l.texture(t, Rect::new(0.0, 0.0, 10.0, 10.0), [0.0, 0.0, 1.0, 1.0], 0.0, [1.0; 4], [0.0; 4]);
        l.save();
        l.fade(0.5);
        l.texture(t, Rect::new(20.0, 0.0, 10.0, 10.0), [0.0, 0.0, 1.0, 1.0], 0.0, [1.0; 4], [0.0; 4]);
        l.restore();
        assert_ne!(l.quads[0].pipe, l.quads[1].pipe);
        let mut v = Vec::new();
        assert_eq!(batches(&l.quads, &mut v).len(), 2);
    }

    #[test]
    fn batching_merges_non_overlapping_and_keeps_order_when_overlapping() {
        let mut l = DrawList::default();
        // Two cards side by side: rrect (edge + fill) + texture each -> 3 batches.
        for x in [0.0, 200.0] {
            l.rect(Rect::new(x, 0.0, 100.0, 100.0), 8.0, [1.0; 4]);
            l.texture(tex(7), Rect::new(x + 10.0, 10.0, 20.0, 20.0), [0.0, 0.0, 1.0, 1.0], 0.0, [1.0; 4], [0.0; 4]);
        }
        let mut v = Vec::new();
        assert_eq!(batches(&l.quads, &mut v).len(), 3);
        // A third rrect over the first texture must not be moved below it.
        l.rect(Rect::new(5.0, 5.0, 40.0, 40.0), 8.0, [1.0; 4]);
        assert_eq!(batches(&l.quads, &mut v).len(), 5);
        assert_eq!(v.len(), l.quads.len() * 4);
    }

    #[test]
    fn damage_covers_only_the_change() {
        let mut a = DrawList::default();
        a.rect(Rect::new(0.0, 0.0, 10.0, 10.0), 0.0, [1.0; 4]);
        a.rect(Rect::new(100.0, 100.0, 10.0, 10.0), 0.0, [1.0; 4]);
        let mut b = DrawList::default();
        b.rect(Rect::new(0.0, 0.0, 10.0, 10.0), 0.0, [1.0; 4]);
        b.rect(Rect::new(102.0, 100.0, 10.0, 10.0), 0.0, [1.0; 4]);
        assert_eq!(diff_damage(&a.quads, &b.quads), Some(Rect::new(100.0, 100.0, 12.0, 10.0)));
        assert_eq!(diff_damage(&a.quads, &a.quads), None);
    }
}
