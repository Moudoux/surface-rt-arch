//! Output + input backends (core layer). `udev`: DRM/KMS, GBM, EGL/GLES (grate), libinput, libseat, on the
//! Surface. `winit`: a window on the host's desktop for development (`--nested`).

#[cfg(feature = "udev")]
pub mod udev;
#[cfg(feature = "winit")]
pub mod winit;

use smithay::{backend::renderer::gles::GlesRenderer, input::keyboard::LedState, output::Output};

use crate::state::State;

pub enum Backend {
    /// Before the backend is initialised (State is created first so the backend can register globals).
    Uninit,
    #[cfg(feature = "udev")]
    Udev(udev::UdevData),
    #[cfg(feature = "winit")]
    Winit(winit::WinitData),
}

impl Backend {
    /// The real display (DRM/KMS); false for the nested window and before init.
    pub fn is_drm(&self) -> bool {
        #[cfg(feature = "udev")]
        if matches!(self, Backend::Udev(_)) {
            return true;
        }
        false
    }

    pub fn renderer(&mut self) -> Option<&mut GlesRenderer> {
        match self {
            Backend::Uninit => None,
            #[cfg(feature = "udev")]
            Backend::Udev(d) => Some(d.renderer()),
            #[cfg(feature = "winit")]
            Backend::Winit(d) => Some(d.renderer()),
        }
    }

    /// Renderer + output for an offscreen re-render (screenshots); None while the session is inactive.
    /// `[input]` changed at runtime (Settings).
    pub fn reconfigure_input(&mut self, _cfg: &crate::config::Input) {
        #[cfg(feature = "udev")]
        if let Backend::Udev(d) = self {
            d.reconfigure_input(_cfg);
        }
    }

    pub fn screenshot_target(&mut self) -> Option<(&mut GlesRenderer, Output)> {
        match self {
            Backend::Uninit => None,
            #[cfg(feature = "udev")]
            Backend::Udev(d) => d.screenshot_target(),
            #[cfg(feature = "winit")]
            Backend::Winit(d) => {
                let output = d.output().clone();
                Some((d.renderer(), output))
            }
        }
    }

    pub fn change_vt(&mut self, vt: i32) {
        match self {
            #[cfg(feature = "udev")]
            Backend::Udev(d) => d.change_vt(vt),
            _ => {
                let _ = vt;
            }
        }
    }

    pub fn update_led_state(&mut self, leds: LedState) {
        match self {
            #[cfg(feature = "udev")]
            Backend::Udev(d) => d.update_led_state(leds),
            _ => {
                let _ = leds;
            }
        }
    }
}

/// Turn the output off (idle blank) or back on (the next frame is a full redraw). Nested: nothing.
pub fn set_output_power(state: &mut State, on: bool) {
    match &state.backend {
        #[cfg(feature = "udev")]
        Backend::Udev(_) => udev::set_output_power(state, on),
        _ => tracing::info!(on, "output power (nested: not changed)"),
    }
}

/// Ask for a new frame. Frames are only drawn when something asked for one: idle = no wakeups.
pub fn queue_redraw(state: &mut State) {
    match &state.backend {
        Backend::Uninit => {}
        #[cfg(feature = "udev")]
        Backend::Udev(_) => udev::queue_redraw(state),
        #[cfg(feature = "winit")]
        Backend::Winit(_) => winit::queue_redraw(state),
    }
}
