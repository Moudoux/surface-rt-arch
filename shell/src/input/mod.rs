//! Input events from any backend (libinput on the device, winit nested) -> seat, shortcuts, focus.
//! Core layer. Touches go to the edge gesture recognizer (gesture.rs, edges.rs) first, then the shell UI,
//! then apps.

pub mod bindings;
mod edges;
mod handles;
mod osk;
mod overview;
pub use edges::EdgeRun;
pub mod script;

use std::time::Duration;

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, InputTime, AxisSource, ButtonState, Device, DeviceCapability, Event, InputBackend,
        InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
        SwitchToggleEvent, TouchEvent,
    },
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
        touch::{DownEvent, MotionEvent as TouchMotionEvent, UpEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};
use smithay::input::keyboard::keysyms as k;
use smithay::backend::input::TouchSlot;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use tracing::info;

use crate::{
    gesture::{self, Verdict},
    overlay::{CANCEL_ALL, KeyEvent, MOUSE, Phase, PointerEvent, VolumeKey},
    state::State,
};
use bindings::Action;

impl State {
    pub fn process_input_event<B: InputBackend>(&mut self, event: InputEvent<B>) {
        match event {
            InputEvent::DeviceAdded { device } => {
                info!(name = %device.name(), usb_id = ?device.usb_id(), "input device added");
                if device.has_capability(DeviceCapability::Touch) && self.seat.get_touch().is_none() {
                    self.seat.add_touch();
                }
                let kb = device.has_capability(DeviceCapability::Keyboard);
                if crate::rotate::is_hw_keyboard(&device.name(), device.usb_id(), kb, device.has_capability(DeviceCapability::Touch)) {
                    self.hw_keyboards.push(device.id());
                    self.hw_keyboard_changed();
                }
            }
            InputEvent::DeviceRemoved { device } => {
                info!(name = %device.name(), "input device removed");
                let id = device.id();
                if let Some(i) = self.hw_keyboards.iter().position(|k| *k == id) {
                    self.hw_keyboards.remove(i);
                    self.hw_keyboard_changed();
                }
            }
            InputEvent::Keyboard { event } => self.on_key::<B>(event),
            InputEvent::PointerMotion { event } => {
                let pos = self.clamp_to_output(self.pointer.current_location() + event.delta());
                self.pointer_moved(pos, event.time());
            }
            InputEvent::PointerMotionAbsolute { event } => {
                if let Some(pos) = self.touch_position::<B, _>(&event) {
                    self.pointer_moved(pos, event.time());
                }
            }
            InputEvent::PointerButton { event } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.on_pointer_axis::<B>(event),
            InputEvent::TouchDown { event } => {
                let Some(pos) = self.touch_position::<B, _>(&event) else { return };
                self.touch_down(i32::from(event.slot()), pos, event.time());
            }
            InputEvent::TouchMotion { event } => {
                let Some(pos) = self.touch_position::<B, _>(&event) else { return };
                self.touch_motion(i32::from(event.slot()), pos, event.time());
            }
            InputEvent::TouchUp { event } => self.touch_up(i32::from(event.slot()), event.time()),
            InputEvent::TouchFrame { .. } => self.touch_frame(),
            InputEvent::TouchCancel { .. } => self.touch_cancel(),
            InputEvent::SwitchToggle { event } => {
                info!(switch = ?event.switch(), state = ?event.state(), "switch toggled");
                // Lid closed: lock at once (logind then sleeps by default; Phase 8 design). Opened: wake.
                if event.switch() == Some(smithay::backend::input::Switch::Lid) {
                    if event.state() == smithay::backend::input::SwitchState::On {
                        self.lock_session(crate::overlay::LockReason::Sleep);
                    } else {
                        self.idle_activity();
                    }
                }
            }
            _ => {}
        }
    }

    /// A finger came down (libinput, or the input script). Edge gestures first, then the shell UI, then apps.
    pub fn touch_down(&mut self, slot: i32, pos: Point<f64, Logical>, time: InputTime) {
        self.hide_cursor();
        if !self.idle_touch_down(slot) {
            return; // woke a blank screen: this touch is dropped until its finger lifts
        }
        self.kick_boost();
        if self.locked() {
            return self.lock_pointer(slot, Phase::Down, pos);
        }
        let now: Duration = self.clock.now().into();
        let size = self.output_geometry().map(|g| (g.size.w as f32, g.size.h as f32)).unwrap_or((1.0, 1.0));
        let armed = self.armed_edges();
        let verdict = self.gestures.down(slot, pos.x as f32, pos.y as f32, now, size, armed);
        if self.gestures.pending_since() == Some(now) {
            self.arm_gesture_hold();
        }
        if self.on_verdict(verdict, time) {
            return;
        }
        self.deliver_touch_down(slot, pos, time);
    }

    pub fn touch_motion(&mut self, slot: i32, pos: Point<f64, Logical>, time: InputTime) {
        if !self.idle_touch(slot, false) {
            return;
        }
        if self.locked() {
            return self.lock_pointer(slot, Phase::Motion, pos);
        }
        let now: Duration = self.clock.now().into();
        let verdict = self.gestures.motion(slot, pos.x as f32, pos.y as f32, now);
        if self.on_verdict(verdict, time) {
            return;
        }
        self.deliver_touch_motion(slot, pos, time);
    }

    pub fn touch_up(&mut self, slot: i32, time: InputTime) {
        if !self.idle_touch(slot, true) {
            return;
        }
        if self.locked() {
            return self.lock_pointer(slot, Phase::Up, Point::default());
        }
        let now: Duration = self.clock.now().into();
        let verdict = self.gestures.up(slot, now);
        if self.on_verdict(verdict, time) {
            return;
        }
        if self.handle_moved(slot, crate::policy::HandlePhase::Up, Point::default()) {
            return;
        }
        if self.panel_input(slot, Phase::Up, Point::default()) {
            return;
        }
        if self.overview_takes_touches() {
            return self.overview_touch_up(slot);
        }
        let Some(touch) = self.seat.get_touch() else { return };
        if self.overlay_input(slot, Phase::Up, Point::default()) {
            return;
        }
        self.touch_frame_due = true;
        touch.up(self, &UpEvent { slot: to_slot(slot), serial: SERIAL_COUNTER.next_serial(), time });
    }

    pub fn touch_frame(&mut self) {
        if std::mem::take(&mut self.touch_frame_due) && let Some(touch) = self.seat.get_touch() {
            touch.frame(self);
        }
    }

    pub fn touch_cancel(&mut self) {
        self.idle_touch_cancel();
        if self.locked() {
            return self.lock_pointer(CANCEL_ALL, Phase::Cancel, Point::default());
        }
        self.handle_cancel();
        if let Some(track) = self.gestures.cancel_all() {
            self.gesture_end(track, true);
        }
        self.panel_input(CANCEL_ALL, Phase::Cancel, Point::default());
        self.overlay_input(CANCEL_ALL, Phase::Cancel, Point::default());
        if let Some(touch) = self.seat.get_touch() {
            touch.cancel(self);
        }
    }

    /// Act on the recognizer's verdict. True if the event is used up (not delivered normally).
    fn on_verdict(&mut self, verdict: Verdict, time: InputTime) -> bool {
        match verdict {
            Verdict::Pass => false,
            Verdict::Swallow => true,
            Verdict::Release(held) => {
                // The held-back down reaches the shell UI or the app now; the current event follows normally.
                self.deliver_touch_down(held.slot, Point::from((held.x as f64, held.y as f64)), time);
                false
            }
            Verdict::Begin(track) => {
                if !self.gesture_begin(track) {
                    self.gestures.refuse();
                }
                true
            }
            Verdict::Move(track) => {
                self.gesture_move(track);
                true
            }
            Verdict::End(track) => {
                self.gesture_end(track, false);
                true
            }
            Verdict::Cancel(track) => {
                self.gesture_end(track, true);
                true
            }
        }
    }

    /// A pending edge touch that rests is handed on after `gesture::HOLD` (a long press at the edge).
    fn arm_gesture_hold(&mut self) {
        let res = self.loop_handle.insert_source(Timer::from_duration(gesture::HOLD), |_, _, state| {
            let now: Duration = state.clock.now().into();
            if let Some(held) = state.gestures.timeout(now) {
                state.deliver_touch_down(held.slot, Point::from((held.x as f64, held.y as f64)), InputTime::now());
                state.touch_frame();
            }
            TimeoutAction::Drop
        });
        if let Err(err) = res {
            tracing::warn!(%err, "cannot arm gesture timer");
        }
    }

    fn deliver_touch_down(&mut self, slot: i32, pos: Point<f64, Logical>, time: InputTime) {
        if self.panel_input(slot, Phase::Down, pos) {
            return;
        }
        if self.overview_takes_touches() {
            return self.overview_touch_down(slot, pos);
        }
        let Some(touch) = self.seat.get_touch() else { return };
        if self.overlay_input(slot, Phase::Down, pos) {
            return;
        }
        let serial = SERIAL_COUNTER.next_serial();
        let grabbed = touch.is_grabbed() || self.seat.get_keyboard().is_some_and(|k| k.is_grabbed());
        if !grabbed && self.handle_down(slot, pos) {
            return;
        }
        if !grabbed && let Some((window, _)) = self.space.element_under(pos).map(|(w, l)| (w.clone(), l)) {
            self.activate_window(&window);
        }
        let under = self.surface_under(pos);
        self.touch_frame_due = true;
        touch.down(self, under, &DownEvent { slot: to_slot(slot), location: pos, serial, time });
    }

    fn deliver_touch_motion(&mut self, slot: i32, pos: Point<f64, Logical>, time: InputTime) {
        if self.handle_moved(slot, crate::policy::HandlePhase::Motion, pos) {
            return;
        }
        if self.panel_input(slot, Phase::Motion, pos) {
            return;
        }
        if self.overview_takes_touches() {
            return self.overview_touch_motion(slot, pos);
        }
        let Some(touch) = self.seat.get_touch() else { return };
        if self.overlay_input(slot, Phase::Motion, pos) {
            return;
        }
        let under = self.surface_under(pos);
        self.touch_frame_due = true;
        touch.motion(self, under, &TouchMotionEvent { slot: to_slot(slot), location: pos, time });
    }

    fn on_key<B: InputBackend>(&mut self, event: B::KeyboardKeyEvent) {
        self.key_input(event.key_code(), event.state(), event.time());
    }

    /// One key event (xkb keycode = evdev + 8), from libinput or the test script's `rawkey`.
    pub fn key_input(&mut self, keycode: smithay::input::keyboard::Keycode, key_state: KeyState, time: smithay::backend::input::InputTime) {
        // A press that wakes a blank screen goes nowhere (its release neither, via `suppressed_keys`).
        let woke = key_state == KeyState::Pressed && self.idle_activity();
        // Another key while the power button is held: no sleep on its release; Volume-down makes it the
        // screenshot chord (below).
        let chord = key_state == KeyState::Pressed && self.power_held.is_some_and(|c| c != keycode);
        if key_state == KeyState::Pressed {
            self.kick_boost();
            if chord {
                self.power_held = None;
            }
        }
        let mut power_tap = false;
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.seat.get_keyboard().unwrap();
        let debug_keys = self.debug_keys;
        let mut home = false;
        let action = keyboard.input(self, keycode, key_state, serial, time, |state, mods, handle| {
            if debug_keys {
                // Opt-in (RTSHELL_DEBUG_KEYS=1). Only modifier keys and keys pressed with Ctrl/Alt/Super are
                // logged, never plain typing (no passwords in the log).
                let syms = handle.raw_syms();
                let is_modifier = syms.iter().any(|s| s.is_modifier_key());
                if is_modifier || mods.ctrl || mods.alt || mods.logo {
                    let names: Vec<String> = syms.iter().map(|s| format!("{s:?}")).collect();
                    info!(
                        code = keycode.raw(),
                        state = ?key_state,
                        syms = ?names,
                        ctrl = mods.ctrl,
                        alt = mods.alt,
                        shift = mods.shift,
                        logo = mods.logo,
                        "key"
                    );
                }
            }
            let is_super = handle.raw_syms().iter().any(|s| matches!(s.raw(), k::KEY_Super_L | k::KEY_Super_R));
            if woke {
                state.super_tap = false;
                state.suppressed_keys.push(keycode);
                return FilterResult::Intercept(None);
            }
            if key_state == KeyState::Pressed && state.locked() {
                // Locked: only volume keys and VT switching act; every other key goes to the lock screen.
                state.super_tap = false;
                state.suppressed_keys.push(keycode);
                if let Some(action) = bindings::lookup(mods, &handle).filter(|a| matches!(a, Action::Volume(_) | Action::VtSwitch(_) | Action::Power)) {
                    return FilterResult::Intercept(Some(action));
                }
                if !is_super {
                    let sym = handle.modified_sym();
                    let text = sym.key_char().filter(|c| !c.is_control()).map(String::from);
                    let ev = KeyEvent { sym: sym.raw(), text, ctrl: mods.ctrl, alt: mods.alt };
                    if let Some(o) = state.overlay.as_mut() {
                        o.lock_key(&ev);
                    }
                    state.handle_overlay_requests();
                    state.queue_redraw();
                }
                return FilterResult::Intercept(None);
            }
            if key_state == KeyState::Pressed {
                // A lone tap of the Super (Windows) key toggles Home; any other key in between cancels it.
                state.super_tap = is_super && !mods.ctrl && !mods.alt && !mods.shift;
                if let Some(action) = bindings::lookup(mods, &handle) {
                    state.suppressed_keys.push(keycode);
                    return FilterResult::Intercept(Some(action));
                }
                // The side panel (Control Center) sees keys first while it is open (Escape closes it).
                if !is_super && let Some(o) = state.overlay.as_mut().filter(|o| o.panel_active()) {
                    let sym = handle.modified_sym();
                    let text = sym.key_char().filter(|c| !c.is_control()).map(String::from);
                    let ev = KeyEvent { sym: sym.raw(), text, ctrl: mods.ctrl, alt: mods.alt };
                    if o.panel_key(&ev) {
                        state.suppressed_keys.push(keycode);
                        state.handle_overlay_requests();
                        state.queue_redraw();
                        return FilterResult::Intercept(None);
                    }
                }
                // Home (or another UI) takes typing while no app has keyboard focus.
                let unfocused = state.seat.get_keyboard().is_some_and(|kb| kb.current_focus().is_none());
                if unfocused && !is_super && !mods.logo && let Some(o) = state.visible_overlay() {
                    let sym = handle.modified_sym();
                    let text = sym.key_char().filter(|c| !c.is_control()).map(String::from);
                    let ev = KeyEvent { sym: sym.raw(), text, ctrl: mods.ctrl, alt: mods.alt };
                    if o.key(&ev) {
                        state.suppressed_keys.push(keycode);
                        state.handle_overlay_requests();
                        state.queue_redraw();
                        return FilterResult::Intercept(None);
                    }
                }
            } else if let Some(i) = state.suppressed_keys.iter().position(|k| *k == keycode) {
                if state.power_held == Some(keycode) {
                    state.power_held = None;
                    power_tap = true;
                }
                if state.volume_repeat.as_ref().is_some_and(|(c, _)| *c == Some(keycode)) {
                    state.volume_release();
                }
                state.suppressed_keys.remove(i);
                return FilterResult::Intercept(None);
            } else if is_super && std::mem::take(&mut state.super_tap) {
                // The release still goes to the app (it saw the press).
                home = true;
            }
            FilterResult::Forward
        });
        match action {
            // Power + Volume-down: a screenshot, and the volume stays (the key's release is swallowed as usual).
            Some(Some(Action::Volume(crate::overlay::VolumeKey::Down))) if chord => self.run_action(Action::Screenshot),
            // Volume keys repeat while held (the release ends it, above).
            Some(Some(Action::Volume(key))) => self.volume_press(key, Some(keycode)),
            // Acts on release (below), so a Power+Volume-down chord can claim it.
            Some(Some(Action::Power)) => self.power_held = Some(keycode),
            Some(Some(action)) => self.run_action(action),
            _ => {}
        }
        if home {
            self.run_action(Action::Home);
        }
        if power_tap {
            self.run_action(Action::Power);
        }
    }

    pub fn run_action(&mut self, action: Action) {
        info!(?action, "shortcut");
        if self.locked() && !matches!(action, Action::Volume(_) | Action::VtSwitch(_) | Action::Power) {
            return;
        }
        match action {
            Action::Lock => self.lock_session(crate::overlay::LockReason::User),
            Action::Screenshot => self.save_screenshot(),
            Action::Power => {
                // Sleep through the overlay's services (the lock screen is drawn first, Phase 8).
                if !self.overlay.as_mut().is_some_and(|o| o.power_key()) {
                    tracing::warn!("power button: no shell UI to sleep with");
                }
                self.handle_overlay_requests();
            }
            Action::Quit => {
                info!("quit requested");
                self.loop_signal.stop();
                self.loop_signal.wakeup();
            }
            Action::VtSwitch(vt) => self.backend.change_vt(vt),
            Action::Terminal => {
                let cmd = self.config.apps.terminal.clone();
                self.spawn(&cmd);
            }
            Action::Launch(i) => {
                if let Some(cmd) = self.config.apps.launch.get(i).cloned() {
                    self.spawn(&cmd);
                }
            }
            Action::CloseFocused => {
                if let Some(w) = self.focused_window() {
                    crate::winconf::close(&w);
                }
            }
            Action::Cycle { forward } => self.cycle(forward),
            Action::Overview => self.toggle_overview(),
            Action::Home => {
                // A full-screen sheet (Settings) closes first, back to what was under it.
                if self.overlay.as_mut().is_some_and(|o| o.close_sheet()) {
                    self.queue_redraw();
                    return;
                }
                self.toggle_home();
            }
            Action::Settings => {
                if self.overlay.as_mut().is_some_and(|o| o.open_settings()) {
                    self.queue_redraw();
                }
            }
            Action::Snap(side) => {
                let Some(w) = self.focused_window() else { return };
                self.with_policy(|p, ctx| p.snap(ctx, &w, side, None));
            }
            Action::Volume(key) => self.volume_step(key),
            Action::ControlCenter => {
                if self.overlay.as_mut().is_some_and(|o| o.toggle_panel()) {
                    self.queue_redraw();
                }
            }
        }
    }

    /// A volume key went down: one step now, then repeats while it is held (`[keyboard] repeat_delay`,
    /// at most 10 per second: a volume sweep should stay controllable). Mute does not repeat.
    pub fn volume_press(&mut self, key: VolumeKey, code: Option<smithay::input::keyboard::Keycode>) {
        self.volume_release();
        self.volume_step(key);
        if key == VolumeKey::Mute {
            return;
        }
        let kb = &self.config.keyboard;
        let delay = Duration::from_millis(kb.repeat_delay.max(100) as u64);
        let every = Duration::from_millis((1000 / kb.repeat_rate.clamp(1, 100)).max(100) as u64);
        self.volume_presses += 1;
        let press = self.volume_presses;
        // Not removed on release (calloop then warns about the queued expiry): it ends itself.
        let res = self.loop_handle.insert_source(Timer::from_duration(delay), move |_, _, state| {
            if state.volume_repeat.is_none_or(|(_, p)| p != press) {
                return TimeoutAction::Drop;
            }
            state.volume_step(key);
            TimeoutAction::ToDuration(every)
        });
        match res {
            Ok(_) => self.volume_repeat = Some((code, press)),
            Err(err) => tracing::warn!(%err, "cannot arm volume repeat"),
        }
    }

    /// The held volume key came up.
    pub fn volume_release(&mut self) {
        self.volume_repeat = None;
    }

    fn volume_step(&mut self, key: VolumeKey) {
        if self.overlay.as_mut().is_some_and(|o| o.volume_key(key)) {
            self.queue_redraw();
        } else {
            tracing::debug!(?key, "volume key: no volume control");
        }
    }

    pub fn output_geometry(&self) -> Option<smithay::utils::Rectangle<i32, Logical>> {
        self.output().and_then(|o| self.space.output_geometry(&o))
    }

    fn clamp_to_output(&self, pos: Point<f64, Logical>) -> Point<f64, Logical> {
        let Some(geo) = self.output_geometry() else { return pos };
        let max_x = (geo.loc.x + geo.size.w - 1) as f64;
        let max_y = (geo.loc.y + geo.size.h - 1) as f64;
        (pos.x.clamp(geo.loc.x as f64, max_x), pos.y.clamp(geo.loc.y as f64, max_y)).into()
    }

    /// An absolute position (touch screen) in the output's logical space: the panel reports framebuffer
    /// positions, mapped back through the output's rotation (rotate.rs).
    fn touch_position<B: InputBackend, E: AbsolutePositionEvent<B>>(&self, event: &E) -> Option<Point<f64, Logical>> {
        let output = self.output()?;
        let geo = self.space.output_geometry(&output)?;
        let transform = output.current_transform();
        if transform == smithay::utils::Transform::Normal || !self.backend.is_drm() {
            return Some(event.position_transformed(geo.size) + geo.loc.to_f64());
        }
        let mode = output.current_mode()?.size;
        let fb = event.position_transformed((mode.w, mode.h).into());
        Some(crate::rotate::touch_point(transform, mode, (fb.x, fb.y).into()) + geo.loc.to_f64())
    }

    /// The Type Cover (or another keyboard) came or went: tell the UI and re-check the rotation.
    fn hw_keyboard_changed(&mut self) {
        let attached = !self.hw_keyboards.is_empty();
        info!(attached, "hardware keyboard");
        if let Some(o) = self.overlay.as_mut() {
            o.set_hw_keyboard(attached);
        }
        self.handle_overlay_requests();
        self.apply_rotation();
    }

    fn show_cursor(&mut self) {
        if !self.cursor.visible {
            self.cursor.visible = true;
            self.queue_redraw();
        }
    }

    fn hide_cursor(&mut self) {
        if self.cursor.visible {
            self.cursor.visible = false;
            self.queue_redraw();
        }
    }

    /// Offer a touch/mouse event to the side panel (Control Center) while it is open. True if it took it.
    /// A touch or click while locked: all of it goes to the lock screen.
    fn lock_pointer(&mut self, id: i32, phase: Phase, pos: Point<f64, Logical>) {
        let time = self.clock.now().into();
        if let Some(o) = self.overlay.as_mut() {
            o.lock_input(PointerEvent { id, phase, x: pos.x as f32, y: pos.y as f32, time });
        }
        self.handle_overlay_requests();
        self.queue_redraw();
    }

    pub fn panel_input(&mut self, id: i32, phase: Phase, pos: Point<f64, Logical>) -> bool {
        let time = self.clock.now().into();
        let Some(o) = self.overlay.as_mut() else { return false };
        // A cancel goes to the panel even when it is closing (it may still hold a touch).
        if phase != Phase::Cancel && !o.panel_active() && !o.panel_wants(id, pos.x as f32, pos.y as f32) {
            return false;
        }
        let used = o.panel_input(PointerEvent { id, phase, x: pos.x as f32, y: pos.y as f32, time });
        if used {
            self.handle_overlay_requests();
            self.queue_redraw();
        }
        used
    }

    /// Offer a touch/mouse event to the shell UI first. True if it took it (then clients do not see it).
    pub fn overlay_input(&mut self, id: i32, phase: Phase, pos: Point<f64, Logical>) -> bool {
        // An app growing in is not where it will be yet: drop new touches and clicks until it is. Motion
        // and releases still reach whoever got the press.
        if phase == Phase::Down && self.switch_blocks_input() {
            return true;
        }
        let time = self.clock.now().into();
        let Some(o) = self.visible_overlay() else { return false };
        let used = o.input(PointerEvent { id, phase, x: pos.x as f32, y: pos.y as f32, time });
        if used {
            self.handle_overlay_requests();
            self.queue_redraw();
        }
        used
    }

    fn pointer_moved(&mut self, pos: Point<f64, Logical>, time: InputTime) {
        self.idle_activity();
        self.show_cursor();
        if self.locked() {
            // The cursor moves (it is drawn over the lock); no client sees the pointer.
            let pointer = self.pointer.clone();
            pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            self.lock_pointer(MOUSE, Phase::Motion, pos);
            return;
        }
        let to_ui = if self.handle_moved(MOUSE, crate::policy::HandlePhase::Motion, pos) || self.panel_input(MOUSE, Phase::Motion, pos) {
            true
        } else if self.overview_takes_touches() {
            // The Overview takes the pointer like a finger (clients and Home see no motion).
            self.overview_touch_motion(MOUSE, pos);
            true
        } else {
            self.overlay_input(MOUSE, Phase::Motion, pos)
        };
        let under = if to_ui { None } else { self.surface_under(pos) };
        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
        // The cursor is a GL element: moving it means a (small, damage-only) redraw.
        self.queue_redraw();
    }

    fn on_pointer_button<B: InputBackend>(&mut self, event: B::PointerButtonEvent) {
        self.show_cursor();
        if !self.idle_button(event.button_code(), event.state() == ButtonState::Pressed) {
            return;
        }
        self.kick_boost();
        const BTN_LEFT: u32 = 0x110;
        if self.locked() {
            if event.button_code() == BTN_LEFT {
                let phase = if event.state() == ButtonState::Pressed { Phase::Down } else { Phase::Up };
                let pos = self.pointer.current_location();
                self.lock_pointer(MOUSE, phase, pos);
            }
            return;
        }
        if event.button_code() == BTN_LEFT {
            let phase = if event.state() == ButtonState::Pressed { Phase::Down } else { Phase::Up };
            let pos = self.pointer.current_location();
            if self.panel_input(MOUSE, phase, pos) {
                return;
            }
        }
        if self.overview_takes_touches() {
            // The Overview takes the left button like a finger; other buttons do nothing there.
            let pos = self.pointer.current_location();
            if event.button_code() == BTN_LEFT {
                if event.state() == ButtonState::Pressed {
                    self.overview_touch_down(MOUSE, pos);
                } else {
                    self.overview_touch_up(MOUSE);
                }
            }
            return;
        }
        if event.button_code() == BTN_LEFT {
            let phase = if event.state() == ButtonState::Pressed { Phase::Down } else { Phase::Up };
            let pos = self.pointer.current_location();
            if self.overlay_input(MOUSE, phase, pos) {
                return;
            }
            let held = match phase {
                Phase::Down => !self.pointer.is_grabbed() && self.handle_down(MOUSE, pos),
                _ => self.handle_moved(MOUSE, crate::policy::HandlePhase::Up, pos),
            };
            if held {
                return;
            }
        }
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.pointer.clone();
        let kb_grabbed = self.seat.get_keyboard().is_some_and(|k| k.is_grabbed());
        if event.state() == ButtonState::Pressed && !pointer.is_grabbed() && !kb_grabbed {
            let pos = pointer.current_location();
            if let Some((window, _)) = self.space.element_under(pos).map(|(w, l)| (w.clone(), l)) {
                self.activate_window(&window);
            }
        }
        pointer.button(
            self,
            &ButtonEvent {
                button: event.button_code(),
                state: event.state(),
                serial,
                time: event.time(),
            },
        );
        pointer.frame(self);
    }

    fn on_pointer_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
        self.show_cursor();
        if self.idle_activity() {
            return;
        }
        let source = event.source();
        let amount = |axis| {
            event
                .amount(axis)
                .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0)
        };
        let mut frame = AxisFrame::new(event.time()).source(source);
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let value = amount(axis);
            if value != 0.0 {
                frame = frame.value(axis, value);
                if let Some(v120) = event.amount_v120(axis) {
                    frame = frame.v120(axis, v120 as i32);
                }
            }
            if source == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                frame = frame.stop(axis);
            }
        }
        let pos = self.pointer.current_location();
        let (dx, dy) = (amount(Axis::Horizontal) as f32, amount(Axis::Vertical) as f32);
        if self.locked() {
            if let Some(o) = self.overlay.as_mut() {
                o.lock_axis(dx, dy);
            }
            self.queue_redraw();
            return;
        }
        if let Some(o) = self.visible_overlay()
            && o.axis(pos.x as f32, pos.y as f32, dx, dy)
        {
            self.queue_redraw();
            return;
        }
        let pointer = self.pointer.clone();
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}

/// Back to Smithay's slot type (`i32::from(TouchSlot)` maps "no slot" to -1).
fn to_slot(slot: i32) -> TouchSlot {
    TouchSlot::from((slot >= 0).then_some(slot as u32))
}
