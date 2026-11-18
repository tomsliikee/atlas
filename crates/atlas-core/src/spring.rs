//! Springs and momentum for the canvas, driven by the frame clock.
//!
//! libadwaita's `SpringAnimation` starts over when its target moves. The
//! camera retargets all the time (every wheel notch, every key press), and
//! a spring that restarts feels like it stutters. This one keeps its
//! velocity when asked for a new target, as Islands' springs do.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub damping: f64,
    pub stiffness: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f64,
    pub velocity: f64,
    pub target: f64,
    pub params: Params,
    /// Distance (and speed) below which the spring snaps to rest.
    pub epsilon: f64,
}

impl Spring {
    pub fn new(value: f64, params: Params) -> Self {
        Spring { value, velocity: 0.0, target: value, params, epsilon: 0.001 }
    }

    pub fn with_epsilon(mut self, epsilon: f64) -> Self {
        self.epsilon = epsilon;
        self
    }

    /// Moves the goal; the current speed is kept.
    pub fn retarget(&mut self, target: f64) {
        self.target = target;
    }

    /// Jumps to `value` and stops.
    pub fn set(&mut self, value: f64) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    pub fn is_resting(&self) -> bool {
        self.value == self.target && self.velocity == 0.0
    }

    /// Advances by `dt` seconds. Returns true while still moving.
    pub fn step(&mut self, dt: f64) -> bool {
        if self.is_resting() {
            return false;
        }
        // Mass 1: c = 2ζ√k. Semi-implicit Euler in 1 ms steps is stable for
        // every stiffness Atlas uses and cheap enough for a dozen springs.
        let c = 2.0 * self.params.damping * self.params.stiffness.sqrt();
        let mut left = dt.clamp(0.0, 0.064);
        while left > 0.0 {
            let h = left.min(0.001);
            let a = -self.params.stiffness * (self.value - self.target) - c * self.velocity;
            self.velocity += a * h;
            self.value += self.velocity * h;
            left -= h;
        }
        if (self.value - self.target).abs() < self.epsilon && self.velocity.abs() < self.epsilon * 10.0 {
            self.set(self.target);
            return false;
        }
        true
    }
}

/// Momentum after a fling: the speed falls off exponentially until it is too
/// small to see.
#[derive(Clone, Copy, Debug, Default)]
pub struct Momentum {
    pub vx: f64,
    pub vy: f64,
}

/// How fast a fling dies down, per second (e^-4.5 ≈ 1 % after one second).
pub const FRICTION: f64 = 4.5;

impl Momentum {
    pub fn is_resting(&self) -> bool {
        self.vx == 0.0 && self.vy == 0.0
    }

    /// Returns the distance to move during `dt`, and slows down.
    pub fn step(&mut self, dt: f64) -> (f64, f64) {
        let dt = dt.clamp(0.0, 0.064);
        let decay = (-FRICTION * dt).exp();
        // Exact integral of v·e^(-ft) over dt.
        let factor = (1.0 - decay) / FRICTION;
        let d = (self.vx * factor, self.vy * factor);
        self.vx *= decay;
        self.vy *= decay;
        if self.vx.hypot(self.vy) < 8.0 {
            self.vx = 0.0;
            self.vy = 0.0;
        }
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROW: Params = Params { damping: 0.58, stiffness: 340.0 };
    const CAMERA: Params = Params { damping: 1.0, stiffness: 260.0 };

    fn run(s: &mut Spring) -> (usize, f64) {
        let mut frames = 0;
        let mut peak: f64 = s.value;
        while s.step(1.0 / 60.0) {
            frames += 1;
            peak = peak.max(s.value);
            assert!(frames < 600, "spring never rests");
        }
        (frames, peak)
    }

    #[test]
    fn underdamped_overshoots_and_rests_exactly() {
        let mut s = Spring::new(0.0, GROW);
        s.retarget(1.0);
        let (frames, peak) = run(&mut s);
        assert!(peak > 1.05, "a damping of 0.58 overshoots, got {peak}");
        assert_eq!(s.value, 1.0);
        assert!(frames < 90, "should settle within 1.5 s, took {frames} frames");
    }

    #[test]
    fn critically_damped_never_passes_the_target() {
        let mut s = Spring::new(0.0, CAMERA);
        s.retarget(100.0);
        let (_, peak) = run(&mut s);
        assert!(peak <= 100.0 + 1e-9);
    }

    #[test]
    fn retarget_keeps_speed() {
        let mut s = Spring::new(0.0, CAMERA);
        s.retarget(100.0);
        for _ in 0..5 {
            s.step(1.0 / 60.0);
        }
        let v = s.velocity;
        s.retarget(200.0);
        assert_eq!(s.velocity, v);
    }

    #[test]
    fn momentum_covers_v_over_friction() {
        let mut m = Momentum { vx: 900.0, vy: 0.0 };
        let mut x = 0.0;
        while !m.is_resting() {
            x += m.step(1.0 / 60.0).0;
        }
        assert!((x - 900.0 / FRICTION).abs() < 5.0, "{x}");
    }
}
