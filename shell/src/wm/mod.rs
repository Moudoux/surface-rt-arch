//! Window policies (DESKTOP-SHELL.md 3.6, layer 2). Each implements `crate::policy::WindowPolicy`.
//! v1: `FullscreenPolicy` with split view (`split.rs`). Later: a `FloatingPolicy` for overlapping, resizable
//! windows.

mod fullscreen;
pub mod split;

pub use fullscreen::FullscreenPolicy;
pub use split::SplitMetrics;

use crate::policy::WindowPolicy;

/// Pick a policy by name (`--policy NAME`; only "fullscreen" exists so far).
pub fn by_name(name: &str, split: SplitMetrics) -> Option<Box<dyn WindowPolicy>> {
    match name {
        "fullscreen" => Some(Box::new(FullscreenPolicy::new(split))),
        _ => None,
    }
}
