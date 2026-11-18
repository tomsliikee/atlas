# Rendering

The canvas is one `gtk::Widget` (`atlas/src/canvas/mod.rs`) that draws itself in `snapshot()`. GTK turns the snapshot into **GSK render nodes**, which the GL or Vulkan renderer draws on the GPU.

---

## One Frame

| Step | What | Cost |
| :--- | :--- | :--- |
| **Ground** | one colour node | constant |
| **Dot grid** | one dot inside a **repeat node**, tiled by the GPU; faded out below 35 % zoom | constant |
| **Camera** | one translate and one scale around everything that follows | constant |
| **Lines** | per relation: a path (stroke), its marks, and filled circles under the marks | about 100 paths for 50 tables |
| **Cards** | per card: a rounded fill, a rounded clip, and its **cached content node** | one node per card |

A card's text and chips are built once into a render node and kept (`Card::cache`) with the state it was built for: unfolded, selected, dark, inferred relations shown. Moving the camera or a card does not rebuild it; only a change of that state does. The height of an unfolding card is a clip around the cached node, so the reveal costs nothing extra.

---

## Text

Text goes through **Pango** with the bundled fonts (`/app/share/fonts/atlas`). GSK caches glyphs and draws text nodes under the camera's scale at the screen's resolution, so text stays sharp at every zoom.

---

## Ticks

`animate()` adds a frame-clock tick callback only when something moves (a spring, a fling) and removes it when everything rests. An idle diagram draws only when GTK asks (resize, theme change).

---

## Export

`Canvas::export_node()` draws the same scene at zoom 1 without dimming or the grid. **PNG** goes through the window's own renderer at 2x (at most 16,000 px a side). **SVG** and **PDF** draw the same node with cairo (`RenderNode::draw`), which keeps lines and text as vectors.
