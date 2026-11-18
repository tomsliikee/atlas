# Motion

All values live in **`atlas/src/motion.rs`**. They follow the shared guideline and borrow the springs of **Islands**, so both read as one family.

---

## Values

| Name | Value | Used for |
| :--- | :--- | :--- |
| hover | **120 ms** | hover tints (CSS transitions in `data/style.template.css`) |
| **`STATE_MS`** | **200 ms** | state changes, the theme cross-fade |
| **`LAYOUT_MS`** | **320 ms** | layout changes that are not springs |
| **`EASE`** | **ease-out cubic** | every timed animation |
| **`GROW`** | damping **0.58**, stiffness **340** | a card unfolding |
| **`SHRINK`** | damping **0.75**, stiffness **420** | a card folding |
| **`HOVER`** | damping **0.7**, stiffness **520** | cards fading when another is focused |
| **`CAMERA`** | damping **1.0**, stiffness **260** | flying to a table, wheel zoom and pan, Tidy Up, cards making room; critically damped, a map must not wobble past its target |
| **`PANEL`** | damping **0.86**, stiffness **300** | the data panel growing out of its card; softer than GROW so a large rectangle does not visibly overshoot |
| momentum | speed falls by **e^-4.5** per second | after a touchpad fling or a dragged view |

Mass is **1** everywhere.

---

## Rules

- **Nothing moves at rest.** The canvas springs (`atlas-core::spring`) run in a frame-clock tick that exists only while something moves. Window-level motion (panel, cross-fade) uses **`Adw.SpringAnimation`** and **`Adw.TimedAnimation`**. No timers anywhere in normal use.
- **Motion says something happened.** A fade or spring answers an action of the user or a change of data, never decorates.
- **A spring asked again keeps its speed** (an Islands rule): the canvas uses its own springs instead of `Adw.SpringAnimation` because every wheel notch and key press retargets the camera, and a spring that restarts stutters. *Tested* (`spring::tests::retarget_keeps_speed`).
- **Zoom keeps the point under the pointer still.** While the zoom spring moves, the pan follows from the anchor, so the table under the pointer stays where it is.

---

## The Panel Growing out of a Card

*Tested* by screenshots during and after it. A plain box with the panel's fill (the **ghost**) is laid over the canvas at the card's screen rectangle and moves to the panel's rectangle on **`PANEL`**; the panel itself fades in over the last 18 % of that way. Closing runs the other way in **`LAYOUT_MS`** and shrinks back into the card if it is still there.

---

## The Theme Cross-Fade

*Built* in **`window.rs`** (`cross_fade`): **`gtk::WidgetPaintable::current_image()`** takes one still of the window content, the scheme changes underneath, and a **`gtk::Picture`** with that still fades from 1 to 0 in **`STATE_MS`** over an **`Overlay`**, then is removed. It costs one texture for 200 ms.
