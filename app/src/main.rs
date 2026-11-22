// Кратко: точка входа. При запуске ставит вместо папок на столе плитки (папки скрываются, ярлыки
// и выделение на столе остаются); при выходе значки папок возвращаются на места. Окно папки вырастает из плитки.

#![windows_subsystem = "windows"]

slint::include_modules!();
mod desk;
mod model;
mod plat;
mod win;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use model::{GRect, GAP, HEAD, PITCH, ROW};
use slint::{ComponentHandle, Image, Model, ModelRc, PhysicalPosition, PhysicalSize, VecModel};
use windows::Win32::Foundation::{POINT, RECT};

const OPEN_MS: f32 = 230.0; // panel grows out of the tile
const CLOSE_MS: f32 = 150.0; // ... and folds back into it

// Icons are requested at exact on-screen pixel size: big sizes make the shell add a frame around low-res icons.
fn px(logical: f32, scale: f32) -> i32 {
    (logical * scale).round() as i32
}

/// What a dragged shortcut will do on release.
#[derive(Clone, Copy, PartialEq, Default)]
enum Target {
    #[default]
    None,
    /// into group (index), at place
    Group(usize, usize),
    /// new group at App::ghost
    New,
}

#[derive(Default)]
struct App {
    folder: Option<model::Folder>,
    ids: Vec<PathBuf>,    // slot id -> file
    id_group: Vec<usize>, // slot id -> group index
    icons: HashMap<PathBuf, Image>,
    set: model::Settings,
    dark: bool,
    tiles: Vec<(PathBuf, Tile)>,
    tile_items: HashMap<PathBuf, Vec<PathBuf>>, // folder -> files behind its tile icons (index = icon)
    // panel layout: base = as rendered, prev = shown now (while dragging), ghost = new-group plate
    base: Vec<GRect>,
    prev: Vec<GRect>,
    ghost: Option<GRect>,
    target: Target,
    grab: Option<(f32, f32)>, // group move: cursor offset from group corner
    lift: [f32; 4],           // moved / resized group as drawn under the cursor (grid px)
    groups: Option<Rc<VecModel<GroupData>>>,
    // open / close animation of the panel
    anim: Option<slint::Timer>,
    closing: bool,
    // group unfolded by its "+N" slot (until folded back or panel closed)
    unfolded: Option<usize>,
}

impl App {
    /// Group header height in panel px. No title and no dot: plate hugs its slots.
    fn head(&self) -> f32 {
        if self.set.names || self.set.dots {
            HEAD
        } else {
            0.0
        }
    }
    fn tile_rect(&self, dir: &Path) -> Option<RECT> {
        self.tiles.iter().find(|(d, _)| d == dir).and_then(|(_, t)| plat::rect_of(t.window()))
    }
}

type Shared = Rc<RefCell<App>>;

fn main() -> Result<(), slint::PlatformError> {
    slint::platform::set_platform(Box::new(plat::GfPlatform::new())).expect("platform");
    win::com_init();
    // any crash: give the user their desktop icons back
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |i| {
        desk::restore_folders(&HashMap::new());
        hook(i);
    }));
    let desktop = win::desktop_dir();
    let app: Shared = Rc::new(RefCell::new(App { set: model::load_settings(), dark: win::system_dark(), ..Default::default() }));
    let dark = app.borrow().dark;
    let panel = Panel::new()?;
    if let Some(h) = plat::hwnd_of(panel.window()) {
        win::glass(h, dark);
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        let stay = std::env::var("GF_STAY").is_ok(); // dev: keep panel open on focus loss
        plat::set_on_deactivate(panel.window(), move || {
            if stay {
                return;
            }
            if let Some(p) = p.upgrade() {
                close(&p, &a);
            }
        });
    }
    panel.set_dark(dark);
    push_settings(&panel, &app.borrow().set);
    plat::set_snap(app.borrow().set.snap);
    plat::set_tile_named(app.borrow().set.tnames);
    model::set_header(app.borrow().set.names || app.borrow().set.dots);
    let pal: Vec<slint::Color> = model::PAL.iter().map(|h| model::color(h)).collect();
    panel.set_pal(ModelRc::new(VecModel::from(pal)));

    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_launch(move |id| {
            let path = a.borrow().ids.get(id as usize).cloned();
            let root = a.borrow().folder.as_ref().map(|f| f.dir.clone());
            if let Some(path) = path {
                win::open(&path);
                if let Some(root) = root {
                    model::bump_use(&root, &path);
                    refresh_tile(&a, &root);
                }
            }
            if let Some(p) = p.upgrade() {
                close(&p, &a);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_open_group(move |idx| {
            // deferred: the clicked slot belongs to the model that render() updates
            let (a, p) = (a.clone(), p.clone());
            slint::Timer::single_shot(Duration::ZERO, move || {
                let Some(p) = p.upgrade() else { return };
                {
                    let mut st = a.borrow_mut();
                    let i = idx as usize;
                    st.unfolded = if st.unfolded == Some(i) { None } else { Some(i) };
                }
                render(&p, &a);
                fit_panel(&p);
            });
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_pick_color(move |idx, c| {
            if let Some(p) = p.upgrade() {
                set_color(&p, &a, idx, Some(model::hex(c)));
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_clear_color(move |idx| {
            if let Some(p) = p.upgrade() {
                set_color(&p, &a, idx, None);
            }
        });
    }
    {
        let p = panel.as_weak();
        panel.on_drag_start(move || {
            if let Some(p) = p.upgrade() {
                plat::start_move(p.window());
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_set_opt(move |key, v| {
            if let Some(p) = p.upgrade() {
                set_opt(&p, &a, key.as_str(), v);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_close_panel(move || {
            if let Some(p) = p.upgrade() {
                close(&p, &a);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_item_drag(move |id, x, y, inside| {
            if let Some(p) = p.upgrade() {
                item_drag(&mut a.borrow_mut(), id, x, y, inside);
                show_preview(&p, &a.borrow(), true);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_item_drop(move |id, _, _, _| {
            // defer: rebuilding the groups model inside the slot's own event handler is unsafe
            let (a, p) = (a.clone(), p.clone());
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(p) = p.upgrade() {
                    drop_item(&p, &a, id);
                }
            });
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_group_drag(move |idx, x, y, resize| {
            if let Some(p) = p.upgrade() {
                // real cursor, grid coords (pad 16, header 48 as in app.slint)
                let (x, y) = plat::cursor_logical(p.window()).map_or((x, y), |(cx, cy)| (cx - 16.0, cy - 48.0));
                group_drag(&mut a.borrow_mut(), idx as usize, x, y, resize);
                show_preview(&p, &a.borrow(), true);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_group_drop(move |idx, _, _, resize| {
            let (a, p) = (a.clone(), p.clone());
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(p) = p.upgrade() {
                    drop_group(&p, &a, idx as usize, resize);
                }
            });
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_drag_cancel(move || {
            if let Some(p) = p.upgrade() {
                reset_preview(&mut a.borrow_mut());
                show_preview(&p, &a.borrow(), false);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_rename_group(move |old, new| {
            let (a, p) = (a.clone(), p.clone());
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(p) = p.upgrade() {
                    rename_group(&p, &a, &old, &new);
                }
            });
        });
    }
    {
        let a = app.clone();
        panel.on_quit(move || {
            give_back_icons(&a);
            let _ = slint::quit_event_loop();
        });
    }

    // v0.4 left-over (whole icon layer hidden by a killed run) + folder icons parked by a killed run
    desk::unhide_icon_layer();
    let killed: HashMap<PathBuf, POINT> =
        app.borrow().set.homes.iter().map(|(k, [x, y])| (PathBuf::from(k), POINT { x: *x, y: *y })).collect();
    desk::restore_folders(&killed);

    let homes = make_tiles(&app, &panel, &desktop, &killed)?;
    // tiles stand where the folders were -> hide the folder icons (dev: GF_KEEP_ICONS=1 keeps them).
    // Spots saved first: a killed run gets them back on next start.
    if std::env::var("GF_KEEP_ICONS").is_err() && !homes.is_empty() {
        {
            let mut st = app.borrow_mut();
            st.set.homes = homes.iter().map(|(k, p)| (k.to_string_lossy().into_owned(), [p.x, p.y])).collect();
            model::save_settings(&st.set);
        }
        desk::hide_folders(homes);
    }

    // Dev helper: glass-folders.exe --open GAMES
    let args: Vec<String> = std::env::args().collect();
    if let Some(name) = args.iter().position(|a| a == "--open").and_then(|i| args.get(i + 1)) {
        let (a, p, d) = (app.clone(), panel.as_weak(), desktop.join(name));
        slint::Timer::single_shot(Duration::from_millis(150), move || {
            if let Some(p) = p.upgrade() {
                open_folder(&p, &a, &d);
            }
        });
    }

    let r = slint::run_event_loop();
    give_back_icons(&app);
    r
}

/// Folder icons back to their spots; nothing left to restore next start.
fn give_back_icons(app: &Shared) {
    desk::restore_folders(&HashMap::new());
    let mut st = app.borrow_mut();
    if !st.set.homes.is_empty() {
        st.set.homes.clear();
        model::save_settings(&st.set);
    }
}

fn push_settings(panel: &Panel, set: &model::Settings) {
    panel.set_labels(set.labels);
    panel.set_names(set.names);
    panel.set_dots(set.dots);
    panel.set_glass(set.glass);
    panel.set_gravity(set.gravity);
    panel.set_snap(set.snap);
    panel.set_ptitle(set.ptitle);
    panel.set_tnames(set.tnames);
}

/// One setting changed in the settings page.
fn set_opt(panel: &Panel, app: &Shared, key: &str, v: i32) {
    let (relayout, snap_now, retile) = {
        let mut st = app.borrow_mut();
        let on = v != 0;
        let mut relayout = false;
        let mut snap_now = false;
        let mut retile = false;
        match key {
            "labels" => st.set.labels = on,
            "names" => (st.set.names, relayout) = (on, true),
            "dots" => (st.set.dots, relayout) = (on, true),
            "ptitle" => st.set.ptitle = on,
            "tnames" => (st.set.tnames, retile) = (on, true),
            "gravity" => (st.set.gravity, relayout) = (v.clamp(0, 2), true),
            "glass" => st.set.glass = v.clamp(0, 2),
            "snap" => (st.set.snap, snap_now) = (on, on),
            _ => return,
        }
        model::save_settings(&st.set);
        push_settings(panel, &st.set);
        plat::set_snap(st.set.snap);
        model::set_header(st.set.names || st.set.dots);
        for (_, t) in &st.tiles {
            reblur(t, st.dark, st.set.glass);
        }
        (relayout, snap_now, retile)
    };
    if retile {
        // same cells, new cell shape: tile grows / shrinks by the name strip
        let st = app.borrow();
        let on = st.set.tnames;
        for (d, t) in &st.tiles {
            let Some(r) = plat::rect_of(t.window()) else { continue };
            let s = t.window().scale_factor();
            plat::set_tile_named(!on); // measure the old way
            let (c, rows) = plat::tile_cells(r.right - r.left, r.bottom - r.top, s);
            plat::set_tile_named(on);
            let (w, h) = plat::tile_size(c, rows, s);
            t.set_named(on);
            t.window().set_size(PhysicalSize::new(w as u32, h as u32));
            let mut cfg = model::load_cfg(d);
            cfg.tile = Some([r.left, r.top, w, h]);
            let _ = model::save_cfg(d, &cfg);
        }
    }
    if snap_now {
        // grid switched on: every tile onto the grid now
        let st = app.borrow();
        for (d, t) in &st.tiles {
            let Some(r) = plat::rect_of(t.window()) else { continue };
            let (x, y) = plat::snap_pos(r.left, r.top, t.window().scale_factor());
            t.window().set_position(PhysicalPosition::new(x, y));
            let mut cfg = model::load_cfg(d);
            cfg.tile = Some([x, y, r.right - r.left, r.bottom - r.top]);
            let _ = model::save_cfg(d, &cfg);
        }
    }
    if relayout && app.borrow().folder.is_some() {
        render(panel, app);
    }
}

/// Desktop tiles: one per folder on Desktop. Place: saved rect; else where the folder icon stands
/// on the desktop (first run); else a column at the right edge. Never on top of another tile.
/// Returns home spots of the folder icons (to park them away while tiles stand in).
fn make_tiles(
    app: &Shared,
    panel: &Panel,
    desktop: &Path,
    killed: &HashMap<PathBuf, POINT>,
) -> Result<HashMap<PathBuf, POINT>, slint::PlatformError> {
    let dark = app.borrow().dark;
    let glass = app.borrow().set.glass;
    let mut native = desk::icon_positions();
    // killed run: its saved spots win (icon may not be back on the desktop yet)
    for (k, p) in killed {
        native.insert(k.clone(), *p);
    }
    let mut homes = HashMap::new();
    // dev: GF_DUMP_ICONS=file writes where Explorer has each desktop icon
    if let Ok(out) = std::env::var("GF_DUMP_ICONS") {
        let txt: String = native.iter().map(|(p, pt)| format!("{}\t{}\t{}\n", p.display(), pt.x, pt.y)).collect();
        let _ = std::fs::write(out, txt);
    }
    let folders: Vec<model::Folder> = model::list_folders(desktop).iter().map(|d| model::scan(d)).collect();
    let wa = win::work_area();
    let mut taken: Vec<RECT> = folders
        .iter()
        .filter_map(|f| f.cfg.tile)
        .map(|[x, y, w, h]| RECT { left: x, top: y, right: x + w, bottom: y + h })
        .collect();
    let dev_row = std::env::var("GF_TILE_AT")
        .ok()
        .and_then(|v| v.split_once(',').map(|(a, b)| (a.parse::<i32>().unwrap_or(0), b.parse::<i32>().unwrap_or(0))));

    for (i, f) in folders.into_iter().enumerate() {
        let dir = f.dir.clone();
        let t = Tile::new()?;
        t.set_dark(dark);
        t.set_name(f.name.clone().into());
        t.set_named(app.borrow().set.tnames);
        let (c, on) = model::tint(&f.tint());
        t.set_tint(c);
        t.set_colored(on);
        let items = tile_icons(&t, &f);
        app.borrow_mut().tile_items.insert(dir.clone(), items);
        {
            let (a, p, d) = (app.clone(), panel.as_weak(), dir.clone());
            t.on_open(move || {
                if let Some(p) = p.upgrade() {
                    open_folder(&p, &a, &d);
                }
            });
        }
        {
            // click on a tile icon: run it right from the desktop, no panel
            let (a, d) = (app.clone(), dir.clone());
            t.on_launch(move |i| {
                let path = a.borrow().tile_items.get(&d).and_then(|v| v.get(i as usize).cloned());
                let Some(path) = path else { return };
                win::open(&path);
                model::bump_use(&d, &path);
                // deferred: icons model is rebuilt, the clicked icon belongs to it
                let (a, d) = (a.clone(), d.clone());
                slint::Timer::single_shot(Duration::ZERO, move || refresh_tile(&a, &d));
            });
        }
        plat::set_kind(t.window(), plat::Kind::Tile);
        if let Some(h) = plat::hwnd_of(t.window()) {
            win::glass(h, dark);
        }
        let s = t.window().scale_factor();
        let (side_w, side_h) = plat::snap_size(0, 0, s); // min tile = 2x2 cells
        let (x, y, w, h) = if let Some((ax, ay)) = dev_row {
            // dev: GF_TILE_AT=x,y puts tiles in a row from that point (free, not snapped)
            (ax + i as i32 * (side_w + 16), ay, side_w, side_h)
        } else if let Some([sx, sy, sw, sh]) = f.cfg.tile {
            let (w, h) = plat::snap_size(sw, sh, s);
            let (x, y) = plat::snap_pos(sx, sy, s);
            (x, y, w, h)
        } else {
            let start = match native.get(&dir) {
                Some(p) => (p.x, p.y),
                None => (wa.right - ((24.0 + 112.0) * s) as i32, wa.top + (24.0 * s) as i32),
            };
            let (x, y) = free_spot(start, side_w, side_h, &taken, s);
            // remember right away: next start puts it in the same place
            let mut cfg = f.cfg.clone();
            cfg.tile = Some([x, y, side_w, side_h]);
            let _ = model::save_cfg(&dir, &cfg);
            (x, y, side_w, side_h)
        };
        taken.push(RECT { left: x, top: y, right: x + w, bottom: y + h });
        if let Some(p) = native.get(&dir) {
            homes.insert(dir.clone(), *p);
        }
        t.window().set_size(PhysicalSize::new(w as u32, h as u32));
        t.window().set_position(PhysicalPosition::new(x, y));
        reblur(&t, dark, glass);
        {
            // user moved/resized the tile: remember it, re-blur what is under it now
            let (a, tw, d) = (app.clone(), t.as_weak(), dir.clone());
            plat::set_on_moved(t.window(), move |r| {
                let mut cfg = model::load_cfg(&d);
                cfg.tile = Some([r.left, r.top, r.right - r.left, r.bottom - r.top]);
                let _ = model::save_cfg(&d, &cfg);
                let st = a.borrow();
                if let Some(t) = tw.upgrade() {
                    reblur(&t, st.dark, st.set.glass);
                }
            });
        }
        t.show()?;
        app.borrow_mut().tiles.push((dir, t));
    }
    Ok(homes)
}

/// Snapped spot near `start` (physical px) where a w x h tile hits no taken rect.
/// Walks down the column, then the next column to the right; stays inside the work area.
fn free_spot(start: (i32, i32), w: i32, h: i32, taken: &[RECT], s: f32) -> (i32, i32) {
    let wa = win::work_area();
    let (cw, ch) = ((plat::CELL_W * s).round() as i32, (plat::CELL_H * s).round() as i32);
    let clamp = |x: i32, y: i32| plat::snap_pos(x.clamp(wa.left, (wa.right - w).max(wa.left)), y.clamp(wa.top, (wa.bottom - h).max(wa.top)), s);
    let (x0, y0) = clamp(start.0, start.1);
    let free = |x: i32, y: i32| !taken.iter().any(|r| x < r.right && r.left < x + w && y < r.bottom && r.top < y + h);
    let mut x = x0;
    for _ in 0..200 {
        let mut y = y0;
        while y + h <= wa.bottom {
            if free(x, y) {
                return (x, y);
            }
            y += ch;
        }
        x += cw;
        if x + w > wa.right {
            x = wa.left; // wrap to the left edge
        }
    }
    (x0, y0)
}

/// Tile icons: most launched first. Enough for a big tile; Slint shows only what fits.
/// Returns the files behind the icons (same order) for launch from the tile.
fn tile_icons(t: &Tile, f: &model::Folder) -> Vec<PathBuf> {
    let all = f.top_items();
    t.set_total(all.len() as i32);
    let s = t.window().scale_factor();
    let (items, icons): (Vec<PathBuf>, Vec<Image>) =
        all.into_iter().take(24).filter_map(|p| win::icon(&p, px(36.0, s)).map(|i| (p, i))).unzip();
    t.set_icons(ModelRc::new(VecModel::from(icons)));
    items
}

/// Re-read one folder from disk and redraw its desktop tile (launch counts changed).
fn refresh_tile(app: &Shared, dir: &Path) {
    let f = model::scan(dir);
    let items = {
        let st = app.borrow();
        st.tiles.iter().find(|(d, _)| d == dir).map(|(_, t)| tile_icons(t, &f))
    };
    if let Some(items) = items {
        app.borrow_mut().tile_items.insert(dir.to_path_buf(), items);
    }
}

// ---------- panel drag: preview ----------

fn hit(r: &GRect, x: f32, y: f32, pad: f32) -> bool {
    let [rx, ry, rw, rh] = r.px();
    x >= rx - pad && x <= rx + rw + pad && y >= ry - pad && y <= ry + rh + pad
}

fn reset_preview(st: &mut App) {
    st.prev = st.base.clone();
    st.ghost = None;
    st.target = Target::None;
    st.grab = None;
}

/// Dragged shortcut at grid point (x, y): pick target group + place, or a new-group plate
/// right under the cursor (groups below make room).
fn item_drag(st: &mut App, id: i32, x: f32, y: f32, inside: bool) {
    let Some(&src) = st.id_group.get(id as usize) else { return };
    if !inside {
        reset_preview(st);
        return;
    }
    // still over the plate: keep it (no jitter)
    if st.ghost.is_some_and(|g| hit(&g, x, y, GAP / 2.0)) {
        return;
    }
    // over a group (as shown now; gaps count as the nearer group)
    if let Some(i) = st.prev.iter().position(|r| hit(r, x, y, GAP / 2.0)) {
        // group at its own place: undo pushes left by a plate the cursor passed over
        if hit(&st.base[i], x, y, GAP / 2.0) {
            st.prev = st.base.clone();
        }
        let r = st.prev[i];
        let [rx, ry, _, _] = r.px();
        let Some(f) = st.folder.as_ref() else { return };
        let col = ((x - rx) / PITCH).floor().clamp(0.0, (r.w - 1) as f32) as usize;
        let row = ((y - ry - st.head()) / PITCH).floor().max(0.0) as usize;
        let n = f.groups[i].items.len() - usize::from(i == src); // items besides the dragged one
        let cap = (r.w * r.rows()) as usize;
        let k = (row * r.w as usize + col).min(n).min(cap.saturating_sub(1));
        st.ghost = None; // groups stay as shown: what you see is where you drop
        st.target = Target::Group(i, k);
        return;
    }
    // empty space: 1x1 plate with its slot under the cursor, never over a group above it
    let mut g = GRect::slots(1, 1, (x / PITCH).floor().max(0.0) as i32, ((y - st.head() - model::CELL / 2.0) / ROW).round().max(0.0) as i32);
    while let Some(b) = st.base.iter().filter(|r| r.hits(&g) && r.y < g.y).map(|r| r.y + r.h).max() {
        g.y = b;
    }
    let mut all = st.base.clone();
    all.push(g);
    let last = all.len() - 1;
    model::resolve(&mut all, Some(last));
    model::settle(&mut all, st.set.gravity);
    st.ghost = all.pop();
    st.prev = all;
    st.target = Target::New;
}

/// Group header / grip dragged to grid point (x, y): move or resize it; others make room.
fn group_drag(st: &mut App, idx: usize, x: f32, y: f32, resize: bool) {
    let Some(mut r) = st.base.get(idx).copied() else { return };
    let [rx, ry, rw, rh] = r.px();
    if resize {
        let w = ((x - rx + GAP) / PITCH).round().max(1.0) as i32;
        let rows = ((y - ry - model::header_px() + GAP) / PITCH).round().max(1.0) as i32;
        r = GRect::slots(w, rows, r.x, r.y);
        // plate follows the cursor, not smaller than one slot
        st.lift = [rx, ry, (x - rx).max(model::CELL), (y - ry).max(model::CELL + model::header_px() - GAP)];
    } else {
        let (ox, oy) = *st.grab.get_or_insert((x - rx, y - ry));
        r.x = ((x - ox) / PITCH).round().max(0.0) as i32;
        r.y = ((y - oy) / ROW).round().max(0.0) as i32;
        st.lift = [(x - ox).max(0.0), (y - oy).max(0.0), rw, rh];
    }
    let mut all = st.base.clone();
    all[idx] = r;
    model::resolve(&mut all, Some(idx));
    model::settle(&mut all, st.set.gravity);
    st.prev = all;
}

/// Push preview into the panel: group rects (rows keep their item models, so a drag in progress
/// is not broken), plate, drop target, canvas size (room to drop right / below while dragging).
fn show_preview(panel: &Panel, st: &App, dragging: bool) {
    if let Some(m) = st.groups.as_ref() {
        for (i, r) in st.prev.iter().enumerate() {
            let Some(mut d) = m.row_data(i) else { continue };
            let [x, y, w, h] = r.px();
            if (d.x, d.y, d.w, d.h) != (x, y, w, h) {
                (d.x, d.y, d.w, d.h, d.cols, d.rows) = (x, y, w, h, r.w, r.rows());
                m.set_row_data(i, d);
            }
        }
    }
    let (t, k) = match st.target {
        Target::None => (-2, 0),
        Target::Group(i, k) => (i as i32, k as i32),
        Target::New => (-1, 0),
    };
    panel.set_drop_target(t);
    panel.set_drop_index(k);
    let [lx, ly, lw, lh] = st.lift;
    panel.set_lift_x(lx);
    panel.set_lift_y(ly);
    panel.set_lift_w(lw);
    panel.set_lift_h(lh);
    if let Some(g) = st.ghost {
        let [x, y, w, h] = g.px();
        panel.set_ghost_x(x);
        panel.set_ghost_y(y);
        panel.set_ghost_w(w);
        panel.set_ghost_h(h);
    }
    panel.set_ghost_on(st.ghost.is_some());
    let mut all: Vec<GRect> = st.base.iter().chain(&st.prev).copied().chain(st.ghost).collect();
    if all.is_empty() {
        all.push(GRect::slots(1, 1, 0, 0));
    }
    let (w, h) = if dragging { model::grid_px(&all, 1, 3) } else { model::grid_px(&st.base, 0, 0) };
    panel.set_grid_w(w);
    panel.set_grid_h(h);
}

// ---------- panel drag: drop ----------

/// Save shown group places (prev) into cfg, by group name.
fn keep_places(cfg: &mut model::Config, f: &model::Folder, prev: &[GRect]) {
    for (g, r) in f.groups.iter().zip(prev) {
        let c = cfg.groups.entry(g.name.clone()).or_default();
        (c.x, c.y) = (Some(r.x), Some(r.y));
    }
}

fn names(items: &[PathBuf]) -> Vec<String> {
    items.iter().filter_map(|p| model::file_name(p)).collect()
}

/// Drop dragged shortcut: reorder in its group, move file into another group (real fs move),
/// or create a new group folder where the plate is. Any fs error: logged, item stays.
fn drop_item(panel: &Panel, app: &Shared, id: i32) {
    let (root, created) = {
        let mut st = app.borrow_mut();
        let target = st.target;
        let st = &mut *st;
        if target == Target::None {
            reset_preview(st);
            show_preview(panel, st, false);
            return;
        }
        let (Some(f), Some(src), Some(&sg)) = (st.folder.as_ref(), st.ids.get(id as usize), st.id_group.get(id as usize)) else { return };
        let root = f.dir.clone();
        let src_name = model::file_name(src).unwrap_or_default();
        let mut cfg = model::load_cfg(&root); // fresh from disk: tile rect may have changed since open
        let mut created = None;
        match target {
            Target::Group(t, k) if t == sg => {
                let mut order = names(&f.groups[t].items);
                order.retain(|n| *n != src_name);
                order.insert(k.min(order.len()), src_name);
                cfg.groups.entry(f.groups[t].name.clone()).or_default().items = order;
                keep_places(&mut cfg, f, &st.prev);
            }
            _ => {
                let src_dir = f.groups[sg].dir.clone();
                let (dst, k, new_name) = match target {
                    Target::Group(t, k) => (f.groups[t].dir.clone(), k, None),
                    _ => match model::create_group(&root) {
                        Ok(n) => (root.join(&n), 0, Some(n)),
                        Err(e) => {
                            eprintln!("glass-folders: create group in {}: {e}", root.display());
                            return;
                        }
                    },
                };
                let moved = match model::move_into(src, &dst) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("glass-folders: move {} -> {}: {e}", src.display(), dst.display());
                        if new_name.is_some() {
                            model::remove_if_empty(&dst);
                        }
                        return;
                    }
                };
                keep_places(&mut cfg, f, &st.prev);
                // target order: new name at drop place
                if let Target::Group(t, _) = target {
                    let mut order = names(&f.groups[t].items);
                    order.insert(k.min(order.len()), model::file_name(&moved).unwrap_or_default());
                    cfg.groups.entry(f.groups[t].name.clone()).or_default().items = order;
                }
                // source order: without it
                let sname = f.groups[sg].name.clone();
                let mut sorder = names(&f.groups[sg].items);
                sorder.retain(|n| *n != src_name);
                cfg.groups.entry(sname.clone()).or_default().items = sorder;
                // emptied subfolder: removed, settings forgotten
                if src_dir != root && model::remove_if_empty(&src_dir) {
                    cfg.forget_group(&sname);
                }
                if let (Some(n), Some(g)) = (&new_name, st.ghost) {
                    cfg.forget_group(n); // stale settings of an old group with the same name
                    let c = cfg.groups.entry(n.clone()).or_default();
                    (c.x, c.y) = (Some(g.x), Some(g.y));
                }
                created = new_name;
            }
        }
        if let Err(e) = model::save_cfg(&root, &cfg) {
            eprintln!("glass-folders: save config in {}: {e}", root.display());
        }
        (root, created)
    };
    rescan(panel, app, &root);
    if let Some(n) = created {
        let idx = app.borrow().folder.as_ref().and_then(|f| f.groups.iter().position(|g| g.name == n));
        if let Some(i) = idx {
            panel.set_rename_idx(i as i32);
        }
    }
}

/// Group moved / resized: save all shown places (others may have made room) + new size.
fn drop_group(panel: &Panel, app: &Shared, idx: usize, resize: bool) {
    let root = {
        let st = app.borrow();
        let Some(f) = st.folder.as_ref() else { return };
        let mut cfg = model::load_cfg(&f.dir);
        keep_places(&mut cfg, f, &st.prev);
        if let (true, Some(g), Some(r)) = (resize, f.groups.get(idx), st.prev.get(idx)) {
            let c = cfg.groups.entry(g.name.clone()).or_default();
            (c.w, c.h) = (Some(r.w as u32), Some(r.rows() as u32));
        }
        if let Err(e) = model::save_cfg(&f.dir, &cfg) {
            eprintln!("glass-folders: save config in {}: {e}", f.dir.display());
        }
        f.dir.clone()
    };
    rescan(panel, app, &root);
}

/// Inline rename committed (Enter). Empty / unchanged = keep name.
fn rename_group(panel: &Panel, app: &Shared, old: &str, typed: &str) {
    panel.set_rename_idx(-1);
    let name = model::clean_name(typed);
    let root = app.borrow().folder.as_ref().map(|f| f.dir.clone());
    if let (Some(root), false) = (root, name.is_empty() || name == old || old == model::LOOSE) {
        match model::rename_group(&root, old, &name) {
            Ok(n) => {
                let mut cfg = model::load_cfg(&root);
                if cfg.groups.contains_key(old) || cfg.order.iter().any(|o| o == old) {
                    cfg.rename_group(old, &n);
                    if let Err(e) = model::save_cfg(&root, &cfg) {
                        eprintln!("glass-folders: save config in {}: {e}", root.display());
                    }
                }
            }
            Err(e) => eprintln!("glass-folders: rename group {old} -> {name}: {e}"),
        }
        rescan(panel, app, &root);
    }
    panel.invoke_focus_keys();
}

/// Folder changed on disk: re-read, redraw panel and the folder's desktop tile.
fn rescan(panel: &Panel, app: &Shared, root: &Path) {
    app.borrow_mut().folder = Some(model::scan(root));
    render(panel, app);
    refresh_tile(app, root);
}

/// Tile glass look. Default: real live acrylic (win::glass + NC-active trick), only a matte layer
/// by glass level. Env GF_FAKE_GLASS=1: old fallback, frosted copy of wallpaper under the tile.
fn reblur(t: &Tile, dark: bool, glass: i32) {
    t.set_glass(glass);
    if std::env::var("GF_FAKE_GLASS").is_err() {
        return;
    }
    let p = t.window().position();
    let sz = t.window().size();
    let r = RECT { left: p.x, top: p.y, right: p.x + sz.width as i32, bottom: p.y + sz.height as i32 };
    if let Some(img) = win::wallpaper_blur(r, dark, glass) {
        t.set_backdrop(img);
    }
}

/// Folder (idx < 0) or group color. None = colorless.
fn set_color(panel: &Panel, app: &Shared, idx: i32, hx: Option<String>) {
    {
        let mut st = app.borrow_mut();
        let st = &mut *st;
        let Some(f) = st.folder.as_mut() else { return };
        if idx < 0 {
            f.cfg.color = hx;
        } else if let Some(g) = f.groups.get(idx as usize) {
            let name = g.name.clone();
            f.cfg.groups.entry(name).or_default().color = hx;
        }
        // merge into fresh cfg: tile rect may have changed since open
        let mut cfg = model::load_cfg(&f.dir);
        cfg.color = f.cfg.color.clone();
        for (n, g) in &f.cfg.groups {
            cfg.groups.entry(n.clone()).or_default().color = g.color.clone();
        }
        let _ = model::save_cfg(&f.dir, &cfg);
        let (dir, (c, on)) = (f.dir.clone(), model::tint(&f.tint()));
        for (d, t) in &st.tiles {
            if *d == dir {
                t.set_tint(c);
                t.set_colored(on);
            }
        }
    }
    render(panel, app);
}

fn open_folder(panel: &Panel, app: &Shared, dir: &Path) {
    // a fold-back still running (click on another tile closed the panel): finish it now
    finish_close(panel, app);
    {
        let mut st = app.borrow_mut();
        st.folder = Some(model::scan(dir));
        st.unfolded = None;
    }
    panel.set_page(0);
    render(panel, app);
    // size = same formula as Panel width/height in app.slint
    let s = panel.window().scale_factor();
    let w = ((panel.get_grid_w() + 32.0).max(panel.get_min_w()) * s).round() as i32;
    let h = ((panel.get_grid_h() + 64.0) * s).round() as i32;
    panel.window().set_size(PhysicalSize::new(w as u32, h as u32));
    // place: centered on the tile, kept inside the work area
    let wa = win::work_area();
    let m = (8.0 * s) as i32;
    let from = app.borrow().tile_rect(dir).unwrap_or_else(|| {
        let (cx, cy) = ((wa.left + wa.right) / 2, (wa.top + wa.bottom) / 2);
        RECT { left: cx - 40, top: cy - 40, right: cx + 40, bottom: cy + 40 }
    });
    let (cx, cy) = ((from.left + from.right) / 2, (from.top + from.bottom) / 2);
    let x = (cx - w / 2).clamp(wa.left + m, (wa.right - w - m).max(wa.left + m));
    let y = (cy - h / 2).clamp(wa.top + m, (wa.bottom - h - m).max(wa.top + m));
    let home = POINT { x, y };
    let to = RECT { left: x, top: y, right: x + w, bottom: y + h };
    plat::set_view(panel.window(), Some((home, from)));
    panel.set_reveal(0.0);
    let _ = panel.show();
    panel.invoke_focus_keys();
    animate(panel, app, home, from, to, OPEN_MS, true);
}

/// Panel shows a growing / shrinking part of itself (from -> to), content fades with it.
fn animate(panel: &Panel, app: &Shared, home: POINT, from: RECT, to: RECT, ms: f32, opening: bool) {
    let start = std::time::Instant::now();
    let (a, p) = (app.clone(), panel.as_weak());
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(8), move || {
        let Some(p) = p.upgrade() else { return };
        if a.borrow().anim.is_none() {
            return; // stopped
        }
        let k = (start.elapsed().as_secs_f32() * 1000.0 / ms).min(1.0);
        // open: ease-out (fast start, soft landing); close: ease-in
        let e = if opening { 1.0 - (1.0 - k).powi(3) } else { k * k };
        let mix = |a: i32, b: i32| a + ((b - a) as f32 * e).round() as i32;
        let r = RECT { left: mix(from.left, to.left), top: mix(from.top, to.top), right: mix(from.right, to.right), bottom: mix(from.bottom, to.bottom) };
        p.set_reveal(if opening { (k * 1.4).min(1.0) } else { 1.0 - k });
        plat::set_view(p.window(), Some((home, r)));
        if k >= 1.0 {
            if let Some(t) = a.borrow_mut().anim.take() {
                slint::Timer::single_shot(Duration::ZERO, move || drop(t));
            }
            if opening {
                plat::set_view(p.window(), None);
                p.set_reveal(1.0);
            } else {
                finish_close(&p, &a);
            }
        }
    });
    app.borrow_mut().anim = Some(t);
}

/// Start folding the panel back into its tile; hidden + cleared at the end.
fn close(panel: &Panel, app: &Shared) {
    let (home, from, to) = {
        let mut st = app.borrow_mut();
        if st.closing || st.folder.is_none() {
            return;
        }
        st.closing = true;
        let Some(cur) = plat::rect_of(panel.window()) else { return };
        let dir = st.folder.as_ref().map(|f| f.dir.clone()).unwrap_or_default();
        let to = st.tile_rect(&dir).unwrap_or_else(|| {
            let (cx, cy) = ((cur.left + cur.right) / 2, (cur.top + cur.bottom) / 2);
            RECT { left: cx - 40, top: cy - 40, right: cx + 40, bottom: cy + 40 }
        });
        st.anim.take(); // an unfinished open: fold from where it is now
        (POINT { x: cur.left, y: cur.top }, cur, to)
    };
    // home = content corner: if open was cut short, content corner is not the window corner
    let home = plat::view_home(panel.window()).unwrap_or(home);
    animate(panel, app, home, from, to, CLOSE_MS, false);
}

/// Hide panel and free everything (end of fold-back, or cut short by a new open).
fn finish_close(panel: &Panel, app: &Shared) {
    {
        let mut st = app.borrow_mut();
        st.anim.take();
        if !st.closing {
            if st.folder.is_some() {
                // open still running: just land it
                drop(st);
                plat::set_view(panel.window(), None);
                panel.set_reveal(1.0);
            }
            return;
        }
        st.closing = false;
    }
    let _ = panel.hide();
    plat::set_view(panel.window(), None);
    panel.set_groups(ModelRc::default());
    panel.set_rename_idx(-1);
    panel.set_drop_target(-2);
    panel.set_ghost_on(false);
    let mut st = app.borrow_mut();
    st.folder = None;
    st.ids.clear();
    st.id_group.clear();
    st.base.clear();
    st.groups = None;
    st.unfolded = None;
    reset_preview(&mut st);
    st.icons.clear(); // free icon memory while hidden
}

/// Push folder data + layout into the panel. All items go in (overflow "+N" is drawn by Slint,
/// so resizing a group shows more / fewer at once).
fn render(panel: &Panel, app: &Shared) {
    let mut st = app.borrow_mut();
    let st = &mut *st;
    let Some(f) = st.folder.as_ref() else { return };
    let mut rects = model::layout(f, st.set.gravity);
    // unfolded group: only while it really overflows
    let unfolded = st.unfolded.filter(|&i| f.groups.get(i).is_some_and(|g| (g.items.len() as i32) > rects[i].w * rects[i].rows()));
    st.unfolded = unfolded;
    if let Some(i) = unfolded {
        // all items + fold slot; wider if it would get very tall
        let n = f.groups[i].items.len() as i32 + 1;
        let w = rects[i].w.max((n + 5) / 6);
        rects[i] = GRect::slots(w, (n + w - 1) / w, rects[i].x, rects[i].y);
        model::resolve(&mut rects, Some(i));
        model::settle(&mut rects, st.set.gravity);
    }
    let icon_px = px(48.0, panel.window().scale_factor());
    st.ids.clear();
    st.id_group.clear();
    let mut groups = Vec::new();
    for (gi, (g, r)) in f.groups.iter().zip(&rects).enumerate() {
        let mut slots = Vec::new();
        for (k, path) in g.items.iter().enumerate() {
            let id = st.ids.len() as i32;
            st.ids.push(path.clone());
            st.id_group.push(gi);
            let icon = st.icons.entry(path.clone()).or_insert_with(|| win::icon(path, icon_px).unwrap_or_default()).clone();
            slots.push(SlotData { name: model::name_of(path).into(), icon, id, k: k as i32 });
        }
        let (tint, colored) = model::tint(&f.group_tint(gi));
        let [x, y, w, h] = r.px();
        groups.push(GroupData {
            name: g.name.clone().into(),
            tint,
            colored,
            fixed: g.dir == f.dir,
            idx: gi as i32,
            x,
            y,
            w,
            h,
            cols: r.w,
            rows: r.rows(),
            n: g.items.len() as i32,
            open: unfolded == Some(gi),
            items: ModelRc::new(VecModel::from(slots)),
        });
    }
    panel.set_folder_name(f.name.clone().into());
    let (c, on) = model::tint(&f.tint());
    panel.set_folder_tint(c);
    panel.set_folder_colored(on);
    st.base = rects;
    reset_preview(st);
    let (gw, gh) = model::grid_px(&st.base, 0, 0);
    panel.set_grid_w(gw);
    panel.set_grid_h(gh);
    panel.set_drop_target(-2);
    panel.set_ghost_on(false);
    // same groups: update rows in place (plates glide to new places); else new model
    if let Some(m) = st.groups.as_ref().filter(|m| m.row_count() == groups.len()) {
        for (i, g) in groups.into_iter().enumerate() {
            m.set_row_data(i, g);
        }
    } else {
        let m = Rc::new(VecModel::from(groups));
        panel.set_groups(ModelRc::from(m.clone()));
        st.groups = Some(m);
    }
}

/// Panel grew (unfolded group): keep it inside the work area.
fn fit_panel(panel: &Panel) {
    let s = panel.window().scale_factor();
    let w = ((panel.get_grid_w() + 32.0).max(panel.get_min_w()) * s).round() as i32;
    let h = ((panel.get_grid_h() + 64.0) * s).round() as i32;
    let p = panel.window().position();
    let wa = win::work_area();
    let m = (8.0 * s) as i32;
    let x = p.x.min(wa.right - w - m).max(wa.left + m);
    let y = p.y.min(wa.bottom - h - m).max(wa.top + m);
    if (x, y) != (p.x, p.y) {
        panel.window().set_position(PhysicalPosition::new(x, y));
    }
}
