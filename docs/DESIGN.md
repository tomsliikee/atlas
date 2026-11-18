# Design

**Atlas** uses **Insights Mono**, the design system of **Lantern** (Material Search): black, white and grey, colour only where it means something. The copy in **`insights-mono/`** was taken from **`~/git/lantern/insights-mono/`** on 2026-11-18.

---

## Where the Values Come From

| Source | Becomes | How |
| :--- | :--- | :--- |
| **`insights-mono/tokens.json`** | `--im-<name>` CSS variables, light and dark | **`atlas-core::tokens`**, called by **`atlas/build.rs`** |
| **`data/style.template.css`** | the rules of Atlas | appended to the light variables; uses only `var(--im-…)` |
| libadwaita's named colours | the same tokens | the map in **`tokens.rs`** (`window-bg-color` → `bg`, `accent-bg-color` → `solid`, …) |

**`style.css`** holds the light variables, the dark ones inside `@media (prefers-color-scheme: dark)`, and all rules. **libadwaita 1.10** answers that media query from its own colour scheme, so Atlas's override counts too (it deprecates the old `style-dark.css`). Shadows (`shadow-bar`) are tokens per theme as well. The drawing code reads the same file at start (`atlas/src/palette.rs`). The two SQL colour schemes in `data/sourceview/styles/` repeat `card`, `ink`, `ink2`, `ink3` and `fill` by hand, because GtkSourceView reads them from files. No colour is typed by hand in Rust or CSS (*tested*: `cargo test -p atlas-core` checks that every mapped token exists in the real file).

---

## Rules

- **Colour:** pure black and white. **PK** and **FK** are told apart by small mono markers and weight, not by hue. The accent is ink itself (`solid` / `on-solid`). The only other colours in the file (`down`, `up`) are kept for errors.
- **Type, one job per family:** **Geist Serif** only for statements (the database name, an empty state), **Inter** with tabular figures for everything else, **Geist Mono** for column types, detail values and SQL. All three are bundled (Geist Mono 1.9.2 from Vercel's release, OFL).
- **Radii:** the Insights Mono scale as Lantern uses it: **`radius-card` 22 px** for cards and panels, **`radius-pill`** for pills, **`radius-segmented` 12 px**, **`radius-chip` 4 px**. A nested shape takes the next step down.
- **Elevation:** one shadow, and only for what really floats (popovers, the search, a card being dragged). Cards separate by fill (`card` on `bg`).
- **Selection is neutral:** a faint tint and a slightly heavier label, no outline, no glow.
- **Lines:** `line` hairlines only where air cannot do the job.

---

## Light and Dark

**Atlas** follows the system (**GNOME**, **Cinnamon** and **KDE** through the settings portal). The sun/moon button in the header bar flips what is on screen and stores the choice in **GSettings** (`color-scheme`: `system`, `light`, `dark`); **Follow System Style** in the main menu resets it. The switch cross-fades: a still image of the window is laid over it and faded out in **200 ms** (see [MOTION.md](MOTION.md)).
