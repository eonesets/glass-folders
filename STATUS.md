Кратко: приложение «стеклянные папки» для рабочего стола. v0.6: настройки разделены на «Папка» и «Группы внутри», плитка без подписи становится ровной (квадратные клетки), группы без заголовка облегают ярлыки, «+N» раскрывает группу на месте, группы в окне двигаются плавно за курсором.
Ждём: проверка пользователем.

# glass-folders — STATUS
Updated: 2026-11-22 (v0.6)
Progress: 70%

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

## Next
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
