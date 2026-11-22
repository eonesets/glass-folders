// Кратко: точка входа. Плитки папок со стола + одно окно открытой папки (прячется, не пересоздаётся).

#![windows_subsystem = "windows"]

slint::include_modules!();
mod model;
mod plat;
mod win;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Image, ModelRc, PhysicalPosition, PhysicalSize, VecModel};

// Icons are requested at exact on-screen pixel size: big sizes make the shell add a frame around low-res icons.
fn px(logical: f32, scale: f32) -> i32 {
    (logical * scale).round() as i32
}

#[derive(Default)]
struct App {
    folder: Option<model::Folder>,
    ids: Vec<PathBuf>, // slot id -> file
    icons: HashMap<PathBuf, Image>,
    set: model::Settings,
    dark: bool,
    tiles: Vec<(PathBuf, Tile)>,
}

type Shared = Rc<RefCell<App>>;

fn main() -> Result<(), slint::PlatformError> {
    slint::platform::set_platform(Box::new(plat::GfPlatform::new())).expect("platform");
    win::com_init();
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
    panel.set_glass(app.borrow().set.glass);
    let pal: Vec<slint::Color> = model::PAL.iter().map(|h| model::color(h)).collect();
    panel.set_pal(ModelRc::new(VecModel::from(pal)));

    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_launch(move |id| {
            let path = a.borrow().ids.get(id as usize).cloned();
            if let Some(path) = path {
                win::open(&path);
            }
            if let Some(p) = p.upgrade() {
                close(&p, &a);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_open_group(move |idx| {
            let dir = a.borrow().folder.as_ref().and_then(|f| f.groups.get(idx as usize)).map(|g| g.dir.clone());
            if let Some(dir) = dir {
                win::open(&dir);
            }
            if let Some(p) = p.upgrade() {
                close(&p, &a);
            }
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
        panel.on_set_glass(move |lvl| {
            {
                let mut st = a.borrow_mut();
                st.set.glass = lvl;
                model::save_settings(&st.set);
            }
            if let Some(p) = p.upgrade() {
                p.set_glass(lvl);
            }
            let st = a.borrow();
            for (_, t) in &st.tiles {
                reblur(t, st.dark, lvl);
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
        panel.on_toggle_labels(move || {
            let on = {
                let mut st = a.borrow_mut();
                st.set.labels = !st.set.labels;
                model::save_settings(&st.set);
                st.set.labels
            };
            if let Some(p) = p.upgrade() {
                p.set_labels(on);
            }
        });
    }
    panel.on_quit(|| {
        let _ = slint::quit_event_loop();
    });

    // Desktop tiles: one per folder on Desktop.
    let glass = app.borrow().set.glass;
    for (i, dir) in model::list_folders(&desktop).into_iter().enumerate() {
        let t = Tile::new()?;
        let f = model::scan(&dir);
        t.set_dark(dark);
        t.set_name(f.name.clone().into());
        let (c, on) = model::tint(&f.tint());
        t.set_tint(c);
        t.set_colored(on);
        // enough icons for a big tile; Slint shows only what fits
        let icons: Vec<Image> = f.first_items(24).iter().filter_map(|p| win::icon(p, px(36.0, t.window().scale_factor()))).collect();
        t.set_icons(ModelRc::new(VecModel::from(icons)));
        {
            let (a, p, d) = (app.clone(), panel.as_weak(), dir.clone());
            t.on_open(move || {
                if let Some(p) = p.upgrade() {
                    open_folder(&p, &a, &d);
                }
            });
        }
        plat::set_kind(t.window(), plat::Kind::Tile);
        if let Some(h) = plat::hwnd_of(t.window()) {
            win::glass(h, dark);
        }
        let s = t.window().scale_factor();
        let (side_w, side_h) = plat::snap_size(0, 0, s); // min tile = 2x2 cells
        let wa = win::work_area();
        let (mut x, mut y, mut w, mut h) =
            (wa.right - ((24.0 + 112.0) * s) as i32, wa.top + ((24.0 + i as f32 * 128.0) * s) as i32, side_w, side_h);
        if let Some([sx, sy, sw, sh]) = f.cfg.tile {
            (x, y, w, h) = (sx, sy, sw, sh);
        }
        // dev: GF_TILE_AT=x,y puts tiles in a row from that point (to inspect the frosted look)
        if let Some((ax, ay)) = std::env::var("GF_TILE_AT").ok().and_then(|v| v.split_once(',').map(|(a, b)| (a.parse::<i32>().unwrap_or(0), b.parse::<i32>().unwrap_or(0)))) {
            (x, y, w, h) = (ax + i as i32 * (side_w + 16), ay, side_w, side_h);
        }
        // saved or default rect -> whole cells / desktop grid (dev GF_TILE_AT stays free)
        if std::env::var("GF_TILE_AT").is_err() {
            (w, h) = plat::snap_size(w, h, s);
            (x, y) = plat::snap_pos(x, y, s);
        }
        t.window().set_size(PhysicalSize::new(w as u32, h as u32));
        t.window().set_position(PhysicalPosition::new(x, y));
        reblur(&t, dark, glass);
        {
            // user moved/resized the tile: remember it, re-blur what is under it now
            let (a, tw, d) = (app.clone(), t.as_weak(), dir.clone());
            plat::set_on_moved(t.window(), move |r| {
                let mut cfg = model::scan(&d).cfg;
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

    slint::run_event_loop()
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
    let r = windows::Win32::Foundation::RECT { left: p.x, top: p.y, right: p.x + sz.width as i32, bottom: p.y + sz.height as i32 };
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
        let _ = model::save_cfg(&f.dir, &f.cfg);
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
    app.borrow_mut().folder = Some(model::scan(dir));
    render(panel, app);
    // center on work area (size = same formula as Panel width/height in app.slint)
    let s = panel.window().scale_factor();
    let w = ((panel.get_grid_w() + 32.0) * s) as i32;
    let h = ((panel.get_grid_h() + 64.0) * s) as i32;
    let wa = win::work_area();
    let x = wa.left + (wa.right - wa.left - w) / 2;
    let y = wa.top + (wa.bottom - wa.top - h) / 2;
    panel.window().set_position(PhysicalPosition::new(x, y));
    let _ = panel.show();
    panel.invoke_focus_keys();
}

fn close(panel: &Panel, app: &Shared) {
    let _ = panel.hide();
    panel.set_groups(ModelRc::default());
    let mut st = app.borrow_mut();
    st.folder = None;
    st.ids.clear();
    st.icons.clear(); // free icon memory while hidden
}

/// Push folder data + layout into the panel.
fn render(panel: &Panel, app: &Shared) {
    let mut st = app.borrow_mut();
    let st = &mut *st;
    let Some(f) = st.folder.as_ref() else { return };
    let lay = model::layout(f);
    let icon_px = px(48.0, panel.window().scale_factor());
    st.ids.clear();
    let mut groups = Vec::new();
    for (gi, (g, geo)) in f.groups.iter().zip(&lay.groups).enumerate() {
        let mut slots = Vec::new();
        for s in &geo.slots {
            let mut d = SlotData {
                x: s.x,
                y: s.y,
                more: s.more as i32,
                id: -1,
                ..Default::default()
            };
            if s.more == 0 {
                let path = &g.items[s.item];
                d.name = model::name_of(path).into();
                d.id = st.ids.len() as i32;
                st.ids.push(path.clone());
                d.icon = st
                    .icons
                    .entry(path.clone())
                    .or_insert_with(|| win::icon(path, icon_px).unwrap_or_default())
                    .clone();
            }
            slots.push(d);
        }
        let (tint, colored) = model::tint(&f.group_tint(gi));
        groups.push(GroupData {
            name: g.name.clone().into(),
            tint,
            colored,
            idx: gi as i32,
            x: geo.x,
            y: geo.y,
            w: geo.w,
            h: geo.h,
            items: ModelRc::new(VecModel::from(slots)),
        });
    }
    panel.set_folder_name(f.name.clone().into());
    let (c, on) = model::tint(&f.tint());
    panel.set_folder_tint(c);
    panel.set_folder_colored(on);
    panel.set_grid_w(lay.w);
    panel.set_grid_h(lay.h);
    panel.set_labels(st.set.labels);
    panel.set_groups(ModelRc::new(VecModel::from(groups)));
}
