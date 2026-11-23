Кратко: карта проекта для Claude — где что лежит в коде, как собрать и проверить. Читать это вместо всего кода; детали истории — в STATUS.md.

# glass-folders — code map

Windows desktop app: each Desktop folder -> glass tile on the desktop; click -> panel with groups (= subfolders).
Rust + Slint 1.x, software renderer, own Win32 platform layer. State/progress: STATUS.md. Spec: docs/SPEC.md.
Rule: when files / key functions / props change, update this map in the same commit. Read only the code the task touches.

## Repo (public, github.com/eonesets/glass-folders)
- README.md (EN) + README.ru.md, LICENSE (MIT), docs/media/*.gif = README demos. promo/ is gitignored (local social drafts).
- Release: zip of glass-folders.exe + LICENSE.txt -> `gh release create vX.Y.Z`. Bump app/Cargo.toml version.

## Files (app/)
- `ui/app.slint` — ALL UI. Only draws; geometry comes from Rust.
  - Small controls on top: Dot, CapButton, Switch, Seg, Card, RowText, SwitchRow, Hairline, SectionTitle.
  - `Slot` — one shortcut cell 76px (also "+N" when more>0, "свернуть" when more<0).
  - `Grip` — edge strip of a group plate = move handle.
  - `Panel` — folder window. page 0 = groups, page 1 = settings. Props from Rust: groups, grid-w/h, settings
    (labels names dots ptitle tnames gravity glass snap), drop-target/index, ghost-*, lift-* (dragged group, free px).
    grow-anim = window size glides (off while opening). land-* = released shortcut glides cursor -> slot (Rust main.rs land()).
    Callbacks -> Rust: launch, open-group (unfold/fold "+N"), item-drag/drop, group-drag/drop, set-opt(key,int), rename-group, quit.
    Header: gear, pin (toggle-pin, `pinned`), X. `hover(id, rect)` -> name hint. `isize` = icon size seg.
    `en` = UI language (all strings inline `en ? "EN" : "RU"`; Slot gets it too). Settings page "lang" seg. Loose group shown as Misc/Разное via `g.fixed`.
  - `Hint` — name hint window (text, pw = width for Rust). Frameless popup (plat set_kind Hint strips thick frame: framed
    windows stay >= ~39px tall -> empty strip under the label), no backdrop (win::clear).
  - `Ask` — rename box (TextInput, done(ok, text)). `Fly` — icon following a shortcut dragged out of its window (Kind Hint).
  - Slot / Tile icon right click -> item-menu(id / index). Tile `drop-hot` = shortcut from another folder is over it.
  - Panel min-w 168px on the folder page: window hugs its groups (no 3-col min), header buttons slide left.
  - `Tile` — desktop tile. `named` = name strip (else square 48px cells). icons = most launched first;
    icon click -> launch(i); last cell "+N" (total > cells), empty space -> open.
- `src/main.rs` — app glue. `Global` via `g()` (settings, tiles, tile_items, panels, hint): app-wide.
  `App` = state of ONE folder window (folder, base/prev rects, ghost, target, lift, unfolded, anim, pinned).
  - panels: make_panel wires a window; panels[0] = the one tiles open; set_pin / pin_open (cfg "pin"); finish_close drops
    non-[0] windows. hint_at / hide_hint. set_opt applies to all panels (isize -> plat::set_zoom, icon_scale).
  - startup: make_tiles -> desk::hide_folders. quit/panic -> give_back_icons.
  - set_opt (settings page), push_settings.
  - drag: item_drag / group_drag (preview, uses plat::cursor_logical for groups) -> show_preview; drop_item / drop_group (fs moves, cfg save) -> rescan -> render.
    Out of the window: outside_target (other tile / other pinned panel / plat::desktop_at) -> App.out, set_hot, fly_at;
    release -> drop_out (move_into, uses follow, Desktop: desk::place_icon at drop point, retried until Explorer lists it).
  - shortcut menu: item_menu(Owner Panel|Tile) -> win::popup_menu (native, themed) -> open / ask_rename / shell::pick_icon /
    shell::recycle / shell::uninstaller+confirm+run_uninstall (none found -> Windows apps settings). g().modal keeps the panel
    open while a menu / dialog is up. model::rekey_item moves launch count + group order to the new name.
  - tiles: tile_icons (top_items -> icons, returns paths) -> App.tile_items; refresh_tile after launch / rescan.
    Launch (panel or tile) -> model::bump_use.
  - render: model::layout + unfold, updates existing VecModel rows in place (plates animate).
  - open_folder / animate / close / finish_close: panel grows out of tile and folds back (plat::set_view).
- `src/icons.rs` — shell icons on 4 worker threads + cache (path, px) -> Image. get() = cache only, want() queues
  (urgent first), on_ready -> main.rs icons_ready patches panel slots / waiting tiles in place. prefetch_icons at start.
- `src/model.rs` — data, no UI. Folder scan, per-folder cfg `.glassfolder.json` (tile rect, group x/y/w/h/color/items order),
  launch counts `uses` (top_items, bump_use), app Settings (lang 0 ru / 1 en, default = Windows UI lang) (%APPDATA%/glass-folders/settings.json), panel grid: GRect (cols 84px PITCH, half-rows 42px ROW,
  h = 2*rows + header), resolve (push overlaps down), settle (gravity), layout, grid_px. Unit tests at bottom.
- `src/plat.rs` — Slint platform on raw Win32: window per Slint window, DIB paint, mouse/keys, tile drag/resize
  (free follow, glide to cells on release), tile cell math (tile_cells/tile_size/snap_pos, TILE_NAMED, SNAP),
  set_view (show part of panel = open animation), cursor_logical. Kind Panel/Tile/Hint; set_zoom (scale = dpi*zoom);
  set_pinned (pinned panel: bottom z, live glass; WM_TILES_DOWN keeps tiles under it).
  Event loop: while animating, one pass per screen refresh (DwmFlush); animation timers use plat::FRAME.
  WM_GETMINMAXINFO min 1x1 (else Windows clamps small windows: hint strip, open anim start size). GF_PERF=file = timing log.
  Mouse path: wndproc -> GfWindow::mouse_move/down/up (shared with scripts). cursor_pos() = real cursor, or virtual in script mode.
  Test hooks (bottom): set_offscreen (layered alpha 0 + click-through + no activate), inject (Ptr event -> dispatch_event),
  shot (render DIB -> PNG), record/take_frames (every drawn frame, shown part only), save_png.
- `src/script.rs` — headless scenario runner (env GF_SCRIPT). Command list in its header. Calls main.rs open_folder/close,
  plat::inject/shot/record. Ends app with exit code 0/1, writes OUT/log.txt.
- `src/desk.rs` — Explorer desktop: icon positions (IShellWindows/IFolderView), hide tiled folders via
  FILE_ATTRIBUTE_HIDDEN, restore + put icons back to exact spots.
- `src/win.rs` — misc Win32: acrylic glass, clear (frameless alpha popup), popup_menu, dark mode, work area, shell icons,
  ShellExecute open, wallpaper blur fallback.
- `src/shell.rs` — shortcut actions: recycle (IFileOperation, undo-able), rename (keeps ext), pick_icon (PickIconDlg, .lnk/.url),
  uninstaller (registry Uninstall keys: DisplayIcon == target, else longest InstallLocation; never Windows dir / nameless keys),
  run_uninstall, confirm. Dev: `glass-folders.exe --uninstall-info OUT FILE...` writes matches, runs nothing.

## Build / run / check
- Build: `cd app; cargo build --release`. Running exe locks the file: stop THAT exe first (killed run is safe:
  hidden folders restored on next start from settings "homes").
- Tests: `cargo test --release` (model layout/gravity).
- Run: `app/target/release/glass-folders.exe` (dev: `--open NAME`, env GF_STAY=1 keep panel open, GF_KEEP_ICONS=1,
  GF_TILE_AT=x,y, GF_FAKE_GLASS=1, GF_DESKTOP=dir = demo folder instead of real Desktop).
- UI tests (default, safe while user works): `powershell -File tests/ui/run.ps1 [name ...]` -> PASS/FAIL per
  `tests/ui/scenarios/*.gfs`, PNGs + log in tests/ui/out/<name>/ (gitignored). Fresh demo desktop + APPDATA in %TEMP%.
  GF_SCRIPT implies GF_OFFSCREEN=1 (windows invisible, click-through, never focused) + virtual cursor + no desktop icon hiding.
  Coords in scripts = logical px of window content; find them from a `shot` first. `#settings {json}` line = settings for that run.
  Not covered headless: DWM acrylic look, real timing on screen, Explorer icon hide/restore.
- Real-screen check (rare): synthetic mouse via PowerShell (SetCursorPos/mouse_event, CopyFromScreen) hijacks the user's PC.
  Such scripts MUST start with `& tests/ui/idle.ps1 -Require 120; if ($LASTEXITCODE) { exit 1 }` (abort if user active).
  Screen 2560x1440, scale 1. Careful: drags in the panel move REAL files (user's Desktop folders).
- Never `taskkill /IM glass-folders.exe` blindly: it kills the user's running copy too (their folders stay hidden until restart).
  Kill by path, or not at all (worktree exe is a different file, no lock).

## Gotchas
- Never rebuild the Slint groups model inside a slot's own event handler: defer with Timer::single_shot(ZERO).
- Drag math: pointer pos relative to an element that moves with the drag drifts -> use real cursor (plat::cursor_logical).
- Explorer clamps icon moves to the screen -> folders are hidden by attribute, not moved away.
- Panel size formula duplicated: app.slint (pad 16, top 48) and main.rs open_folder / fit_panel (+32, +64).
- Bash heredocs with Cyrillic/backslashes break -> write patch scripts with the Write tool.
