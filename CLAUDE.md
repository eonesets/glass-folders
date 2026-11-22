Кратко: карта проекта для Claude — где что лежит в коде, как собрать и проверить. Читать это вместо всего кода; детали истории — в STATUS.md.

# glass-folders — code map

Windows desktop app: each Desktop folder -> glass tile on the desktop; click -> panel with groups (= subfolders).
Rust + Slint 1.x, software renderer, own Win32 platform layer. State/progress: STATUS.md. Spec: docs/SPEC.md.
Rule: when files / key functions / props change, update this map in the same commit. Read only the code the task touches.

## Files (app/)
- `ui/app.slint` — ALL UI. Only draws; geometry comes from Rust.
  - Small controls on top: Dot, CapButton, Switch, Seg, Card, RowText, SwitchRow, Hairline, SectionTitle.
  - `Slot` — one shortcut cell 76px (also "+N" when more>0, "свернуть" when more<0).
  - `Grip` — edge strip of a group plate = move handle.
  - `Panel` — folder window. page 0 = groups, page 1 = settings. Props from Rust: groups, grid-w/h, settings
    (labels names dots ptitle tnames gravity glass snap), drop-target/index, ghost-*, lift-* (dragged group, free px).
    Callbacks -> Rust: launch, open-group (unfold/fold "+N"), item-drag/drop, group-drag/drop, set-opt(key,int), rename-group, quit.
    Header: gear, pin (toggle-pin, `pinned`), X. `hover(id, rect)` -> name hint. `isize` = icon size seg.
  - `Hint` — name hint window (text, pw = width for Rust).
  - `Tile` — desktop tile. `named` = name strip (else square 48px cells). icons = most launched first;
    icon click -> launch(i); last cell "+N" (total > cells), empty space -> open.
- `src/main.rs` — app glue. `Global` via `g()` (settings, tiles, tile_items, panels, hint): app-wide.
  `App` = state of ONE folder window (folder, base/prev rects, ghost, target, lift, unfolded, anim, pinned).
  - panels: make_panel wires a window; panels[0] = the one tiles open; set_pin / pin_open (cfg "pin"); finish_close drops
    non-[0] windows. hint_at / hide_hint. set_opt applies to all panels (isize -> plat::set_zoom, icon_scale).
  - startup: make_tiles -> desk::hide_folders. quit/panic -> give_back_icons.
  - set_opt (settings page), push_settings.
  - drag: item_drag / group_drag (preview, uses plat::cursor_logical for groups) -> show_preview; drop_item / drop_group (fs moves, cfg save) -> rescan -> render.
  - tiles: tile_icons (top_items -> icons, returns paths) -> App.tile_items; refresh_tile after launch / rescan.
    Launch (panel or tile) -> model::bump_use.
  - render: model::layout + unfold, updates existing VecModel rows in place (plates animate).
  - open_folder / animate / close / finish_close: panel grows out of tile and folds back (plat::set_view).
- `src/model.rs` — data, no UI. Folder scan, per-folder cfg `.glassfolder.json` (tile rect, group x/y/w/h/color/items order),
  launch counts `uses` (top_items, bump_use), app Settings (%APPDATA%/glass-folders/settings.json), panel grid: GRect (cols 84px PITCH, half-rows 42px ROW,
  h = 2*rows + header), resolve (push overlaps down), settle (gravity), layout, grid_px. Unit tests at bottom.
- `src/plat.rs` — Slint platform on raw Win32: window per Slint window, DIB paint, mouse/keys, tile drag/resize
  (free follow, glide to cells on release), tile cell math (tile_cells/tile_size/snap_pos, TILE_NAMED, SNAP),
  set_view (show part of panel = open animation), cursor_logical. Kind Panel/Tile/Hint; set_zoom (scale = dpi*zoom);
  set_pinned (pinned panel: bottom z, live glass; WM_TILES_DOWN keeps tiles under it).
- `src/desk.rs` — Explorer desktop: icon positions (IShellWindows/IFolderView), hide tiled folders via
  FILE_ATTRIBUTE_HIDDEN, restore + put icons back to exact spots.
- `src/win.rs` — misc Win32: acrylic glass, dark mode, work area, shell icons, ShellExecute open, wallpaper blur fallback.

## Build / run / check
- Build: `cd app; cargo build --release`. Running exe locks the file: `taskkill //IM glass-folders.exe //F` first
  (killed run is safe: hidden folders restored on next start from settings "homes").
- Tests: `cargo test --release` (model layout/gravity).
- Run: `app/target/release/glass-folders.exe` (dev: `--open NAME`, env GF_STAY=1 keep panel open, GF_KEEP_ICONS=1,
  GF_TILE_AT=x,y, GF_FAKE_GLASS=1).
- Visual check: screenshots + synthetic mouse via PowerShell (SetProcessDPIAware, CopyFromScreen, SetCursorPos/mouse_event).
  Screen 2560x1440, scale 1. Careful: drags in the panel move REAL files (user's Desktop folders).

## Gotchas
- Never rebuild the Slint groups model inside a slot's own event handler: defer with Timer::single_shot(ZERO).
- Drag math: pointer pos relative to an element that moves with the drag drifts -> use real cursor (plat::cursor_logical).
- Explorer clamps icon moves to the screen -> folders are hidden by attribute, not moved away.
- Panel size formula duplicated: app.slint (pad 16, top 48) and main.rs open_folder / fit_panel (+32, +64).
- Bash heredocs with Cyrillic/backslashes break -> write patch scripts with the Write tool.
