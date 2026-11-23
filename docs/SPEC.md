Кратко: что решили про приложение «стеклянные папки» на рабочем столе — ответы Тимы на вопросы и технический выбор.
Дизайн-макеты: docs/mockups.html. 

# glass-folders — spec (decisions 2026-11-22)

## Goal
Replace Windows desktop icons with iOS-like folder widgets. Click folder -> centered popup panel.
Style: Windows 11 design language (Fluent): matte acrylic/mica glass, rectangular, small radius (8px), not round iOS bubbles.

## User decisions
- Desktop: app REPLACES Windows icons (hide native icons, draw own tiles on desktop layer).
- Closed folder tile: user-resizable. Small = mini-grid preview. Large = icons launchable right from desktop.
- Inside folder: icons only (no game covers).
- Launch: single click; panel auto-closes after launch.
- Theme: follow Windows light/dark.
- Groups inside folder: rectangular blocks (e.g. Strategies, Shooters). Folder has grid (e.g. 4x5 or larger); group spans WxH cells, resizable by drag.
- Grid auto-fits app count so edges stay even (no ragged last row).
- Color settings: per folder + per group tint.

## v2 decisions (2026-11-22)
- Style: variant A "Mica" chosen. B/C dropped.
- Inside folder: phone-like. Every slot = square cell, one app per cell, icon fixed square. Group w x h = exactly w*h slots. Never stretch icons/slots. Short names. Labels toggle.
- Group too small -> last slot shows "+N".

## Folder-based model (user idea, core principle)
- Widget = a real folder on Desktop. New folder on desktop -> new widget automatically.
- Groups = subfolders inside it (GAMES\Шутеры\...). Loose shortcuts in folder root -> group "Разное".
- Layout extras (group size, colors, order) stored in a hidden file inside the folder (e.g. `.glassfolder.json`, hidden attr). Windows ignores it.
- Uninstall app -> native desktop icons restored, everything stays plain folders/subfolders. Zero lock-in.
- Install app -> picks up existing folders as widgets. No import step.
- Watch Desktop with FileSystemWatcher: add/rename/delete folder or shortcut -> widget updates live.


## v3 decisions (2026-11-22)
- No color strip. Color = click the color dot (group header / folder title) -> small popover with 10 colors.
- Narrow groups (w=1): hide count, keep name readable.
- Desktop tiles: 1 desktop cell = 2x2 icons. Tile w x h -> 2w x 2h icon slots. Overflow -> last slot "+N" opens folder.
- Big tile: icons launch directly. 1x1 tile: click opens folder.

## Tech choice (updated 2026-11-22: Rust, user asked for low RAM)
- Rust. Single small .exe, no runtime. Target idle RAM < 40 MB.
- UI: Slint (declarative UI, software/femtovg renderer, low RAM). Fallback if Slint fights transparency: raw windows-rs + Direct2D.
- OS glue: windows-rs crate. DWM backdrop (DWMWA_SYSTEMBACKDROP_TYPE acrylic) + rounded corners (DWMWA_WINDOW_CORNER_PREFERENCE) = real Win11 glass per window.
- Rejected: C# WinUI 3 (~80-120 MB RAM), Tauri/Electron (WebView, 100 MB+).
- Each desktop tile = own small borderless window parented to WorkerW (so acrylic blurs wallpaper). Popup = one reusable window, hidden when closed (no re-create).
- Idle = zero CPU: no timers, redraw only on events. Icons cached as PNG on disk, loaded lazily, freed when popup hidden.
- (old) C# + WinUI 3 plan dropped.
- Desktop layer: attach host window to WorkerW behind icons; hide native icons (toggle in settings, restore on exit).
- Data: shortcuts stay in real folders (Desktop\GAMES etc). Layout (groups, sizes, colors) in JSON config.
- Icons: extract from .lnk/.url targets (Shell API), cache PNG.
- Autostart with Windows.

## Prototype v0.1 findings (2026-11-22)
- Slint winit + femtovg (OpenGL): 247 MB private RAM (GPU driver per process). Rejected.
- Slint winit + software renderer: 18 MB, but softbuffer presents opaque -> no glass.
- Final: own Slint Platform (app/src/plat.rs). SoftwareRenderer -> premultiplied BGRA DIB -> BitBlt.
  Window = WS_POPUP|WS_THICKFRAME, frame removed in WM_NCCALCSIZE (needed: DWM honors alpha/backdrop only on framed windows).
  DwmExtendFrameIntoClientArea(-1) + DWMSBT_TRANSIENTWINDOW (panel only).
  Result: ~6 MB private, ~28 MB working set, 0% CPU idle (MsgWaitForMultipleObjects until next timer/input).
- Glass on inactive windows (tested with transparency ON, build 26200): DWMSBT acrylic, Mica, forced WM_NCACTIVATE,
  SWCA accent (normal + layered window) all render SOLID once the window is inactive and has drawn content.
  SWCA blurs only a fully empty window. => Panel uses real acrylic (it is active while open).
  Tiles (never active) draw own frosted wallpaper: WIC decode wallpaper at 1/4 size, crop under tile,
  3x box blur, 55% theme tint (win::wallpaper_blur). Static wallpaper only; Wallpaper Engine would need another way.
- Icons: IShellItemImageFactory at exact px (48*scale). Big sizes add a frame around low-res icons.
- Theme passed from Rust (registry AppsUseLightTheme); Slint Palette unknown on custom platform.
- Needs Windows "Transparency effects" ON (EnableTransparency=1), else panel is solid. User enabled it 2026-11-22.

## Open questions
- Edit mode UX: long-press / right-click "Edit" -> drag corners of groups. (default plan)
- Wallpaper Engine is installed: check glass blur works over it.
