Кратко: приложение «стеклянные папки» для рабочего стола. Прототип на Rust работает: стекло у окна папки — настоящее (Windows), у плиток — своё размытие обоев. Ест ~6 МБ памяти.
Ждём: проверка кликов пользователем, потом встраивание плиток в сам рабочий стол.

# glass-folders — STATUS
Updated: 2026-11-22
Progress: 25%

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

## Next
- User click test: tile opens panel, icon launches, click outside closes (not verified by Claude).
- Desktop layer: parent tiles to WorkerW / hide native icons (now: bottom-most windows; Win+D hides them).
- Re-blur tile backdrop on wallpaper change (WM_SETTINGCHANGE) and tile move. Multi-monitor wallpaper.
- Tile placement + resize (big tiles launch icons directly), drag to move.
- Edit mode: resize groups by drag, reorder. Watch Desktop for changes (now: rescan on open).
- Tray icon + autostart; sort shortcuts into real subfolders.

## Blockers
- none

## Repo
GitHub (private): https://github.com/eonesets/glass-folders, branch main.

## Run
`app\target\release\glass-folders.exe`. Dev flags: `--open GAMES`; env `GF_STAY=1` (panel stays open), `GF_TILE_AT=x,y` (tiles row at point).
Build: `cargo build --release` in app/.
