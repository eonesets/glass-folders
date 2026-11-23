Кратко: приложение «стеклянные папки» для рабочего стола. Готовим 1.0: большие папки открываются мгновенно, анимации плавные, окно папки по размеру иконок, меню по ПКМ (переименовать, значок, удалить, удалить программу), ярлык можно вытащить на стол или в другую папку. Ветка claude/animations-performance-v1-3ed962, не слита.
Ждём: проверка пользователем на своём столе.

# glass-folders — STATUS
Updated: 2026-11-23 (1.0: hug-size panel, hint fix, shortcut menu, drag out; headless UI tests)
Progress: 88%

## Done
- Desktop shortcuts audited and cleaned (see docs/SPEC.md).
- Requirements gathered (docs/SPEC.md). Mockups v1-v3 (docs/mockups.html).
- Folder-based model agreed (widget = real folder, groups = subfolders).
- Toolchain: rustup (stable 1.100, msvc) + VS 2022 Build Tools via winget.
- Prototype v0.1 in app/ (Rust + Slint, own Win32 platform layer src/plat.rs):
  - Tiles: one per Desktop folder, 2x2 icon preview, bottom z-order, no focus, frosted wallpaper backdrop.
  - Panel: centered, real Win11 acrylic (verified with transparency ON), groups = subfolders + "Разное",
    square 76px slots, shell icons, "+N" overflow, min width 3 cells, single group fills width.
  - Color dot -> 10-color popover, saved to hidden .glassfolder.json, tile tint updates.
  - Labels toggle, Esc / focus loss closes, click launches via ShellExecute.
  - RAM: private ~6 MB, working set ~28 MB, 0 CPU idle. Exe 9.5 MB.

- v0.2 (2026-11-22):
  - Colors: folder + groups colorless by default; picker has "no color" swatch; tile name centered.
  - Real live acrylic on tiles: WM_NCACTIVATE always-TRUE + DWMWA_CAPTION_COLOR none (spike verified on 26200).
    Old wallpaper-blur kept as fallback: env GF_FAKE_GLASS=1.
  - Glass level setting (Плотное/Среднее/Прозрачное) = matte layer over acrylic, panel + tiles.
  - Panel header: Windows-style red X (closes folder) + gear menu (labels toggle, glass level, quit app).
  - Panel draggable by header. Tiles: drag to move, bottom-right grip to resize, icon grid adapts;
    rect saved per folder in .glassfolder.json "tile". App settings: %APPDATA%/glass-folders/settings.json.

- v0.3 (T1, T2): tiles resize/move snap to icon-cell grid; panel top strip (DWM caption band) removed.
- v0.3 (T3): drag shortcuts in panel between groups / to new group (real fs move), inline group rename (new group + double-click title), keyboard text input in panel.

- v0.4 (2026-11-22):
  - Desktop: native icon layer hidden on start (ShowWindow SW_HIDE on desktop SysListView32, src/desk.rs), shown on quit /
    panic / after a killed run (next start sees hidden view + HideIcons=0 -> owns it). Icon positions never touched.
  - First tile place = where Explorer has the folder icon (IShellWindows -> desktop IFolderView::GetItemPosition),
    snapped, no overlap with other tiles; saved to "tile" at once. Dev: GF_KEEP_ICONS=1, GF_DUMP_ICONS=file.
  - Panel = free grid (cols 84px, half-rows 42px; group of r rows = 2r+1). Group place/size in cfg groups.<name>.x/y/w/h.
    Overlaps -> groups pushed down (model::resolve). Unplaced groups -> first free spot.
  - Drag group header = move, corner grip = resize (whole slots); others make room live, animated.
  - Shortcut drag: reorder in group (order in cfg groups.<name>.items), into other group at chosen place,
    empty space -> "Новая группа" plate right under cursor (groups below make room), drop = real folder there + rename.
  - "+N" overflow computed in Slint (all items in model) -> resize shows more/less at once.
  - Verified on Desktop\GF_TEST with synthetic mouse (reorder, new group at spot, move, resize, cross-group, quit restores icons).

- v0.5 (2026-11-22):
  - Desktop: native icon layer stays visible (rubber-band select, loose shortcuts, menus work). Only tiled folders hidden:
    FILE_ATTRIBUTE_HIDDEN while running (off-screen icon moves tried: Explorer clamps them to the screen edge).
    Home spots saved in settings.json "homes"; quit / panic / next start after a kill -> unhide + icons back to exact spots.
  - Settings page in panel (gear): labels, group names, group color dots, gravity (none / top-left / top-right),
    glass level, tiles snap to grid, quit. Win11-like switches + sliding segmented controls. Panel grows while shown.
  - Names off -> dot only; dots off too -> plain plates, slots centered. Group color also by right click on plate.
  - Gravity: model::settle (up, then sideways, until stable) in layout + live drag previews. Unit tests in model.rs.
  - Panel opens out of its tile (window rect grows tile -> panel, content fades in, 230 ms) and folds back on close;
    placed centered on the tile. plat::set_view = show part of content without relayout.
  - Tiles: free move/resize under cursor, on release short glide to whole cells (+ grid if "по сетке" on).
    Tile z-order forced only on real z changes (cheaper drag).

- v0.6 (2026-11-22):
  - Settings split: "Папка" (name in panel header = ptitle, name under tile = tnames, snap) / "Группы внутри"
    (names, dots, labels, gravity on own line -> no overlap on narrowest panel) / glass. SwitchRow component.
  - Tile without name: square cells (row pitch 48, pad 12; plat::tile_v). Toggle keeps cell count, resizes tiles.
  - Groups without title+dot: no header row (model::set_header -> GRect h = 2r), plate hugs slots.
    Plate edges (6 px strips, move cursor) = drag handles, always (only ones without header).
  - "+N" click unfolds the group in place (all items + "свернуть" slot, wider if > 6 rows), others make room;
    panel kept inside work area (fit_panel). Folded again by the slot or on close.
  - Group move/resize: plate follows cursor freely (lift-x/y/w/h), glides onto the grid on release.
    render() updates rows of the existing model -> plates animate instead of jumping.
  - Group drag/resize point = real cursor (plat::cursor_logical), not pointer pos relative to the moving grip
    (that drifted -> resize shook). Filmed: size now tracks cursor 1:1 both ways.

- Promo (2026-11-23): Threads thread draft + 5 demo GIFs/MP4 in promo/threads/ (tiles, open, groups, settings, pin).
  Dev env GF_DESKTOP=dir runs app on a demo folder (filmed with copies of Start-menu shortcuts, APPDATA redirected).

- English UI (2026-11-23): settings lang 0 ru / 1 en, default from Windows UI language, "Language" seg in settings.
  Settings labels reworded, one-line rows; Settings title aligned with header buttons.
  README GIFs re-recorded per language: docs/media/en, docs/media/ru (old Russian-only GIFs removed).
- Public on GitHub (2026-11-23): README EN/RU with demo GIFs, MIT, release v0.8.0 (zip). Goal: stars.
- 1.0 polish (2026-11-23 night, branch claude/animations-performance-v1-3ed962):
  - Big folder open: 3.9 s -> 24 ms (icons.rs: 4 loader threads + cache + background prefetch; icons fade in).
  - Big folder fits the screen: auto group height max 3 rows, up to 12 cols (was 2280 px tall).
  - Name hint: no empty glass strip under it (WM_GETMINMAXINFO min 1x1).
  - Frame pacing: loop paced by DwmFlush while animating -> even 120 fps (was 50-60 uneven).
  - Open / close: panel zooms out of the tile (whole content scaled, fades), lands back softly; no grey flash
    (panel always NC-active, DWM transitions off).
  - Window size glides (drag room, unfold, settings); settings page swap without double exposure.
  - Dropped shortcut glides from cursor into its slot; hover / press fades; dragged tile goes over neighbours;
    tile "+N" on a soft squircle.
  - Verified: screen recordings 120 fps (ffmpeg ddagrab) of 11 scenarios on a demo desktop, frame analysis.
  - Gemini (Antigravity, Neon) motion/UI review: docs/review-2026-11-23-gemini.md. RAM private ~12.6 MB (icon cache).

- Headless UI tests (2026-11-23): env GF_SCRIPT=scenario runs the app with invisible click-through windows,
  events go straight to Slint (virtual cursor, real mouse untouched), shots/frames from the DIB to PNG, exit 0/1.
  tests/ui/run.ps1 (demo desktop + own APPDATA in %TEMP%), scenarios open-close, drag-item, hover-hint: all PASS.
  tests/ui/idle.ps1 = guard for rare real-input checks (abort if user idle < 120 s).

## Next
- 1.0: user check on the real desktop, then merge branch + bump version + release.
  Not checked on the real screen yet (headless tests only): native menu look (dark), rename box focus, PickIconDlg,
  uninstaller run, drop onto the real desktop (icon placed at drop point), Fly icon + tile highlight while dragging.
- Shortcut rename: renamed item re-sorts by name (keeps place only if the group has a saved order).
- From Gemini review (not done): 2-line labels, stronger dark tint on bright wallpaper, collapse button in group
  header, tooltip fade, unfolded rows fade-in, row hysteresis on tile resize.
- User click test: tile opens panel, icon launches, click outside closes (not verified by Claude).
- Desktop layer: parent tiles to WorkerW (now: bottom-most windows; Win+D hides them).
- New folder on Desktop while running: no tile until restart. "Show hidden files" on -> tiled folders show dimmed.
- Big tiles: launch icons directly (now any click opens panel). Snap tiles to grid.
- Panel: remember moved position (now re-centers each open).
- Watch Desktop for changes (now: rescan on open).
- Tray icon + autostart; sort shortcuts into real subfolders.

## Blockers
- none

## Repo
GitHub (private): https://github.com/eonesets/glass-folders, branch main.

## Run
`app\target\release\glass-folders.exe`. Dev flags: `--open GAMES`; env `GF_KEEP_ICONS=1` (don't hide folder icons), `GF_DUMP_ICONS=file`, `GF_STAY=1` (panel stays open), `GF_TILE_AT=x,y` (tiles row at point), `GF_FAKE_GLASS=1` (old tile blur).
Build: `cargo build --release` in app/.
UI tests (no real mouse): `powershell -File tests/ui/run.ps1` (GF_SCRIPT, GF_OUT, GF_OFFSCREEN, GF_TIMEOUT). See CLAUDE.md.

- v0.7 (2026-11-22): launch from tile.
  - Launch counts per item file name in .glassfolder.json "uses" (panel + tile launches).
  - Tile icons sorted by uses desc (ties = panel order). Click icon = run it, no panel; hover/press highlight.
  - Items > cells: last cell "+N" opens panel. Empty space / name strip also open panel.
  - Verified on screen: VS Code launched from Кодинг tile, count saved, moved to first cell.

- v0.8 (2026-11-22): pin, name hint, icon size.
  - Pin button (header, between gear and X): folder window stays on the desktop (bottom z-order like tiles,
    live glass via NC-active trick, no close on focus loss / launch / Esc), its tile hides. Saved as cfg "pin" [x, y]
    (moved by header -> saved). Start: pinned folders open at their spot. Unpin = tile back; X on pinned = unpin + fold.
    Several windows now: Global.panels ([0] = the one tiles open; pinning [0] makes a fresh [0]). Tiles kept under pinned.
  - Name hint: own topmost click-through window (Kind::Hint) under a hovered shortcut after 400 ms; tile icons always,
    panel only when labels are off. Follows at once between shortcuts.
  - Icon size (settings: small / medium / large = zoom 0.85 / 1 / 1.2): window scale = DPI x zoom (plat::set_zoom),
    panels + tiles scale, tiles keep their cells. Large: icons fetched at plain DPI (bigger = shell frames low-res icons).
  - Tile "+N": no own plate/hover; it is part of the tile (whole tile lights, drag moves it). Icon hover kept.
  - Fix: tiles no longer snapped on start when "Плитки по сетке" is off.
  - Verified on screen: hint (tile + panel), pin -> outside click keeps it, restart restores it, other folder opens
    meanwhile, unpin -> tile back, large size + back to medium. RAM private ~8.6 MB.