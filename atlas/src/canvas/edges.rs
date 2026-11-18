//! Relation lines: where they leave and enter a card, their curve, and the
//! crow's-foot marks at both ends.
//!
//! A line leaves the child at the row of its foreign key column and enters
//! the parent at the row of the referenced column (or at the header while
//! that row is folded away). It takes the facing sides of the two cards; if
//! the cards overlap horizontally it goes out and back in on the right.

use atlas_core::schema::End;
use gtk::{graphene, gsk};

/// Room at each end for the marks before the curve begins.
const STUB: f32 = 22.0;

#[derive(Clone, Copy, Debug)]
pub struct Anchor {
    pub x: f32,
    pub y: f32,
    /// -1 leaves to the left, +1 to the right.
    pub dir: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
}

/// The two anchors (child, parent) for cards `a` (child) and `b` (parent)
/// with rows at `ya` and `yb` (card coordinates).
pub fn anchors(a: Rect, ya: f32, b: Rect, yb: f32, same_card: bool) -> (Anchor, Anchor) {
    let right = |r: Rect, y: f32| Anchor { x: r.x + r.w, y: r.y + y, dir: 1.0 };
    let left = |r: Rect, y: f32| Anchor { x: r.x, y: r.y + y, dir: -1.0 };
    if same_card {
        return (right(a, ya), right(b, yb));
    }
    if a.x > b.x + b.w + 24.0 {
        (left(a, ya), right(b, yb))
    } else if a.x + a.w + 24.0 < b.x {
        (right(a, ya), left(b, yb))
    } else {
        (right(a, ya), right(b, yb))
    }
}

/// The curve between the two anchors, stubs included.
pub fn curve(c: Anchor, p: Anchor) -> gsk::Path {
    let b = gsk::PathBuilder::new();
    let s0 = (c.x + c.dir * STUB, c.y);
    let s1 = (p.x + p.dir * STUB, p.y);
    let reach = ((s1.0 - s0.0).abs() * 0.5).max(48.0);
    b.move_to(c.x, c.y);
    b.line_to(s0.0, s0.1);
    b.cubic_to(s0.0 + c.dir * reach, s0.1, s1.0 + p.dir * reach, s1.1, s1.0, s1.1);
    b.line_to(p.x, p.y);
    b.to_path()
}

/// Marks for one end: strokes, and circles to be filled with the ground
/// colour first so the line does not show through them.
pub fn marks(at: Anchor, end: End) -> (gsk::Path, Option<gsk::Path>) {
    let b = gsk::PathBuilder::new();
    let along = |d: f32| at.x + at.dir * d;
    let bar = |b: &gsk::PathBuilder, d: f32| {
        b.move_to(along(d), at.y - 6.0);
        b.line_to(along(d), at.y + 6.0);
    };
    let circle = |d: f32| {
        let c = gsk::PathBuilder::new();
        c.add_circle(&graphene::Point::new(along(d), at.y), 3.5);
        c.to_path()
    };
    match end {
        End::ExactlyOne => {
            bar(&b, 7.0);
            bar(&b, 12.0);
            (b.to_path(), None)
        }
        End::ZeroOrOne => {
            bar(&b, 7.0);
            (b.to_path(), Some(circle(16.5)))
        }
        End::ZeroOrMany => {
            // The foot spreads into the card: three lines meeting 12 out.
            for dy in [-6.0, 0.0, 6.0] {
                b.move_to(along(12.0), at.y);
                b.line_to(at.x, at.y + dy);
            }
            (b.to_path(), Some(circle(16.5)))
        }
    }
}
