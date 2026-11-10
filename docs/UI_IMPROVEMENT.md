# N-shade UI Improvement Spec

Normal Map Studio: usability, accessibility and consistency pass.
Based on a screenshot audit (1915 × 1010, dark theme) against Nielsen's 10 heuristics, Shneiderman's Eight Golden Rules, Fitts's/Hick's/Miller's laws and WCAG 2.2 AA.

> **Rule for every change in this file: no feature is removed, renamed in file formats, or moved out of reach.** Only presentation, labelling, grouping, feedback and input affordances change. See section 1.

---

## 0. How to use this file in your IDE

1. Put this file in the repo root (or `/docs`).
2. Open it next to the code. Work **one section 8 phase at a time**.
3. Paste this prompt into your IDE assistant (Copilot, Cursor, Claude Code, etc.):

```text
Read UI_IMPROVEMENT.md. Implement Phase <N> only.
Constraints:
- Do not remove, rename or relocate any feature listed in section 1.
- Do not change file formats, export output, saved project schema or shader code.
- Use the design tokens in section 4 instead of hard-coded colours, sizes and fonts.
- Every interactive element needs: accessible name, visible focus ring, hover/active/disabled states, minimum 24x24 px hit area (32 px preferred).
- After the change, run the regression checklist in section 9 and list anything you could not verify.
```

4. The stack is unknown to this document (the screenshot looks like a web-view or Qt-style UI). Tokens are given as CSS variables with a Qt/QSS mapping note, so adapt to what you use.
5. A working HTML prototype of most of this exists (`nshade-redesign.html`). Treat it as a visual reference only, not production code.

---

## 1. Functionality-preservation contract

Everything visible in the screenshot must still exist and behave the same after the redesign.

| Area | Features that must be preserved |
|---|---|
| Menus | File, Edit, View, Tools |
| History | Undo, Redo |
| Window/layout | Two orange panel toggles (left/right panel visibility, assumed), lighting (sun), the "stand" icon, the circle-slash icon, settings sliders icon |
| Tools | Select, Rectangle, Circle, Star, Pen (has sub-tool flyout), Zoom/Search (has flyout), Brush, Eraser, Pan/Move |
| Left rail bottom | Cube (3D) icon, grid icon |
| Layers | Add (+), visibility, thumbnail, name, type (SHAPE/FLAT), mask slot, per-layer blend mode, drag-reorder, move up/down, 3 more footer icons, Reset, layer count |
| Layer-panel header | Blend dropdown + Opacity % |
| Export | Normal, Height, Project, 16-bit, Edge wear, Curvature, Edge detect, ID map, Normal Y (OpenGL Y+/DirectX Y−), Height source ("Integrated (normals)"), Size ("Document (1024)"), Folder…, output path, Export maps |
| Viewport | 3D View / 2D Canvas / Split, mesh selector (Cube), 5 viewport icons, zoom label, orbit/zoom hint, lighting label |
| Right tabs | Properties, Mesh, Bake, Lightmap, PBR |
| Properties | Name, Visible, Opacity, Blend mode, Material ID, Mask (Paint a mask, Bind below, + Mask layer, Source, + Add smart mask), Shape (Style, Radius, Height, Softness, Smooth, Carve (invert), Profile + curve editor, Convert to editable path, Extract outline) |
| Status bar | Active view, document size, RAM, GPU %, VRAM, GPU indicator |

Allowed: restyle, relabel (user-facing text only), regroup, add tooltips/shortcuts/feedback, make panels resizable/collapsible.
Not allowed: delete a control, hide a feature behind an unlabeled menu, change data model or output.

---

## 2. Audit by region

Priority: **P0** blocks use or accessibility, **P1** causes errors or slowness, **P2** polish.
Colours below are estimated from the screenshot. Measure real pixels with a colour picker before final sign-off.

### 2.1 Window chrome and top bar

| # | Observed | Problem | Fix | Principle | Pri |
|---|---|---|---|---|---|
| T1 | Thin blue line above the title bar | Colour is unrelated to the orange brand/accent; may read as an OS title bar leftover | Remove, or use it only as a loading/busy indicator in the accent colour | Consistency (N4) | P2 |
| T2 | Logo "N" tile + "N-shade / NORMAL MAP STUDIO" | Subtitle is ~10 px and low contrast | Keep, raise subtitle to 11–12 px, `--text-muted` | Legibility (WCAG 1.4.3) | P2 |
| T3 | Menus File/Edit/View/Tools are plain text, ~13 px | No visible hover/open state, small targets | Min height 32 px, hover and pressed backgrounds, keyboard `Alt` access keys | Fitts, N7 | P1 |
| T4 | Undo/Redo shown as very dim icons at top right | Disabled state is indistinguishable from low-contrast enabled state; no tooltip or shortcut | Disabled at 45% opacity with `cursor:not-allowed`, enabled at full contrast; tooltips "Undo (Ctrl+Z)" and "Redo (Ctrl+Shift+Z)"; optional history dropdown | Control & freedom (N3) | P0 |
| T5 | Two orange filled/outlined square icons | Orange = "on" but nothing says what they toggle | Add `aria-pressed`, tooltips "Show/hide left panel", "Show/hide right panel", shortcuts | Visibility (N1) | P1 |
| T6 | Sun, stand, circle-slash, sliders icons, no labels, equal weight | Icon-only, ambiguous meanings, no grouping | Group with dividers: History \| Panels \| View/Lighting \| Settings. Tooltips with names and shortcuts. Give Settings a text label on wide windows | Recognition over recall (N6) | P1 |
| T7 | Top-right cluster is far from the tool and canvas | Frequently used controls are far from the cursor's usual position | Keep position (standard), but duplicate Undo/Redo nowhere; rely on Ctrl+Z | Fitts | P2 |

### 2.2 Left tool rail

| # | Observed | Problem | Fix | Pri |
|---|---|---|---|---|
| R1 | 8 icon-only tools, ~36 px, select tool filled orange | No labels, no shortcut hints, tiny corner triangles (Pen, Zoom) are the only flyout cue | Add tooltips with name and key (V, R, O, S, P, Z, B, E, H). Widen rail to 56–64 px and show a short label under each icon (collapsible to icon-only). Make the flyout triangle ≥ 8 px and add `aria-haspopup`. Long-press or right-click opens the flyout | P0 |
| R2 | Active tool has bright orange fill; others have no hover state visible | Hard to tell hover from rest | Hover background `--surface-3`, pressed `--accent` | P1 |
| R3 | Rectangle and Circle look like filled glyphs, Star is filled, Pen is a nib | Icon style is inconsistent (filled vs outline) | One icon set, 20 px, 1.8 px stroke, outline style | P2 |
| R4 | Cube and grid icons at the bottom, cube outlined in orange | Unclear what they do (probably 3D/2D or library toggles) and why one is outlined | Name them in tooltips, and show selected state the same way as tools | P1 |
| R5 | Tools are not grouped | 8 items in a flat list | Group by purpose with thin dividers: Selection (Select, Pan) \| Shapes (Rect, Circle, Star, Pen) \| Paint (Brush, Eraser) \| View (Zoom) | P2 |

### 2.3 Layers panel

| # | Observed | Problem | Fix | Pri |
|---|---|---|---|---|
| L1 | Header: "Layers" + small "+" button | "+" is ~28 px with no label, and it adds what? | Button reads "+ Add layer" (icon + text), shortcut Shift+N, optional split menu for layer type (Shape, Fill, Paint) | P1 |
| L2 | Below header: dropdown "Derivative" + "Opacity 100%" | Applies to which layer? It duplicates the per-row dropdown and the Properties panel, so there are **three** blend controls | Keep functionality, but show these only in Properties. In the layers panel, keep a read-only badge per row. If the header control is a *global default for new layers*, relabel "New layer blend" and move it to the Add layer menu | P0 |
| L3 | Opacity shows `100%` in Layers but `1.000` in Properties | Same value, two scales (0–100 vs 0–1) | Use **percent everywhere** in the UI (store 0–1 internally) | P0 |
| L4 | Row: orange target-like visibility icon | The icon looks like a record/target button; it stays orange when visible and probably dims when hidden, so state is subtle | Use eye / eye-off, `aria-pressed`, hidden rows dim name and thumbnail to 45% | P0 |
| L5 | Row: 40 px thumbnail with shape glyph | Glyph, not a real preview of the layer's contribution | Option: render a live mini normal/height preview; fall back to the glyph. Do not block on it | P2 |
| L6 | Row: name (white) and "SHAPE"/"FLAT" (~10 px, grey) | Type text is tiny and low contrast | 12 px, `--text-muted`, sentence case ("Shape", "Flat") | P1 |
| L7 | Row: small square slot to the right (orange outline when selected) | Unlabeled: mask? lock? material colour? | Tooltip "Layer mask" (confirm its real meaning). Empty = dashed border, has mask = filled thumbnail | P1 |
| L8 | Row: per-row blend dropdown "Derivative" with chevron | Crowds the row, and changing blend mode inline is a rare action that risks accidental change | Show as a read-only badge; edit in Properties (or on click of the badge open a popover) | P1 |
| L9 | Row: ⠿ drag handle at the far edge, ~10 px wide | Hard to hit; dragging is the only visible way to reorder | Keep drag, enlarge handle to ≥ 24 px, add keyboard reorder (Alt+↑/↓) and footer ↑/↓ buttons (already exist). Required by WCAG 2.5.7 (dragging alternative) | P1 |
| L10 | Selected row: blue-tinted background with orange left bar/outline | Two cues (blue + orange) conflict with the brand | Selected = `--surface-3` background, 1 px accent border, 3 px accent left bar | P2 |
| L11 | Footer: 5 tiny unlabeled icons (appear to be up, down, duplicate/merge, a brush-like action, delete) + "Reset" + "4 layers" | Destructive Delete and Reset sit next to harmless actions; "Reset" has unclear scope and no undo hint; icons ~14 px | 32 px targets, tooltips with shortcuts, separate destructive actions (right-aligned, danger colour on hover). Delete and Reset execute immediately but show an **Undo toast** (6 s) instead of a modal. Rename "Reset" to "Reset layers" if that is what it does, and confirm | P0 |
| L12 | ~330 px of empty space under 4 layers while Export is cramped below | Wasted space, uneven density | Make the Layers/Export split resizable (drag divider) and remember the size; Export is collapsible (already has a caret) | P1 |
| L13 | No lock, no solo | Easy to edit the wrong layer | Add a **lock** toggle (new, additive) and optional solo | P2 |

### 2.4 Export panel

| # | Observed | Problem | Fix | Pri |
|---|---|---|---|---|
| E1 | Chips: Normal, Height, Project, 16-bit (orange outline = on); Edge wear, Curvature, Edge detect, ID map (grey = off) | **Mixes three kinds of controls**: maps (Normal, Height, Edge wear, Curvature, Edge detect, ID map), a file option (16-bit), and a scope (Project). Outline-only "on" state is weak and looks like a button | Split into: **Maps** (checkbox chips with a check icon when on), **Format** (8-bit / 16-bit segmented), **Also save** (Project file checkbox) | P0 |
| E2 | "Normal Y" label is ~10 px and clipped to "Normal Y" with trailing text | Jargon, and the labels "OpenGL (Y+)" / "DirectX (Y−)" don't say which software needs which | Segmented control, with helper text "OpenGL: Blender, Unity, Godot. DirectX: Unreal." | P1 |
| E3 | "Height" row with dropdown "Integrated (normals)" + caption "Rebuild from the final normal map: matches the normals, but can drift and ring at corners." | "Height" appears twice (map chip and source row) with no visible link; the caption is 10 px and technical | Rename the row "Height source". Show it only when the Height map is checked (disable and explain otherwise: "Turn on the Height map to choose a source."). Caption 12 px | P1 |
| E4 | "Size" dropdown "Document (1024)" with an up-pointing triangle; "Folder…" button | Arrow glyph is inconsistent with other dropdowns; "Folder…" doesn't say it sets the destination | Same chevron on all selects. Label "Save to", field shows the path, button "Choose folder…" | P1 |
| E5 | "To: output (default)" caption | Tiny, ambiguous | Show the full resolved path, truncate in the middle, tooltip with the full path, "Open folder" after export | P1 |
| E6 | Orange "Export maps" button | Doesn't tell how many or which maps; no progress; no completion feedback; stays enabled with nothing selected | Label "Export N maps" (disabled with 0 selected, with reason). While running: inline progress bar and cancel. After: toast "Exported N maps to <path>" with **Open folder** | P0 |
| E7 | No summary | User can't confirm settings at a glance | One-line summary under the button: "Normal, Height · 16-bit · 1024 × 1024 · OpenGL" | P1 |

### 2.5 Viewport

| # | Observed | Problem | Fix | Pri |
|---|---|---|---|---|
| V1 | Tabs "3D View", "2D Canvas", "Split" | Selected state is only a faint underline | Proper tab pattern (`role=tab`, `aria-selected`), filled selected state | P1 |
| V2 | Dropdown "Cube" + 5 icons (move, pan, cube, rotate, fullscreen) | Icons unlabeled and some look alike (two move glyphs) | Tooltips with shortcuts (F = frame, Home = reset view), distinct glyphs, group with dividers. Label "Preview mesh" for the dropdown | P1 |
| V3 | "Cube · 1.00×" chip at top-left | Fine, but low contrast and not clickable | Make it a click-to-edit zoom field (type 200%, or choose Fit/100%) | P2 |
| V4 | Hints "drag to orbit · scroll to zoom" and "Lighting · Studio" at the bottom, ~10 px | Small, low contrast, hint never goes away | 12 px; show hint for the first sessions only, then keep a "?" for shortcuts. "Lighting · Studio" becomes a clickable control that opens lighting presets (same as the sun icon) | P2 |
| V5 | Plane fills the viewport, no orientation cue | User can lose track of orientation, and 2D/3D light direction is invisible | Optional axis gizmo and a light-direction handle (additive, off by default) | P2 |
| V6 | No busy state | Re-render after a slider change is silent | Thin progress line under the tabs while the normal map recomputes, and the status bar says "Updating preview…" | P1 |

### 2.6 Right panel (tabs and Properties)

| # | Observed | Problem | Fix | Pri |
|---|---|---|---|---|
| P1 | Tabs: Properties, Mesh, Bake, Lightmap, PBR, equal width, ~11 px, selected state is a thin orange outline | Hard to read, tiny targets (~24 px high), no keyboard pattern | 32–36 px tall tabs, 13 px text, selected = filled surface + 2 px accent underline, arrow-key navigation, shortcuts 1–5. **These are the only entry to Mesh/Bake/Lightmap/PBR, so they must never be hidden** | P0 |
| P2 | Header "Properties" with icon, but no hint of which layer | The panel title doesn't say what it edits | "Properties — emblem" and the layer thumbnail | P1 |
| P3 | Name field, "Visible" toggle, "Opacity 1.000", "Blend mode", "Material ID: Assign color" | Opacity scale differs from Layers (see L3). "Assign color" is a button-like text field and its purpose (ID map colour) is unclear. Toggle "Visible" duplicates the eye icon (fine, but must stay in sync) | Opacity: slider + number in %. "Material ID colour": show a colour swatch + "Assign colour" button. Keep Visible toggle bound to the same state as the eye | P1 |
| P4 | Section "Mask" with sub-headings PAINTED, CLIP TO LAYER, SMART MASKS (all-caps, 10 px, very low contrast) | Labels are too small; three concepts stacked; long grey paragraphs | Sentence case sub-headings 12 px. Descriptions shortened to one line and moved to an info tooltip (ⓘ). Collapse by default when no mask is set | P1 |
| P5 | "Bind below" and "+ Mask layer" side by side, then "Source: None" | "Bind below" vs "Source" relationship is unclear; the paragraph about binding hiding the source is the only explanation | Group as "Clip to layer": Source select first, then the two buttons. One-line helper: "Binding hides the source so it acts only as a mask." | P1 |
| P6 | Section "Shape · primitive" with Style: Fill | Middle dot label style is inconsistent with "Mask" | Section title "Shape" with a quiet "primitive" tag | P2 |
| P7 | Sliders: Radius 14.0, Height 10.0, Softness 0.0, Smooth 0.0 | **Radius thumb sits at the far right while the value is 14** (suggesting max = 14 or a clamped range), Height thumb near the left at 10, Softness/Smooth thumbs are hollow circles at zero. Inconsistent thumb styles and ranges; values are not visibly bounded | Verify and document each range (min/max/step/unit). One slider style: filled track to the left of the thumb, same thumb in all states. Show range ticks on hover. Double-click the label or thumb to reset to default. Allow typing beyond the slider's soft range if the model allows it | P0 |
| P8 | Number fields without units next to labels that have "(px)" | Units are in the label, so the value is ambiguous when the label is truncated | Put the unit inside or beside the field | P2 |
| P9 | 4-line paragraph under the sliders: "Averages the outline and its shading…" | 10 px, dense, low contrast; explains Smooth and Softness together | Replace with two single-line hints under their sliders, or ⓘ tooltips | P1 |
| P10 | Toggle "Carve (invert)" | Technical wording | "Carve into the surface" with helper "Off: raised. On: recessed." Preview icon beside it | P1 |
| P11 | "Profile: Round" select + small curve thumbnail | Thumbnail looks decorative; the select arrow points up | Make the curve thumbnail clickable (opens the curve editor), same chevron as other selects | P1 |
| P12 | "Convert to editable path" button and a partially visible "Extract outline" | Panel clips content without a scroll cue; "Convert…" is irreversible? | Add a scroll shadow at the bottom edge, sticky section headers. Convert to path should be undoable and say so in the tooltip | P1 |

### 2.7 Status bar

| # | Observed | Problem | Fix | Pri |
|---|---|---|---|---|
| S1 | Bottom-left: "3D View" (orange) and "1024 × 1024" | Fine, but the orange text suggests it is clickable | Make it neutral, or make it a real button that opens the canvas size dialog | P2 |
| S2 | "RAM 15.2/15.7 GB (97%)" in the same grey as everything else | **97% memory use is a failure risk and looks normal** | Amber at ≥ 85%, red at ≥ 95%, always with an icon and text, plus a tooltip with advice ("Close other apps or lower export size") | P0 |
| S3 | GPU 42%, VRAM 0.8/8.0 GB, green dot "GPU" | Dot meaning unclear | Label "GPU active"; tooltip with the device name | P2 |
| S4 | No transient messages | The user gets no confirmation of actions | Left side shows the last action for 2.5 s ("Layer moved"), then "Ready" | P1 |

---

## 3. Cross-cutting issues

| # | Issue | Evidence | Fix |
|---|---|---|---|
| X1 | **Accent overuse.** Orange is used for the active tool, selected chips, toggles, selected layer outline, visibility icons, top-right panel icons, tab outline and the Export button | Screenshot throughout | Orange means exactly "selected / on / primary action". Don't use it for decoration or for off-state icons (the visibility icon) |
| X2 | **Three scales of text**: 10 px captions, 11–12 px labels, 13 px body | Captions under Export, Mask, Shape | Minimum 12 px for any readable text. Define a type scale (section 4) |
| X3 | **Low contrast text**: grey captions and all-caps sub-labels look < 4.5:1 | Mask and Shape descriptions | Muted text ≥ 4.5:1, disabled text exempt but must be distinguishable |
| X4 | **Control borders are barely visible** (dark grey on dark grey) | Inputs, dropdowns, chips | Borders ≥ 3:1 against the adjacent surface (WCAG 1.4.11) |
| X5 | **Inconsistent dropdown arrows** (down chevron, filled triangle ▲ on Size and Profile) | Export, Properties | One chevron icon; flips on open |
| X6 | **Hit targets**: many controls ~14–28 px | Footer icons, drag handles, "+" | ≥ 24×24 CSS px (WCAG 2.5.8), 32 px preferred, 44 px for primary touch |
| X7 | **No visible keyboard focus** (not detectable in a screenshot; verify) | n/a | 2 px accent outline with 2 px offset on every focusable element (WCAG 2.4.7) |
| X8 | **State vs action ambiguity**: outlined orange chips, outlined buttons and "on" toggles look alike | Export chips vs "Bind below" | Toggles show a check/pressed state; buttons are solid or outlined with no check |
| X9 | **Mixed vocabulary**: "Radius", "Smooth", "Softness"; "Carve (invert)"; "Bind below"; "Derivative" | Properties | Plain labels; add tooltips with one-sentence definitions |
| X10 | **Fixed panel proportions**, no resize handles visible | Left column | Resizable, collapsible, persisted panels |
| X11 | **No empty/disabled explanations** | Disabled Undo/Redo | Tooltip says why ("Nothing to undo") |
| X12 | **Destructive actions without a safety net** | Trash, Reset | Undo toast instead of modal dialogs |

---

## 4. Design tokens

> **Superseded:** the shipped palette is the Vibrant Dark System documented in `docs/UI_IMPROVEMENT_MATERIAL_EGUI.md` §3.

Define once, reference everywhere. Values are proposals tuned for the existing dark UI and orange accent.

```css
:root{
  /* surfaces */
  --bg:        #101318;  /* viewport / app background */
  --surface-1: #171b22;  /* panels */
  --surface-2: #1d222b;  /* inputs, cards */
  --surface-3: #262c38;  /* hover, selected row */
  --divider:   #2c3340;  /* decorative lines only */
  --ctl-border:#76809a;  /* control borders, >= 3:1 on surfaces */

  /* text */
  --text:       #e8ebf1; /* ~14:1 on surface-1 */
  --text-muted: #a6aebd; /* ~7:1 on surface-1 */
  --text-disabled:#6b7384;

  /* accent (selected / on / primary) */
  --accent:     #f0a03c;
  --accent-ink: #1c1203; /* text on accent */

  /* status (always paired with an icon and text) */
  --ok:#6fcf8e; --warn:#f2c14e; --danger:#ff8a80;

  /* type */
  --font: "IBM Plex Sans", system-ui, sans-serif;
  --fs-xs:12px;  /* captions, helper text (minimum) */
  --fs-sm:13px;  /* labels, controls */
  --fs-md:14px;  /* body, layer names */
  --fs-lg:16px;  /* panel titles */
  --lh:1.45;

  /* space (4 px grid) */
  --sp-1:4px; --sp-2:8px; --sp-3:12px; --sp-4:16px; --sp-6:24px;

  /* shape */
  --r-sm:4px; --r-md:6px; --r-lg:8px;

  /* hit targets */
  --hit-min:24px; --hit:32px; --hit-primary:44px;

  /* motion */
  --t-fast:120ms; --t-med:200ms;
}
@media (prefers-reduced-motion:reduce){*{transition:none!important;animation:none!important}}
:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
```

**Qt/QSS note:** QSS has no variables. Generate the stylesheet from these tokens at build time (string template), and set the focus outline with a custom `QProxyStyle` or `border` on `:focus`.

**Light theme (optional):** swap the surfaces and text and darken the accent to ~`#b85f00` so that text on it stays ≥ 4.5:1.

### Contrast reference (compute again with your final colours)

| Pair | Target | Proposed |
|---|---|---|
| `--text` on `--surface-1` | ≥ 4.5:1 | ~14:1 |
| `--text-muted` on `--surface-1` | ≥ 4.5:1 | ~7:1 |
| `--ctl-border` on `--surface-1` | ≥ 3:1 | ~4.4:1 |
| `--accent` on `--surface-1` | ≥ 3:1 | ~8:1 |
| `--accent-ink` on `--accent` | ≥ 4.5:1 | ~9:1 |

---

## 5. Component specs

### 5.1 Icon button
- Size 32×32 (hit area never < 24×24), icon 18 px, 1.8 px stroke.
- States: rest, hover (`--surface-3`), pressed/on (`--accent` icon or fill), disabled (45% opacity), focus ring.
- Always has `aria-label` and a tooltip "Name (Shortcut)". Toggle buttons use `aria-pressed`.

### 5.2 Tooltip
- Delay 400 ms, appears on hover and keyboard focus, dismisses on Esc, never covers its target.
- Format: `Action name (Key)`; second line optional for a short explanation.

### 5.3 Tool rail
- Width 64 px expanded (icon + 11–12 px label), 44 px collapsed (icon only). Toggle via View menu and a small chevron.
- Tool buttons 52×48. Active = accent fill. Shortcut letter in the corner (expanded mode).
- Flyout tools: triangle indicator ≥ 8 px, open on long-press, right-click or `Shift+key`.

### 5.4 Layer row (grid)
```
[ eye 32 ][ thumb 40 ][ name + type  (flex) ][ blend badge ][ lock 32 ]   (drag handle appears on row hover, 24 px, left edge)
```
- Row min height 52 px, gap 6 px.
- `role="option"` in a `role="listbox"`, arrow keys move selection, `Enter` renames, `Alt+↑/↓` reorders, `Del` deletes (with Undo toast), `H` toggles visibility of the selected layer.
- Hidden layers dim to 45%. Locked layers show a lock icon and disable their properties.
- Selected: `--surface-3`, 1 px accent border, 3 px inset accent bar on the left.

### 5.5 Layer list footer
- Left group: Move up, Move down, Duplicate, [existing 4th action]. Right group: Delete (danger on hover), "Reset layers".
- All actions announce through the status bar and (for Delete/Reset) an Undo toast.

### 5.6 Export panel
```
Maps            [✓ Normal] [✓ Height] [  Curvature] [  Edge detect] [  Edge wear] [  ID map]
Format          [ 8-bit | 16-bit ]
Normal Y        [ OpenGL (Y+) | DirectX (Y−) ]   Blender, Unity, Godot / Unreal
Height source   [ Integrated (normals) ▾ ]       (enabled only when Height is on)
Size            [ Document (1024) ▾ ]
Save to         [ output/                ] [Choose folder…]
[ ] Also save the project file
[ Export 2 maps ]    Normal, Height · 16-bit · 1024 × 1024 · OpenGL
```
- Checkbox chips show a ✓ when on, so state never depends on colour only.
- Button states: idle ("Export N maps"), disabled (0 maps, with reason), running (progress fill, % and Cancel), done (toast with Open folder).
- Preserve existing export code paths. This is a UI regrouping, so map the same underlying flags (`normal`, `height`, `project`, `bit16`, …) to the new controls.

### 5.7 Properties panel
- Sticky header: layer thumbnail + "Properties — <name>".
- Sections are `<details>`-style collapsibles with a chevron, state remembered per section.
- Slider row: `label (92 px) | slider | number + unit`. Slider and number are two-way bound, both with `aria-label`. Double-click resets to default. Arrow keys step, Shift+arrow ×10.
- Tooltips (ⓘ) hold the long explanations currently printed as paragraphs.

### 5.8 Tab strip (right panel)
- `role="tablist"`, 36 px tall, selected has 2 px accent underline and `--surface-2` fill.
- Left/Right arrows move focus, Home/End jump, `Ctrl+1…5` select.
- Remembers the last tab per session.

### 5.9 Viewport toolbar
- Tabs (3D View, 2D Canvas, Split) on the left, mesh selector and view icons on the right.
- View icons: Frame (F), Reset view (Home), Wireframe/Shading, Fullscreen (F11). Confirm which glyph maps to which existing action before renaming.

### 5.10 Status bar
- Left: last action or "Ready". Centre: document size. Right: RAM, GPU, VRAM.
- Thresholds: RAM/VRAM ≥ 85% amber + icon, ≥ 95% red + icon. Text always says what to do.

### 5.11 Toast
- Bottom centre above the status bar, `role="status" aria-live="polite"`, auto-dismiss 6 s, pauses on hover, optional action (Undo, Open folder).

---

## 6. Keyboard map (proposed, resolve conflicts with existing shortcuts first)

| Keys | Action |
|---|---|
| V R O S P Z B E H | Select, Rectangle, Circle, Star, Pen, Zoom, Brush, Eraser, Pan |
| Ctrl+Z / Ctrl+Shift+Z | Undo / Redo |
| Shift+N | Add layer |
| Ctrl+D | Duplicate layer |
| Del | Delete layer (undoable) |
| Alt+↑ / Alt+↓ | Move layer up / down |
| H (layer focused) | Toggle visibility |
| Ctrl+1…5 | Properties, Mesh, Bake, Lightmap, PBR |
| F / Home | Frame / reset view |
| Ctrl+E | Export maps |
| ? | Open shortcut cheat sheet |

---

## 7. Accessibility checklist (WCAG 2.2 AA)

- [ ] 1.4.3 Text contrast ≥ 4.5:1 (≥ 3:1 for 18 px+ or 14 px bold).
- [ ] 1.4.11 Non-text contrast ≥ 3:1 for control borders, icons, focus rings, slider thumbs.
- [ ] 1.4.1 Colour is never the only signal (status bar, chip state, layer visibility).
- [ ] 2.1.1 Every function works from the keyboard, including layer reordering and the curve editor (provide numeric fallback).
- [ ] 2.4.7 Focus is always visible. Tab order follows the visual order: top bar → rail → layers → viewport → right panel → status bar.
- [ ] 2.5.7 Drag-only actions (layer reorder, curve points) have a non-drag alternative.
- [ ] 2.5.8 Targets ≥ 24×24 px (or enough spacing).
- [ ] 4.1.2 Name, role and state exposed for custom widgets (tabs, listbox, toggles, sliders).
- [ ] Respect OS "reduce motion" and, where available, high-contrast mode.
- [ ] Support UI scaling at 125%, 150%, 200% without clipping (test at 1280×720).

---

## 8. Implementation plan

Do the phases in order. Each phase is shippable on its own and touches presentation only.

### Phase 1: Foundations (low risk)
1. Add the tokens from section 4 and replace hard-coded values.
2. Minimum font 12 px, raise muted text contrast, border contrast, unify chevrons.
3. Global focus ring and hover/pressed/disabled states.
4. Tooltips with shortcuts on every icon button (T4–T6, R1, L11, V2).

### Phase 2: Consistency fixes (P0 items)
1. Opacity as % everywhere (L3, P3).
2. One blend-mode control; read-only badges on rows (L2, L8).
3. Eye/eye-off visibility icon with `aria-pressed` (L4).
4. Status bar memory thresholds (S2).
5. Verify and fix slider ranges and thumb styles (P7).

### Phase 3: Export panel regrouping
1. Split chips into Maps / Format / Also save (E1).
2. Segmented Normal Y with helper (E2), "Height source" enabling rule (E3).
3. Export button states, progress, summary and completion toast (E6, E7).

### Phase 4: Panels and navigation
1. Right tab strip rebuild with keyboard pattern (P1).
2. Properties sections collapsible, helper text to tooltips (P4, P5, P9, P10).
3. Resizable and collapsible panels, persisted layout (X10, L12).
4. Tool rail labels and grouping (R1–R5).

### Phase 5: Safety net and feedback
1. Undo toasts for Delete and Reset (L11, X12).
2. Status-bar action messages and a viewport busy indicator (S4, V6).
3. Lock toggle (L13), keyboard reordering (L9), shortcut cheat sheet.

### Phase 6: Optional polish
Live layer thumbnails (L5), axis/light gizmo (V5), click-to-edit zoom (V3), light theme.

---

## 9. Regression checklist (run after every phase)

**Functional**
- [ ] Every item in the section 1 table is still reachable and works.
- [ ] Export produces byte-identical files to before for the same settings (compare checksums on a sample project: Normal, Height, 16-bit, OpenGL and DirectX).
- [ ] Project save/load round-trips unchanged. Old project files open.
- [ ] Undo/redo covers add, delete, reorder, duplicate, visibility, property edits, reset.
- [ ] Layer order, blend modes, masks and clipping render the same as before.
- [ ] Pen and Zoom flyouts still open.
- [ ] 3D, 2D and Split views still sync.

**UI**
- [ ] 1280×720, 1920×1080 and 2560×1440; 100% and 150% scaling: no clipped text.
- [ ] Right panel scrolls with a visible cue; no controls cut off.
- [ ] Every icon has a tooltip and accessible name.
- [ ] Tab through the whole UI using only the keyboard.
- [ ] Contrast measured with a tool on the final colours.

**Performance**
- [ ] No extra re-render of the viewport when only styles change.
- [ ] Slider drag stays ≥ 30 fps on the reference scene.

---

## 10. Questions to answer before coding

1. What do the two orange top-right icons, the sun, the "stand", the circle-slash and the sliders icon do exactly?
2. What does the square slot on each layer row mean (mask, lock, material colour)?
3. Is the "Derivative / Opacity" control above the layer list a global default or a control for the selected layer?
4. What are the real min/max/step values for Radius, Height, Softness and Smooth? (The Radius thumb looks clamped.)
5. What do the 4th and 5th icons in the layer footer do?
6. What do Mesh, Bake, Lightmap and PBR contain? They were not visible in the screenshot, so they need the same audit.
7. What UI framework is used (web view, Qt, Dear ImGui, other)? That decides how tokens and components are implemented.
8. Is "Extract outline" the last item in Shape, or are more controls below it?

---

## 11. Principle reference

| Principle | Where it applies in this spec |
|---|---|
| Nielsen N1 Visibility of status | Status bar, export progress, busy indicator, toasts |
| N2 Match the real world | Plain labels (Carve into the surface, Height source), Normal Y helper text |
| N3 User control and freedom | Undo/redo, Undo toasts, cancel export |
| N4 Consistency and standards | Opacity %, one blend control, one dropdown style, accent semantics |
| N5 Error prevention | Lock layers, disabled export with 0 maps, disabled Height source |
| N6 Recognition over recall | Labelled tools, tooltips, grouped icons |
| N7 Flexibility and efficiency | Shortcuts, keyboard reorder, double-click reset |
| N8 Aesthetic and minimalist design | Collapsible sections, helper text into tooltips, single accent |
| N9 Recover from errors | Memory warnings with advice |
| N10 Help and documentation | Info tooltips, shortcut cheat sheet |
| Shneiderman 1–8 | Consistency, shortcuts, feedback, closure (export toast), error handling, easy reversal, user control, low memory load |
| Fitts's law | Larger, closer targets for frequent actions |
| Hick's / Miller's law | Grouping chips and property sections |
| WCAG 2.2 | Section 7 checklist |

**Sources consulted:** Nielsen Norman Group's ten usability heuristics (summaries), Shneiderman's Eight Golden Rules (course notes), and WCAG 2.2 plain-language summaries for success criteria 1.4.3, 1.4.11, 2.4.7, 2.5.7 and 2.5.8. Check exact criterion wording against the W3C specification before citing in formal documents.

---

## Implementation status (2026-10-28)

Done: Phase 1 foundations; Phase 2 P0 consistency items (opacity in percent, eye/eye-off, slider ranges, one quiet row blend badge); Phase 3 export regrouping (Maps / Format / Normal Y / Height source / Size / Save to) with an "Export N maps" action, summary line and completion toast; Phase 4 measured right-tab labels with Ctrl+1-5 and a wider, grouped tool rail; Phase 5 delete/reset Undo toasts, busy status and an in-app shortcut sheet (`?`).

Not yet: export progress/cancel (export is synchronous), layer lock/solo, live layer thumbnails, a persisted layers/export split, Alt+Arrow reorder, axis/light gizmo, click-to-edit zoom, and the optional light theme.
