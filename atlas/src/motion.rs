//! The motion values of Atlas, in one place. Durations follow the shared
//! guideline (120 ms hover, 200 ms state, 320 ms layout); the springs are the
//! ones Islands uses, so both feel like one family. See docs/MOTION.md.

pub const STATE_MS: u32 = 200;
pub const LAYOUT_MS: u32 = 320;

/// The one ease-out curve.
pub const EASE: adw::Easing = adw::Easing::EaseOutCubic;

/// Damping ratio and stiffness, mass 1.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub damping: f64,
    pub stiffness: f64,
}

pub const GROW: Spring = Spring { damping: 0.58, stiffness: 340.0 };
pub const SHRINK: Spring = Spring { damping: 0.75, stiffness: 420.0 };
pub const HOVER: Spring = Spring { damping: 0.7, stiffness: 520.0 };
/// Camera moves (fly to a table, settle after a fling): no overshoot, a map
/// that wobbles past its target reads as a mistake.
pub const CAMERA: Spring = Spring { damping: 1.0, stiffness: 260.0 };

/// The data panel growing out of a card: a little softer than GROW, so a
/// rectangle that large does not visibly overshoot.
pub const PANEL: Spring = Spring { damping: 0.86, stiffness: 300.0 };

impl Spring {
    pub fn params(self) -> adw::SpringParams {
        adw::SpringParams::new(self.damping, 1.0, self.stiffness)
    }
}
