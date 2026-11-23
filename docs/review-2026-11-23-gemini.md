Кратко: разбор анимаций и интерфейса от Gemini (Antigravity, Neon) по видеозаписям сборки до правок 23.11. Часть уже исправлена — см. STATUS.md.

Source: Gemini 4 Neon via Antigravity CLI, 2026-11-23, on recordings of build 40f8d0a~1 (before zoom open / landing / hover fades).

### 01 / Per-Scenario Motion Review

#### `a_open_close` (small folder `Work` open/close)
- **`1.567s -> 1.600s` — 1-frame blank grey flash**: `Work` tile icons and label hard-cut to an opaque flat light-grey square (brightness jump vs dark glass tile).
- **`1.633s -> 1.733s` — 1:1 clipped content + late chrome**: Content renders at full 1:1 scale clipped by the expanding rect (right `Разное` column and bottom `Office` icons sliced); titlebar (`Work` + icons) absent until `1.733s`.
- **`1.733s -> 1.767s` — acrylic/contrast pop**: Flat opaque grey snaps to translucent acrylic near the end of expansion.
- **`3.000s -> 3.033s` — close header vanish + clipping**: Titlebar vanishes in 1 frame; full-size icons remain opaque and clip harshly as rect shrinks.
- **`3.100s -> 3.133s` — 1-frame blank grey box + tile pop**: Shrinks to an empty flat-grey tile-sized box for 1 frame (`3.100s`), then `Work` tile snaps back at `3.133s`.
- **Fix**: Crossfade tile snapshot out (`0–70 ms`, opacity `1 -> 0`) while scaling a single cached RGBA snapshot of the settled panel (`scale 0.92 -> 1.0`, opacity `0 -> 1`, `220 ms cubic-bezier(0.16, 1, 0.3, 1)`), keeping acrylic active from frame 0. Reverse on close (`150 ms cubic-bezier(0.4, 0, 1, 1)`), fading content out by `t = 90 ms` and crossfading tile back in during the final `60 ms`.

#### `a_open_big` (130-item folder `Big` open/close)
- **`1.533s -> 1.567s` — blank frame + center vertical seam**: `Big` tile vanishes into a flat-grey box with a visible light/dark vertical split down the middle.
- **`1.567s -> 1.667s` — flat opaque proxy + center-clipped content**: Zero acrylic blur for 4 frames (completely blocks green wallpaper glow); icons are center-clipped at 1:1 scale (confirmed in [shot_open_big.png](shot_open_big.png)), no titlebar, bottom rows missing behind an empty grey strip.
- **`1.667s -> 1.700s` — severe multi-property pop**: In 1 frame, acrylic snaps ON (green glow floods in), window jumps taller and shifts up to `y = 0`, header pops in, and rows 7–8 appear.
- **`1.700s -> 1.733s` — late height settle**: Window extends further down to unclip Row 9.
- **`3.100s -> 3.133s` — close acrylic/row pop-off**: Acrylic snaps OFF to flat grey; header and bottom 3 rows vanish instantaneously.
- **`3.200s -> 3.233s` — premature hard cut**: Cuts straight from a ~40%-size grey square back to collapsed tiles, skipping final fold into `Big` tile.
- **Fix**: Same cached-snapshot open/close as `a_open_close`; paint the full big-folder bitmap once offscreen before frame 0 (no progressive row/height pop), keep `12 px` minimum top/bottom screen margin, and complete the close trajectory all the way to tile bounds.

#### `a_switch` (open folder -> click another tile)
- **`1.400s -> 1.567s` — identical open defects**: Blank grey flash (`1.433s`), 3 flat-grey clipped frames (`1.467s–1.533s`), single-frame pop into acrylic + header + final rect (`1.567s`).
- **`1.567s -> 3.700s` — sibling occlusion blocks switching**: Open `Work` panel completely covers `Media` and clips `Big`'s `+123` counter; cursor clicks inside `Work` (triggering busy ring `2.400s–3.700s`), so no switch-to-second-folder transition occurs in the clip.
- **`3.700s -> 3.867s` — close slicing + double blank frame**: Flat grey pop-off (`3.733s`), red `64` icon sliced horizontally in half (`3.767s`), **two consecutive blank grey frames** (`3.800s`, `3.833s`), hard tile pop (`3.867s`).
- **Fix**: Position opened panel offset/anchored to avoid occluding adjacent tiles when space allows (or dim/shift strip); for folder-to-folder switching, run a direct morph (`200 ms cubic-bezier(0.22, 1, 0.36, 1)` rect interpolation + `90 ms` content crossfade) instead of close-then-open.

#### `a_tile_drag` (tile drag & grid glide)
- **`2.833s -> 2.900s` — drag #2 pickup lag**: 1-frame (~33 ms) stall after cursor moves (`2.867s`), then tile snaps to catch up (`2.900s`).
- **`3.100s -> 3.333s` — severe Y-coordinate oscillation + stalls**: Travel stalls (`3.100s–3.133s`), **teleports ~45 px down** in 1 frame (`3.167s`), freezes (`3.233s–3.267s`), **teleports ~35 px up** (`3.300s`), freezes again (`3.333s`).
- **`3.300s -> 3.467s` — wrong Z-order during drag**: Dragged `Media` tile slides **behind** stationary `Big` tile (also visible in [shot_tile_drag.png](shot_tile_drag.png)).
- **`1.333s -> 3.467s` — zero hover/pickup feedback**: No elevation shadow, subtle scale (`1.0 -> 1.03`), or border highlight while dragging.
- *Working well*: Release grid glides (`2.033s–2.133s`, `3.467s–3.567s`) are crisp ~100 ms ease-outs.
- **Fix**: Drive drag position strictly from screen-space `GetCursorPos() - grab_offset` (eliminates client-coord feedback Y-jumps); call `SetWindowPos(..., HWND_TOP)` on drag start; add `120 ms ease-out` lift (`scale 1.03`, stronger drop shadow, bright 1 px accent/white border) and slight `140 ms` release glide.

#### `a_tile_resize` (bottom-right corner resize of `Work`)
- **`1.300s -> 1.467s` — zero grip/cursor affordance**: Corner hover shows no grip reveal and cursor stays default arrow instead of `IDC_SIZENWSE`.
- **`1.667s`, `1.867s`, `1.900s` — 3 mid-drag 1-frame icon reflow pops**: Hard grid jumps `2x2 -> 3x3` (`1.667s`), `3x3 -> 4x3` (`1.867s`), and **double-flash 33 ms later** `4x3 -> 4x4` (`1.900s`); icons pin top-left leaving uneven dead margins between thresholds.
- **`2.133s -> 2.167s` — critical release snap + origin shift into `Media`**: On mouse-up, zero glide; **left edge jumps ~14 px left** closing the gutter to `0 px` against `Media`, right edge pulls left out from under the cursor, and icons pop `4x4 -> 5x3` leaving ~40% empty bottom space.
- **`2.967s -> 3.600s` — missed second grab leaks desktop marquee**: Because `2.167s` yanked the corner away from the cursor, the second drag draws a blue Windows desktop marquee behind `Work`'s glass.
- **`4.000s -> 4.033s` — blank tooltip flash**: Hover tooltip renders as an empty grey pill for 1 frame (`4.000s`) before `"Auto-Tune Artist Manual"` pops in (`4.033s`).
- **Fix**: Keep top-left `(x, y)` strictly anchored during resize and snap only `(w, h)` to grid cell multiples (preserving inter-tile gutter) with a `140 ms ease-out` settle glide; add hysteresis (`~12 px`) to grid column/row thresholds and animate icon slot positions (`150 ms ease-out`); show `IDC_SIZENWSE` + subtle corner arc grip on hover (`100 ms` fade); don't show tooltip window until text layout & first paint complete.

#### `a_group_move` (drag group plate inside `Work`)
- **`2.267s -> 2.400s` — zero header hover cue**: No hover fill, drag-grip dots, or cursor change over `Dev` header.
- **`2.433s` — instant window height jump on pickup**: Window snaps ~1 row taller in 1 frame with zero animation.
- **`2.467s -> 3.300s` — no drop preview + unreadable translucent overlap**: No ghost/target slot outline; translucent dragged `Dev` plate blurs unreadably over `Разное`/`Office` icons.
- **`2.900s -> 3.000s` — severe 3-frame window height spike + render seam**: Window snaps +2 extra rows taller at `2.900s` with a glaring horizontal border seam at the old bottom edge (Win32/Slint resize desync), holds 3 frames, then snaps back up at `3.000s`.
- **`3.333s` — premature window shrink clips dragged group**: On release, window snaps back to 4-row height in 1 frame while `Dev` is still at bottom-right, clipping `Dev`'s bottom edge before it glides up to top-right (`3.333s–3.467s`).
- *Working well*: `Разное` reflow slide (`2.900s–3.000s`) and `Dev` drop glide (`3.333s–3.467s`) animate smoothly.
- **Fix**: Elevate dragged group surface opacity (`~0.88` dark frosted card + `16 px` shadow) and draw a subtle dashed/tinted drop-slot preview (`120 ms` fade); debounce drag-time window height changes (`120 ms`), keep window height locked until the `160 ms ease-out` drop glide finishes, then animate window height smoothly (`180 ms ease-out`).

#### `a_group_resize` (corner grip resize inside `Work`)
- **`2.300s -> 3.300s` — missed grip (no resize occurred)**: Because earlier `a_group_move` swapped `Разное`/`Dev`, the synthetic drag traverses empty gutter below `Dev` with zero reaction. Confirms a real UX issue: **group corner grips are completely invisible at rest**.
- **`4.200s -> 4.300s` — close titlebar vanish + clipping**: Desktop marquee dismisses `Work`; titlebar vanishes on frame 1 (`4.200s`) and body clips while shrinking.
- **Fix**: Reveal a crisp `12x12` corner resize indicator on group-plate hover (`100 ms` fade) with a generous `14x14 px` hitbox and `IDC_SIZENWSE` cursor; apply the same `150 ms ease-out` icon reflow and animated window height sync when resizing groups.

#### `a_item_drag` (drag shortcuts across groups in `Work`)
- **`2.467s`, `4.000s` — instant window rect jump on pickup**: Window snaps ~45 px wider and ~90 px taller in 1 frame, exposing an unlabeled empty grey band; source slot turns into a blank hole with zero pickup lift, flat ghost (no shadow/scale), and 0 ms cyan group-border snap.
- **`2.567s`, `2.833s`, `4.233s`, `4.267s` — sibling icons teleport (0 ms)**: Every reorder and cross-group shift teleports sibling icons in 1 frame with zero slide easing; group target border snaps `off -> on` across the gutter (`2.767s -> 2.800s`).
- **`4.233s` — contradictory auto-fold during hover**: Dragging a 7th item over 6-item `Office` instantly folds `64` and `Anki` behind a `+2` tile; on drop (`4.700s`) `Office` instantly unfolds to 3 full rows anyway.
- **`4.700s -> 4.733s` — 1-frame empty icon + Win32 L-shaped overhang**: On drop (`4.700s`), the dropped slot `Office (0,1)` renders as a blank dark square (icon missing for 1 frame) while Slint shrinks its layout 1 frame before the Win32 acrylic host window resizes, leaving a flat grey L-shaped border artifact along right/bottom edges until `4.733s`.
- **Fix**: Animate sibling slot positions on reorder (`140 ms cubic-bezier(0.2, 0.9, 0.3, 1)`), fade group highlight borders (`100 ms`), show a subtle `"New group"` dashed drop zone in the expanded drag margin (and animate margin expansion over `160 ms`), suppress auto-folding during drag-over (expand row instead), keep the dropped icon painted immediately on drop, and commit `SetWindowPos` atomically with the resized framebuffer.

#### `a_unfold` (`+11` unfold in `Big`)
- **`1.433s -> 1.600s` — same folder-open blank flash + clip + late titlebar**.
- **`2.433s -> 2.500s` — minimal hover feedback on `+11`**.
- **`2.500s -> 2.533s` — Win32 window height SNAPS to screen bottom in 1 frame**: Before `Apps` finishes expanding, window height instantly jumps ~140 px down to touch the bottom edge of the screen, exposing a huge empty frosted slab below `Разное`. `+11` vanishes and item #12 pops in at `2.533s`.
- **`2.533s -> 2.667s` — hard bottom-edge curtain wipe**: While `Apps` height and lower groups (`System`, `Разное`) slide down smoothly (~133 ms), newly revealed rows clip sharply at the moving bottom edge (icons sliced in half across `2.567s–2.633s`) with zero fade-in.
- **`2.667s -> 5.967s` — no fold-back triggered**: Collapse control (`^ свернуть`) appears far away at bottom-left Row 6 Col 0 instead of near the original `+11` position or group header, so synthetic click misses it.
- **Fix**: Step window height in lockstep with `Apps` expansion (`200 ms cubic-bezier(0.22, 1, 0.36, 1)`) clamped to `work_area_height - 24 px` with smooth internal scroll if exceeded; fade newly revealed rows (`opacity 0 -> 1` + `translateY -8px -> 0`, `140 ms` with `20 ms` per-row stagger) so icons aren't sliced bare; place collapse control in the group header (`Apps  ^`) so it stays reachable.

#### `a_settings` (settings page & back)
- **`1.433s -> 1.600s` — same `Work` open blank flash + clip + late header pop**.
- **`2.333s -> 4.967s` — settings page never opens**: Cursor stops ~15–20 px below the thin titlebar (see [shot_settings.png](shot_settings.png)); zero hover or click response. Exposes undersized titlebar hit targets (~24 px bar, ~12 px glyphs).
- **Fix (for settings navigation + chrome)**: Enlarge titlebar to `36–40 px` with `32x32 px` Fluent icon buttons (`100 ms` hover pill); transition folder <-> settings via `180 ms` subtle horizontal drill/crossfade (`opacity 1 -> 0` + `translateX -12px` / incoming `+12px -> 0`) with animated window height if page heights differ.

#### `a_hint` (hover tile icons -> name hint)
- **`1.300s`, `2.033s`, `2.533s`, `3.033s`, `3.533s` — 0 ms hover snap**: Tile and per-icon hover highlights snap on/off in 1 frame.
- **`1.733s`, `2.433s`, `3.433s` — 0 ms tooltip pop-in/out**: `MultiRack`, `MultiRack SoundGrid`, `Auto-Tune Pro VST3_ARA Read Me` snap to 100% opacity in 1 frame and vanish in 1 frame (`2.033s`, `2.533s`, `3.533s`).
- **`2.033s -> 2.433s` — full 400 ms delay resets on sibling icon**: Scrubbing from icon 1 to icon 2 inside `Big` kills the tooltip and restarts the 400 ms timer.
- **Fix**: Fade hover highlights (`90 ms ease-out`); animate tooltip in (`110 ms ease-out`, `opacity 0 -> 1`, `translateY 4px -> 0`) and out (`80 ms ease-in`); keep a `250 ms` warm-hover grace window so moving to an adjacent icon updates text/position smoothly (`100 ms`) with zero delay.

---

### 02 / Static UI Review (vs. Windows 11 Fluent Design)

Reviewed from [shot_open_close.png](shot_open_close.png), [shot_settings.png](shot_settings.png), [shot_unfold.png](shot_unfold.png), [shot_hint.png](shot_hint.png), [shot_tile_drag.png](shot_tile_drag.png), and settled storyboard frames.

1. **Contrast failure on bright wallpaper bleed (`shot_unfold.png`, `a_open_big_03`)**: Bright green wallpaper glows straight through the acrylic backdrop behind `System` and `Разное`, turning muted grey shortcut labels (`~#9A9A9A`) nearly unreadable. Fluent dark acrylic needs higher luminosity/dark-tint opacity (`~0.72–0.78` dark tint or subtle dark scrim behind labels) plus primary text at `rgba(255,255,255,0.92)`.
2. **Flat visual hierarchy in headers (`shot_open_close.png`, `shot_unfold.png`)**: Window title (`Work`, `Big`) uses almost the same muted weight/style and leading hollow circle (`○`) as group sub-headers (`Dev`, `Office`, `Apps`). Those hollow circles read like unchecked radio buttons. Make window title `15–16 px SemiBold #F5F5F5` without a hollow circle (or use a filled accent dot/folder glyph); keep group headers `12 px SemiBold rgba(255,255,255,0.72)`.
3. **Tiny, cramped titlebar controls (`shot_settings.png`, `shot_unfold.png`)**: Gear, pin, and close icons are thin ~12 px dim glyphs packed into a ~26 px bar. Fluent requires `32x32 px` button hit targets (with `4–6 px` rounded hover pills) inside a `36–40 px` header bar.
4. **Massive dead voids in 2-column masonry (`shot_open_close.png`, `shot_unfold.png`)**: Unbalanced column heights leave 35–45% of the bottom-right window as an empty acrylic slab (worst when `Apps` unfolds in `shot_unfold.png`). Also `Big` slams flush against `y = 0` and bottom screen edge with `0 px` margin. Re-balance columns or wrap groups cleanly, and enforce `12–16 px` outer screen work-area margin + internal scrolling when taller than viewport.
5. **Aggressive 1-line label truncation + tight hover pill (`shot_unfold.png`)**: Single-line ellipsis produces indistinguishable siblings (`Auto-Tune ...`, `Application ...`). Allow 2-line centered wrapping (`11 px`, line-height `14 px`, max 2 lines) with `6 px` icon-to-label gap. In `shot_unfold.png`, the hover box on `Oracle Virtu...` nearly touches the text descenders (`~2 px` bottom padding vs `6 px` top)—balance vertical padding (`6 px` top/bottom).
6. **Weak group plate differentiation & invisible grips (`shot_open_close.png`)**: Plates rely on thin uniform grey 1 px wireframes on nearly identical glass. Use Fluent dark card surface (`rgba(255,255,255,0.045)` fill + `rgba(255,255,255,0.07)` 1 px hairline, `8 px` radius inside `12 px` outer window with `12 px` outer padding and `8 px` gutters). Replace the dead-pixel 5x5 square tile corner dot (`shot_hint.png`) with a subtle corner arc/diagonal grip revealed on hover.
7. **Floating `+N` counts & orphan collapse slot (`shot_hint.png`, `shot_unfold.png`)**: On tiles, `+123`/`+4`/`+12` float as bare muted text in an empty 4th quadrant—give them a subtle frosted squircle pill (`rgba(255,255,255,0.08)`). In unfolded `Apps` (`shot_unfold.png`), `^ свернуть` consumes an entire bottom row as a lone orphan slot; move collapse to the `Apps` group header bar.
8. **Tile padding, concentric radii, and tooltip placement (`shot_hint.png`, `a_hint_03`, `a_hint_05`)**:
   - Tile label (`Big`, `Media`, `Work`) is cramped `~3 px` from the bottom border; top-left icon hover box (`~4 px` radius) clashes against the tile's `~12 px` outer corner (use concentric `8 px` inner radius + `8 px` inset).
   - Hover tooltips render **over row 2 of the tile** and spill across all 3 tiles on long names (`Auto-Tune Pro VST3_ARA Read Me`). Float tooltips cleanly **below (or above) the tile** with `8 px` offset, `max-width: 240 px` ellipsis, `8 px 10 px` padding, and soft elevation shadow.
9. **Weak window-over-tile depth separation (`shot_open_close.png`)**: Open `Work` window overlapping `Big` tile lacks a strong elevation drop shadow, visually fusing the two dark glass surfaces.

---

### 03 / Ranked Top-10 Fixes (by Perceived Quality Impact)

| Rank | Problem | Concrete Fix (Timing, Easing, Software-Renderer / Win32 Note) |
| :--- | :--- | :--- |
| **1** | **Broken open/close transition** (`a_open_close`, `a_open_big`, `a_switch`): blank grey flash frames, acrylic popping on/off, 1:1 sliced icons, late header/rows, premature close cut. | Render settled panel once into an offscreen RGBA bitmap before frame 0. Keep Win32 acrylic active throughout. Open: animate rect (`220 ms cubic-bezier(0.16, 1, 0.3, 1)`), fade tile bitmap out (`0–70 ms`), scale+fade panel bitmap in (`scale 0.92 -> 1.0`, `opacity 0 -> 1`, `40–200 ms`). Close: reverse (`150 ms cubic-bezier(0.4, 0, 1, 1)`), fade content out by `90 ms`, fade tile in `90–150 ms`, finish full collapse to tile rect. Avoids per-frame full Slint relayout. |
| **2** | **Win32 window resize desync & 1-frame snaps** (`a_group_move` `2.900s` seam & `3.333s` clip, `a_item_drag` `4.700s` L-shaped overhang, `a_unfold` `2.533s` bottom slab). | Synchronize framebuffer paint with Win32 resize (render new size before committing `SetWindowPos` / `WM_WINDOWPOSCHANGING`). Animate window height in lockstep with unfold/reflow (`200 ms cubic-bezier(0.22, 1, 0.36, 1)`). During drag, debounce height growth (`120 ms`) and defer window shrink until the `160 ms` drop glide finishes. |
| **3** | **Teleporting shortcuts & missing drag feedback** (`a_item_drag` `2.567s`, `2.833s`, `4.233s`, `4.700s`). | Animate sibling slot positions on reorder/transfer (`140 ms cubic-bezier(0.2, 0.9, 0.3, 1)`). Add elevated drag ghost (`scale 1.06`, shadow), soft slot placeholder outline, `100 ms` target-group border fade, and labeled `"New group"` drop zone. Never auto-fold a target group during drag-over, and never render an empty slot on the drop frame. |
| **4** | **Tile resize release jump + desktop bleed + reflow pops** (`a_tile_resize` `1.667s–2.167s`, `2.967s–3.600s`). | Lock top-left `(x, y)` on resize; snap only `(w, h)` to grid multiples preserving inter-tile gutters with a `140 ms ease-out` settle glide. Add `12 px` threshold hysteresis + `150 ms ease-out` icon position glide to eliminate double-flash reflow pops (`1.867s -> 1.900s`). |
| **5** | **Tile drag Z-order bug & Y-axis oscillation** (`a_tile_drag` `3.100s–3.467s`, `shot_tile_drag.png`). | Raise dragged tile via `SetWindowPos(hwnd, HWND_TOP)` on drag start. Compute drag rect purely from screen-space `GetCursorPos() - grab_offset` to eliminate Y-jump feedback loops. Add `120 ms ease-out` pickup lift (`scale 1.03`, shadow, subtle border brighten). |
| **6** | **Glass contrast & label legibility** (`shot_unfold.png`, `shot_open_close.png`). | Increase dark acrylic tint/luminosity opacity (`~0.75` dark backing) so bright wallpapers don't wash out text. Upgrade shortcut labels to `rgba(255,255,255,0.90)` with 2-line wrap (`11 px`, `14 px` leading), balanced `6 px` hover-pill vertical padding, and stronger window drop shadow over overlapping tiles. |
| **7** | **Unfold curtain-wipe slicing, dead void & screen clipping** (`a_unfold` `2.533s–2.667s`, `shot_unfold.png`). | Reveal unfolded rows with `opacity 0 -> 1` + `translateY -8px -> 0` (`140 ms ease-out`, `20 ms`/row stagger) instead of bare clipping. Clamp window to work area (`12 px` margin) with smooth scrolling; move `Collapse` button into the group header bar and tighten multi-column packing to eliminate the 40% bottom-right empty void. |
| **8** | **Zero-easing micro-interactions & mispositioned hover tooltips** (`a_hint`, `a_tile_resize` `4.000s`). | Anchor tile tooltips **outside/below** the tile (`8 px` gap, `max-width 240 px`, elevation shadow), never over row-2 icons or neighbor tiles. Animate in (`110 ms ease-out`, fade + `4 px` slide) and out (`80 ms ease-in`), paint text before first show (fixes `4.000s` blank pill), add `250 ms` warm-hover grace across sibling icons, and fade all hover states (`90 ms ease-out`). |
| **9** | **Header hierarchy, tiny hit targets & invisible resize grips** (`shot_settings.png`, `a_settings`, `a_group_resize`). | Raise titlebar height to `36–40 px`, style title as `15 px SemiBold #F5F5F5` (drop hollow `○` radio-like bullets), give gear/pin/close full `32x32 px` Fluent hover targets. Reveal crisp `12x12` corner resize arcs (`100 ms` fade) + `IDC_SIZENWSE` cursor on tile/group hover (replacing the 5x5 square dot). |
| **10** | **Group-drag clarity & tile polish** (`a_group_move`, `shot_hint.png`). | Increase dragged group card opacity (`~0.88`) + shadow and show a `120 ms` tinted drop-target preview so users see if a 3-col group fits before release. On desktop tiles, put `+N` in a subtle frosted squircle pill, fix concentric inner hover radius (`8 px` inside `12 px`), and add `6–8 px` bottom padding under tile names. |
