Кратко: приложение «стеклянные папки» для рабочего стола. v0.4: при запуске значки Windows прячутся и вместо папок стоят плитки (впервые — на месте папок), при выходе значки возвращаются. В окне папки группы свободно двигаются и растягиваются, ярлыки переставляются, вынос ярлыка в пустое место сразу создаёт плашку новой группы.
Ждём: проверка пользователем.

# glass-folders — STATUS
Updated: 2026-11-22 (v0.4)
Progress: 55%

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

## Next
- User click test: tile opens panel, icon launches, click outside closes (not verified by Claude).
- Desktop layer: parent tiles to WorkerW (now: bottom-most windows; Win+D hides them). Re-hide icons after Explorer restart.
- Loose files on Desktop (not folders) are hidden with the icon layer: give them tiles if any appear.
- Big tiles: launch icons directly (now any click opens panel). Snap tiles to grid.
- Panel: remember moved position (now re-centers each open).
- Watch Desktop for changes (now: rescan on open).
- Tray icon + autostart; sort shortcuts into real subfolders.

## Blockers
- none

## Repo
GitHub (private): https://github.com/eonesets/glass-folders, branch main.

## Run
`app\target\release\glass-folders.exe`. Dev flags: `--open GAMES`; env `GF_KEEP_ICONS=1` (don't hide desktop icons), `GF_DUMP_ICONS=file`, `GF_STAY=1` (panel stays open), `GF_TILE_AT=x,y` (tiles row at point), `GF_FAKE_GLASS=1` (old tile blur).
Build: `cargo build --release` in app/.
