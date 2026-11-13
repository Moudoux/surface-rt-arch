//! Turning the scene into render elements, and the per-frame bookkeeping after a render (frame callbacks,
//! presentation feedback, dmabuf feedback). Core layer: draws whatever windows the policy mapped, wherever it
//! put them, in stacking order; it does not assume full screen.

pub mod atlas;
pub mod batch;
pub mod blur;
pub mod cursor;
pub mod gl;
pub mod icons;
pub mod kit;
pub mod layer;
pub mod login_wall;
pub mod preload;
pub mod screenshot;
pub mod text;
pub mod wallpaper;

use std::time::Duration;

use smithay::{
    backend::renderer::{
        element::{
            AsRenderElements, Element, Id, Kind, RenderElement, UnderlyingStorage, RenderElementStates, default_primary_scanout_output_compare,
            memory::MemoryRenderBufferRenderElement, surface::WaylandSurfaceRenderElement,
            utils::{Relocate, RelocateRenderElement, RescaleRenderElement, select_dmabuf_feedback},
        },
        gles::{GlesError, GlesFrame, GlesRenderer},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    desktop::{
        Space, Window,
        utils::{
            OutputPresentationFeedback, send_frames_surface_tree, surface_presentation_feedback_flags_from_states,
            surface_primary_scanout_output, update_surface_primary_scanout_output, with_surfaces_surface_tree,
        },
    },
    input::pointer::CursorImageStatus,
    output::Output,
    utils::{Buffer, Logical, Monotonic, Physical, Point, Rectangle, Scale, Size, Time, Transform, user_data::UserDataMap},
    wayland::dmabuf::DmabufFeedback,
};

use cursor::CursorState;

use crate::{
    overlay::Overlay,
    overview::Overview,
    switch::{Backdrop, Pose, Style, Switch},
};
use smithay::backend::renderer::{
    Bind, Offscreen, Renderer,
    damage::OutputDamageTracker,
    element::{solid::SolidColorRenderElement, texture::TextureRenderElement},
    gles::GlesTexture,
};
use smithay::backend::allocator::Fourcc;

smithay::backend::renderer::element::render_elements! {
    pub OutputElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
    Ui=layer::UiElement,
    Scaled=ScaledElement,
    Solid=SolidColorRenderElement,
    Texture=TextureRenderElement<GlesTexture>,
}

type Moved = RelocateRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>;

/// A surface of a switching app, scaled and moved. `opaque` (element-local) is the app's window geometry,
/// claimed opaque while the app is drawn at full alpha although the client did not say so (Chromium's
/// ABGR8888 buffers never are), so Home under it is not drawn: one dropped frame less per switch on the
/// Surface. Cost: a maximized app with a see-through background would show stale pixels behind it during
/// the animation. `RTSHELL_SWITCH_OPAQUE=0` turns the claim off.
#[derive(Debug)]
pub struct ScaledElement {
    inner: Moved,
    opaque: Option<Rectangle<i32, Physical>>,
}

impl Element for ScaledElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        match self.opaque {
            Some(r) if self.inner.alpha() >= 1.0 => OpaqueRegions::from_slice(&[r]),
            _ => self.inner.opaque_regions(scale),
        }
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for ScaledElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        self.inner.draw(frame, src, dst, damage, opaque_regions, cache)
    }
    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        self.inner.underlying_storage(renderer)
    }
}

impl std::fmt::Debug for OutputElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Surface(e) => f.debug_tuple("Surface").field(e).finish(),
            Self::Memory(e) => f.debug_tuple("Memory").field(e).finish(),
            Self::Ui(e) => f.debug_tuple("Ui").field(e).finish(),
            Self::Scaled(e) => f.debug_tuple("Scaled").field(e).finish(),
            Self::Solid(e) => f.debug_tuple("Solid").field(e).finish(),
            Self::Texture(e) => f.debug_tuple("Texture").field(e).finish(),
            Self::_GenericCatcher(_) => f.write_str("_GenericCatcher"),
        }
    }
}

/// Per-output dmabuf feedback: one for composited surfaces, one for surfaces that could be scanned out.
#[derive(Debug, Clone)]
pub struct SurfaceDmabufFeedback {
    pub render: DmabufFeedback,
    pub scanout: DmabufFeedback,
}

/// Everything the shell UI draws with in one frame: the draw list being built and the render kit's
/// resources (text, glyph/shadow atlas, icons, the glass texture). The UI layer gets one of these per frame.
pub struct Painter<'a> {
    pub gl: &'a smithay::backend::renderer::gles::ffi::Gles2,
    pub list: &'a mut batch::DrawList,
    pub text: &'a mut text::Text,
    pub atlas: &'a mut text::SharedAtlas,
    pub icons: &'a mut icons::Icons,
    /// The blurred wallpaper, once ready.
    pub glass: Option<gl::Texture>,
    /// The output rectangle the glass texture covers.
    pub screen: batch::Rect,
}

/// All elements for one output, front to back: cursor, the app of a running switch (scaled), the shell UI
/// layer (if any), the previous app of an app -> app switch, then windows from the top of the stack down
/// (minus those the switch draws).
pub fn output_elements(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    cursor: &mut CursorState,
    pointer: Point<f64, Logical>,
    ui: Option<layer::UiElement>,
    top: Vec<layer::UiElement>,
    switch: Option<&Switch>,
    overview: Option<&Overview>,
    decorations: Vec<OutputElement>,
    locked: bool,
) -> Vec<OutputElement> {
    let Some(output_geo) = space.output_geometry(output) else {
        return Vec::new();
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let mut elements = Vec::new();

    if output_geo.to_f64().contains(pointer) {
        elements.extend(cursor.render_elements(renderer, pointer - output_geo.loc.to_f64(), scale));
    }
    // The top layers (on-screen keyboard, Control Center) are above everything else, topmost first.
    elements.extend(top.into_iter().map(OutputElement::Ui));
    // Locked: nothing else. No app, Home, switch or Overview pixels may reach the screen (lock.rs); without
    // a lock element the output shows only the clear colour.
    if locked {
        return elements;
    }

    // The space stores the window geometry origin; the buffer starts geometry.loc earlier (CSD shadows).
    let surfaces = |renderer: &mut GlesRenderer, window: &Window, loc: Point<i32, Logical>, alpha: f32| {
        let render_loc = (loc - window.geometry().loc - output_geo.loc).to_physical_precise_round(scale);
        AsRenderElements::<GlesRenderer>::render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
            window, renderer, render_loc, scale, alpha,
        )
    };

    if let Some(sw) = switch {
        push_scaled(&mut elements, renderer, &sw.windows, sw.area, sw.pose(), claim_opaque(), output_geo, scale);
    }
    if let Some(ov) = overview {
        // The dragged card on top, then the pinned card (split view, waiting for its partner; the row slides
        // under it), then the row.
        if let Some(i) = ov.dragged() {
            push_scaled(&mut elements, renderer, &ov.cards[i].windows, ov.area, ov.card_pose(i), claim_opaque(), output_geo, scale);
        }
        if let Some((card, pose)) = ov.pinned() {
            push_scaled(&mut elements, renderer, &card.windows, ov.area, pose, claim_opaque(), output_geo, scale);
        }
        for i in (0..ov.cards.len()).filter(|i| Some(*i) != ov.dragged()) {
            push_scaled(&mut elements, renderer, &ov.cards[i].windows, ov.area, ov.card_pose(i), claim_opaque(), output_geo, scale);
        }
        // The half the pinned card or a lifted card takes.
        if let Some(half) = ov.highlight() {
            let geo = Rectangle::new((half.loc - output_geo.loc).to_physical_precise_round(scale), half.size.to_physical_precise_round(scale));
            elements.push(OutputElement::Solid(SolidColorRenderElement::new(ov.drop_id.clone(), geo, CommitCounter::default(), ov.drop_color, Kind::Unspecified)));
        }
        let a = ov.open.value.clamp(0.0, 1.0);
        let full_texture = |id: &Id, tex: &GlesTexture, alpha: f32, renderer: &mut GlesRenderer| {
            let opaque = Some(vec![Rectangle::from_size(smithay::backend::renderer::Texture::size(tex))]);
            OutputElement::Texture(TextureRenderElement::from_static_texture(
                id.clone(),
                renderer.context_id(),
                (0.0, 0.0),
                tex.clone(),
                1,
                Transform::Normal,
                Some(alpha),
                None,
                None,
                opaque,
                Kind::Unspecified,
            ))
        };
        if let (Some(home), Some(bd)) = (ov.home.as_ref(), ov.backdrop.as_ref()) {
            // The frosted backdrop fades in over Home's snapshot; fully open it is opaque and Home's snapshot
            // under it is skipped (one textured quad per frame).
            if a > 0.004 {
                elements.push(full_texture(&ov.backdrop_id, bd, a, renderer));
            }
            if a < 0.999 {
                elements.push(full_texture(&ov.home_id, home, 1.0, renderer));
            }
        } else if let Some(tex) = ov.home.as_ref() {
            // No backdrop (yet): Home's snapshot fades out over the background (the clear colour).
            let alpha = 1.0 - a;
            if alpha > 0.004 {
                let opaque = Some(vec![Rectangle::from_size(smithay::backend::renderer::Texture::size(tex))]);
                elements.push(OutputElement::Texture(TextureRenderElement::from_static_texture(
                    ov.home_id.clone(),
                    renderer.context_id(),
                    (0.0, 0.0),
                    tex.clone(),
                    1,
                    Transform::Normal,
                    Some(alpha),
                    None,
                    None,
                    opaque,
                    Kind::Unspecified,
                )));
            }
        } else if a > 0.004 {
            // No snapshot: a scrim over the live Home, opaque once fully open (the damage tracker then
            // skips Home under it).
            let geo = Rectangle::from_size(output_geo.size.to_physical_precise_round(scale));
            let commit = CommitCounter::from((a * 1000.0) as usize);
            let c = ov.scrim.map(|v| v * a);
            elements.push(OutputElement::Solid(SolidColorRenderElement::new(ov.scrim_id.clone(), geo, commit, c, Kind::Unspecified)));
        }
    }
    if let Some(ui) = ui {
        elements.push(OutputElement::Ui(ui));
    }
    if let Some(sw) = switch {
        for (window, loc) in sw.under.iter().rev() {
            elements.extend(surfaces(renderer, window, *loc, 1.0).into_iter().map(OutputElement::Surface));
        }
    }

    // The policy's decorations (split divider) above the windows; the caller leaves them out while a switch
    // or the Overview runs.
    elements.extend(decorations);

    // Windows the switch draws itself (an app sliding out over the one it came back to is in both).
    let moving = |w: &Window| switch.is_some_and(|s| s.windows.iter().chain(&s.under).any(|(x, _)| x == w));
    for window in space.elements_for_output(output).rev() {
        if moving(window) {
            continue;
        }
        let Some(loc) = space.element_location(window) else { continue };
        elements.extend(surfaces(renderer, window, loc, 1.0).into_iter().map(OutputElement::Surface));
    }
    elements
}

/// The window policy's decorations as solid quads, front to back, looks from the `[split]` tokens: the
/// divider with its grip, the snap mark, the area uncovered while dragging, the dismiss cover.
pub fn decoration_elements(
    decos: &[crate::policy::Decoration],
    t: &crate::theme::SplitTokens,
    ids: &mut std::collections::HashMap<(crate::policy::DecoRole, bool), Id>,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Vec<OutputElement> {
    use crate::policy::DecoRole;
    let mut out = Vec::new();
    let mut push = |role: DecoRole, grip: bool, rect: Rectangle<i32, Logical>, color: [f32; 4]| {
        if rect.size.w <= 0 || rect.size.h <= 0 || color[3] <= 0.0 {
            return;
        }
        let id = ids.entry((role, grip)).or_insert_with(Id::new).clone();
        let geo = Rectangle::new((rect.loc - output_geo.loc).to_physical_precise_round(scale), rect.size.to_physical_precise_round(scale));
        out.push(OutputElement::Solid(SolidColorRenderElement::new(id, geo, CommitCounter::default(), color, Kind::Unspecified)));
    };
    // A centred strip of `thick` across the bar's short side and `len` along its long side (whole if None).
    let centred = |r: Rectangle<i32, Logical>, thick: i32, len: Option<i32>| {
        if r.size.h >= r.size.w {
            let l = len.unwrap_or(r.size.h).min(r.size.h);
            Rectangle::new((r.loc.x + (r.size.w - thick) / 2, r.loc.y + (r.size.h - l) / 2).into(), (thick, l).into())
        } else {
            let l = len.unwrap_or(r.size.w).min(r.size.w);
            Rectangle::new((r.loc.x + (r.size.w - l) / 2, r.loc.y + (r.size.h - thick) / 2).into(), (l, thick).into())
        }
    };
    // Front to back: the input is bottom to top.
    for d in decos.iter().rev() {
        match d.role {
            DecoRole::Divider | DecoRole::DividerActive => {
                let active = d.role == DecoRole::DividerActive;
                let grip = centred(d.rect, t.handle_thickness as i32, Some(t.handle_length as i32));
                push(d.role, true, grip, if active { t.handle_active } else { t.handle_color }.premultiplied());
                push(d.role, false, d.rect, if active { t.divider_active } else { t.divider_color }.premultiplied());
            }
            DecoRole::SnapMark => push(d.role, false, centred(d.rect, t.snap_mark as i32, None), t.snap_color.premultiplied()),
            DecoRole::Reveal => push(d.role, false, d.rect, t.reveal_color.premultiplied()),
            DecoRole::Dismiss => push(d.role, false, d.rect, t.dismiss_color.premultiplied()),
        }
    }
    out
}

/// Draw an app family scaled by `pose.scale` about the centre of `area` and moved so that centre lands on
/// `pose.center`. `claim`: claim the main surface's window geometry opaque at full alpha (see `ScaledElement`).
#[allow(clippy::too_many_arguments)]
fn push_scaled(
    elements: &mut Vec<OutputElement>,
    renderer: &mut GlesRenderer,
    windows: &[(Window, Point<i32, Logical>)],
    area: Rectangle<i32, Logical>,
    pose: Pose,
    claim: bool,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) {
    if pose.alpha <= 0.004 {
        return;
    }
    let a = area.to_f64();
    let origin = Point::<f64, Logical>::from((a.loc.x + a.size.w / 2.0, a.loc.y + a.size.h / 2.0));
    let origin_px = (origin - output_geo.loc.to_f64()).to_physical_precise_round(scale);
    let center_px: Point<i32, Physical> = (pose.center - output_geo.loc.to_f64()).to_physical_precise_round(scale);
    for (window, loc) in windows.iter().rev() {
        // The toplevel's own surface: its window geometry (inside CSD shadows), element-local.
        let main = crate::winconf::surface(window).map(|s| Id::from_wayland_resource(&s));
        let g = window.geometry();
        let render_loc = (*loc - g.loc - output_geo.loc).to_physical_precise_round(scale);
        let surfaces = AsRenderElements::<GlesRenderer>::render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
            window, renderer, render_loc, scale, pose.alpha,
        );
        for e in surfaces {
            let opaque = (claim && Some(e.id()) == main.as_ref()).then(|| {
                let s = scale.x * pose.scale;
                let (x0, y0) = ((g.loc.x as f64 * s).ceil() as i32, (g.loc.y as f64 * s).ceil() as i32);
                let x1 = ((g.loc.x + g.size.w) as f64 * s).floor() as i32;
                let y1 = ((g.loc.y + g.size.h) as f64 * s).floor() as i32;
                // One pixel in from each edge: rounding of the scaled geometry never claims too much.
                Rectangle::new((x0 + 1, y0 + 1).into(), ((x1 - x0 - 2).max(0), (y1 - y0 - 2).max(0)).into())
            });
            let e = RescaleRenderElement::from_element(e, origin_px, pose.scale);
            let inner = RelocateRenderElement::from_element(e, center_px - origin_px, Relocate::Relative);
            elements.push(OutputElement::Scaled(ScaledElement { inner, opaque }));
        }
    }
}

fn claim_opaque() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RTSHELL_SWITCH_OPAQUE").is_none_or(|v| v != "0"))
}

/// Before building a frame's elements: step the switch to the frame's presentation time, tell the overlay
/// how far it has revealed it, build the overlay's element, then aim the switch at the app's tile as the
/// overlay has just laid it out. Takes fields, not `State`, because the backend holds the renderer.
/// Returns Home's element and the top layers' (drawn above everything; see `Overlay::top_elements`).
pub fn prepare_ui(
    switch: &mut Option<Switch>,
    overview: &mut Option<Overview>,
    overlay: &mut Option<Box<dyn Overlay>>,
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    present_at: Duration,
    locked: bool,
) -> (Option<layer::UiElement>, Vec<layer::UiElement>) {
    let home = if locked {
        // Home is not drawn; a switch or the Overview still runs to its end underneath.
        if let Some(sw) = switch.as_mut() {
            sw.step(present_at);
        }
        if let Some(ov) = overview.as_mut() {
            ov.step(present_at);
        }
        None
    } else {
        prepare_home(switch, overview, overlay, renderer, size, present_at)
    };
    let top = overlay.as_mut().map(|o| o.top_elements(renderer, size, present_at)).unwrap_or_default();
    (home, top)
}

fn prepare_home(
    switch: &mut Option<Switch>,
    overview: &mut Option<Overview>,
    overlay: &mut Option<Box<dyn Overlay>>,
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    present_at: Duration,
) -> Option<layer::UiElement> {
    if let Some(sw) = switch.as_mut() {
        sw.step(present_at);
    }
    if let Some(ov) = overview.as_mut() {
        ov.step(present_at);
    }
    let o = overlay.as_mut()?;
    // Home sits back under a switch's app as it grows. Under the Overview it stays still: a changing
    // reveal repaints Home's whole layer every frame, which is what made the Overview miss frames.
    let reveal = match (switch.as_ref(), overview.as_ref()) {
        (_, Some(_)) => 1.0,
        (Some(sw), _) if sw.backdrop == Backdrop::Home => sw.p.value.clamp(0.0, 1.0),
        _ => 1.0,
    };
    o.set_reveal(reveal);
    if overview.is_none() && switch.is_none() && SNAPSHOT.with(|c| c.borrow().tex.is_none()) {
        // Allocate the Overview's Home snapshot texture early (the first frames after startup are slow
        // anyway), so opening the Overview does not pay for it.
        if let Err(err) = snapshot_texture(renderer, size) {
            tracing::warn!(%err, "overview snapshot texture");
        }
    }
    let visible = o.visible();
    SNAPSHOT.with(|c| {
        let mut c = c.borrow_mut();
        if c.was_visible && !visible {
            c.redo_hidden = true;
        }
        c.was_visible = visible;
    });
    if !visible && overview.is_none() && switch.is_none() && SNAPSHOT.with(|c| c.borrow().redo_hidden) {
        // Home just went into hiding behind an app and reset itself: render it once as it will look when it
        // comes back (shown just for this, then hidden again), so the Overview never fades out a stale
        // splash or sheet. This frame shows an app, so the ~10 ms fit.
        SNAPSHOT.with(|c| c.borrow_mut().redo_hidden = false);
        o.set_apps_shown(false);
        if let Some(ui) = o.element(renderer, size, present_at) {
            let commit = smithay::backend::renderer::element::Element::current_commit(&ui);
            match snapshot(renderer, size, ui) {
                Ok(_) => SNAPSHOT.with(|c| c.borrow_mut().commit = Some(commit)),
                Err(err) => tracing::warn!(%err, "home snapshot"),
            }
        }
        if !SNAPSHOT.with(|c| c.borrow().backdrop_fresh(o.backdrop_serial(), size)) {
            make_backdrop(o, renderer, size, present_at);
        }
        o.set_apps_shown(true);
        SNAPSHOT.with(|c| c.borrow_mut().was_visible = false);
    }
    if let Some(ov) = overview.as_mut() {
        if ov.home.is_some() {
            // The snapshot stands in for Home until the Overview is gone.
            return None;
        }
        // The wallpaper or theme changed while Home was hidden (Settings over an app): redo the backdrop now.
        if o.visible() && !SNAPSHOT.with(|c| c.borrow().backdrop_fresh(o.backdrop_serial(), size)) {
            make_backdrop(o, renderer, size, present_at);
        }
        // Normally Home was snapshotted when it last settled (below): opening costs nothing extra. Its
        // clock may be a minute old, which nobody sees during a 0.3 s fade.
        if let Some(tex) = SNAPSHOT.with(|c| c.borrow().valid_for(size)) {
            ov.home = Some(tex);
            ov.backdrop = SNAPSHOT.with(|c| c.borrow().fresh_backdrop(o.backdrop_serial(), size));
            return None;
        }
        if !ov.home_failed && o.visible() {
            let ui = o.element(renderer, size, present_at)?;
            let commit = smithay::backend::renderer::element::Element::current_commit(&ui);
            match snapshot(renderer, size, ui) {
                Ok(tex) => {
                    SNAPSHOT.with(|c| c.borrow_mut().commit = Some(commit));
                    ov.home = Some(tex);
                    ov.backdrop = SNAPSHOT.with(|c| c.borrow().fresh_backdrop(o.backdrop_serial(), size));
                    return None;
                }
                Err(err) => {
                    tracing::warn!(%err, "overview: no Home snapshot, using a scrim");
                    ov.home_failed = true;
                }
            }
        }
    }
    let ui = if o.visible() { o.element(renderer, size, present_at) } else { None };
    if let Some(sw) = switch.as_mut().filter(|s| s.backdrop == Backdrop::Home && s.style != Style::Card) {
        sw.tile = sw.app_id.as_deref().and_then(|id| o.app_rect(id)).or(sw.tile_hint);
    }
    // Home has settled (nothing moves, no switch): refresh the Overview's snapshot if Home changed since.
    // Settled frames are small damage-only frames, so the ~7-13 ms of the snapshot fit in them.
    if ui.is_some() && switch.is_none() && !o.animating() && !SNAPSHOT.with(|c| c.borrow().backdrop_fresh(o.backdrop_serial(), size)) {
        // Home is idle: a good moment for the one-time backdrop (Home's next frame is a full redraw).
        make_backdrop(o, renderer, size, present_at);
    }
    if let Some(ui) = ui.as_ref().filter(|_| switch.is_none() && !o.animating()) {
        let commit = smithay::backend::renderer::element::Element::current_commit(ui);
        if SNAPSHOT.with(|c| c.borrow().commit) != Some(commit) {
            match snapshot(renderer, size, ui.clone()) {
                Ok(_) => SNAPSHOT.with(|c| c.borrow_mut().commit = Some(commit)),
                Err(err) => tracing::warn!(%err, "home snapshot"),
            }
        }
    }
    ui
}

/// Home rendered into a texture: the Overview's stand-in for Home. Kept between Overviews (allocating 4 MB
/// on the Tegra takes ~22 ms) and refreshed whenever Home settles with new content (`commit` = the UI
/// layer's commit it shows; None = no content yet).
#[derive(Default)]
struct Snapshot {
    tex: Option<GlesTexture>,
    commit: Option<CommitCounter>,
    /// Home was visible last frame: when it goes into hiding (an app came up), Home resets its transient
    /// state (launch splash, All apps, edit mode) and the snapshot is redone once from that state.
    was_visible: bool,
    redo_hidden: bool,
    /// The Overview's backdrop (frosted wallpaper + scrim), rendered once (it changes only with the
    /// wallpaper or theme).
    backdrop: Option<GlesTexture>,
    /// The overlay's `backdrop_serial` and the output size `backdrop` was rendered for.
    backdrop_for: (u64, (i32, i32)),
}

impl Snapshot {
    /// The backdrop exists and still matches the wallpaper/theme and output size.
    fn fresh_backdrop(&self, serial: u64, size: Size<i32, Physical>) -> Option<GlesTexture> {
        self.backdrop.clone().filter(|_| self.backdrop_fresh(serial, size))
    }
    fn backdrop_fresh(&self, serial: u64, size: Size<i32, Physical>) -> bool {
        self.backdrop.is_some() && self.backdrop_for == (serial, (size.w, size.h))
    }
    fn valid_for(&self, size: Size<i32, Physical>) -> Option<GlesTexture> {
        let t = self.tex.as_ref().filter(|_| self.commit.is_some())?;
        let s = smithay::backend::renderer::Texture::size(t);
        ((s.w, s.h) == (size.w, size.h)).then(|| t.clone())
    }
}

thread_local! {
    static SNAPSHOT: std::cell::RefCell<Snapshot> = std::cell::RefCell::new(Snapshot::default());
}

/// The snapshot texture for this output size: the kept one, or a new one (~22 ms on the Tegra).
fn snapshot_texture(renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> Result<GlesTexture, String> {
    let cached = SNAPSHOT.with(|c| c.borrow().tex.clone()).filter(|t| {
        let s = smithay::backend::renderer::Texture::size(t);
        (s.w, s.h) == (size.w, size.h)
    });
    if let Some(t) = cached {
        return Ok(t);
    }
    // ARGB8888 is grate's native render target format.
    let t = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Argb8888, (size.w, size.h).into())
        .map_err(|e| format!("create_buffer: {e}"))?;
    SNAPSHOT.with(|c| {
        let mut c = c.borrow_mut();
        c.tex = Some(t.clone());
        c.commit = None;
    });
    Ok(t)
}

fn snapshot(renderer: &mut GlesRenderer, size: Size<i32, Physical>, ui: layer::UiElement) -> Result<GlesTexture, String> {
    let t0 = std::time::Instant::now();
    let tex = snapshot_texture(renderer, size)?;
    let t1 = t0.elapsed();
    render_into(renderer, size, ui, tex.clone())?;
    tracing::debug!(
        alloc_ms = format!("{:.1}", t1.as_secs_f32() * 1000.0),
        total_ms = format!("{:.1}", t0.elapsed().as_secs_f32() * 1000.0),
        "home snapshot"
    );
    Ok(tex)
}

/// Render the overlay's backdrop (Home showing only its frosted wallpaper + scrim) into its own texture.
/// False if the overlay cannot draw it yet.
fn make_backdrop(o: &mut Box<dyn Overlay>, renderer: &mut GlesRenderer, size: Size<i32, Physical>, present_at: Duration) -> bool {
    if !o.show_backdrop(true) {
        return false;
    }
    let ui = o.element(renderer, size, present_at);
    o.show_backdrop(false);
    let Some(ui) = ui else { return false };
    let serial = o.backdrop_serial();
    let kept = SNAPSHOT.with(|c| c.borrow().backdrop.clone()).filter(|t| {
        let s = smithay::backend::renderer::Texture::size(t);
        (s.w, s.h) == (size.w, size.h)
    });
    let tex = match kept {
        Some(t) => Ok(t),
        None => Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Argb8888, (size.w, size.h).into())
            .map_err(|e| format!("create_buffer: {e}")),
    };
    match tex.and_then(|t| render_into(renderer, size, ui, t.clone()).map(|_| t)) {
        Ok(t) => {
            tracing::info!("overview backdrop ready");
            SNAPSHOT.with(|c| {
                let mut c = c.borrow_mut();
                c.backdrop = Some(t);
                c.backdrop_for = (serial, (size.w, size.h));
            });
            true
        }
        Err(err) => {
            tracing::warn!(%err, "overview backdrop");
            false
        }
    }
}

fn render_into(renderer: &mut GlesRenderer, size: Size<i32, Physical>, ui: layer::UiElement, mut tex: GlesTexture) -> Result<(), String> {
    {
        let mut fb = renderer.bind(&mut tex).map_err(|e| format!("bind: {e}"))?;
        let mut tracker = OutputDamageTracker::new(size, 1.0, Transform::Normal);
        tracker
            .render_output(renderer, &mut fb, 0, &[OutputElement::Ui(ui)], [0.0, 0.0, 0.0, 1.0])
            .map_err(|e| format!("render: {e:?}"))?;
    }
    Ok(())
}

/// After a render: remember which output each surface was last shown on (drives frame callback throttling
/// and fractional scale), for windows and a client cursor surface.
pub fn update_primary_scanout_output(
    space: &Space<Window>,
    output: &Output,
    cursor: &CursorState,
    states: &RenderElementStates,
) {
    for window in space.elements() {
        window.with_surfaces(|surface, data| {
            update_surface_primary_scanout_output(
                surface,
                output,
                data,
                None,
                states,
                default_primary_scanout_output_compare,
            );
        });
    }
    if let CursorImageStatus::Surface(s) = &cursor.status {
        with_surfaces_surface_tree(s, |surface, data| {
            update_surface_primary_scanout_output(
                surface,
                output,
                data,
                None,
                states,
                default_primary_scanout_output_compare,
            );
        });
    }
}

/// Frame callbacks for everything visible on `output`. Surfaces that were not on screen in the last frame
/// get at most one callback per second (Smithay's throttle), which keeps hidden clients from spinning.
/// An app growing in gets none until it is in place: it shows its last frame meanwhile, and its next
/// frame's GPU work does not compete with the animation.
pub fn send_frame_callbacks(
    space: &Space<Window>,
    output: &Output,
    cursor: &CursorState,
    now: Time<Monotonic>,
    switch: Option<&Switch>,
) {
    let throttle = Some(Duration::from_secs(1));
    let held = |w: &Window| switch.is_some_and(|s| s.entering() && s.windows.iter().any(|(x, _)| x == w));
    for window in space.elements() {
        if held(window) {
            continue;
        }
        if space.outputs_for_element(window).contains(output) {
            window.send_frame(output, now, throttle, surface_primary_scanout_output);
        }
    }
    if let CursorImageStatus::Surface(s) = &cursor.status {
        send_frames_surface_tree(s, output, now, throttle, surface_primary_scanout_output);
    }
}

pub fn send_dmabuf_feedback(
    space: &Space<Window>,
    output: &Output,
    feedback: &SurfaceDmabufFeedback,
    states: &RenderElementStates,
) {
    for window in space.elements() {
        if space.outputs_for_element(window).contains(output) {
            window.send_dmabuf_feedback(output, surface_primary_scanout_output, |surface, _| {
                select_dmabuf_feedback(surface, states, &feedback.render, &feedback.scanout)
            });
        }
    }
}

pub fn take_presentation_feedback(
    space: &Space<Window>,
    output: &Output,
    states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);
    for window in space.elements() {
        if space.outputs_for_element(window).contains(output) {
            window.take_presentation_feedback(&mut feedback, surface_primary_scanout_output, |surface, _| {
                surface_presentation_feedback_flags_from_states(surface, None, states)
            });
        }
    }
    feedback
}
