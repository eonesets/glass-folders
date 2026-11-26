// Кратко: тестовый режим без настоящей мыши. Env GF_SCRIPT=файл — читает сценарий (open, move, down, up, shot…),
// шлёт события прямо в окна Slint, снимки окон пишет в PNG, в конце выходит с кодом 0 (всё ок) или 1 (ошибка).
//
// Script: one command per line, '#' = comment. Coords = logical px inside the target window content.
//   target panel | hint | tile NAME   window for pointer / shot commands (default panel = panels[0])
//   open NAME | close                 open folder NAME (desktop dir) in panels[0] / fold it back
//   move x y | down [right] | up [right] | click x y | rclick x y | leave
//   drag x1 y1 x2 y2 [steps]          press, move in steps (16 ms apart), release
//   key Escape|Return|Tab|Backspace|Delete | type TEXT
//   wait ms                           let timers / animations run
//   shot NAME                         target window content -> OUT/NAME.png
//   frames NAME ms                    every frame target renders in next ms -> OUT/NAME_000.png ... (non-blocking)
//   shown T | hidden T                fail unless window T (panel/hint/tile NAME) is visible / hidden
//   exists REL | missing REL          fail unless path REL (relative to desktop dir) exists / not
//   log TEXT
//   dragto x y tile NAME              drag from (x, y) onto the center of tile NAME (drop out of the window)
//   menu N                            next shortcut menu (right click) picks item N: 1 open, 2 rename,
//                                     3 icon, 4 delete, 5 uninstall (0 = dismiss). Target ask = rename box.
//   fits T                            fail unless window T's real OS size == its drawn size
//
// v1.1 scenario command contracts (to be implemented alongside v1.1 features; exact pass/fail semantics):
//   page N                            switches Panel page via the SAME Rust handler as clicking gear/back
//                                     (sets Panel.page = N AND runs fit_panel when opening settings, N == 1).
//   inwork T                          fails unless window T's OS rect (plat::rect_of) lies completely inside
//                                     win::work_area() (left >= wa.left, top >= wa.top, right <= wa.right,
//                                     bottom <= wa.bottom).
//   zoomend tile NAME                 fails unless the last recorded zoom-view rect of the target panel
//                                     (plat::last_zoom_rect(window)) exists and equals tile NAME's OS rect
//                                     within +-1 px on all 4 edges (left, top, right, bottom).
//   renders reset                     zeroes the real zview SoftwareRenderer::render() counter in plat.rs
//                                     (incremented inside GfWindow::draw whenever Slint renders during zview).
//   renders panel max N               fails if that real zview render counter for the panel exceeds N.
//   item panel <group> <name> [absent]
//                                     checks live panel model (App.folder / Panel groups) that an item with
//                                     file stem or file name <name> is present (or absent) in group <group>.
//   tileitem <folder> <name> [absent] checks live tile state (g().tile_items for <folder>) that an item with
//                                     file name or stem <name> is present (or absent).
//   extdrop tile NAME REL             simulates external drop (copy = false, move) of desktop-relative REL onto
//                                     tile NAME through the real drop handler (with tile/panel UI refresh);
//                                     fails if rejected (returns empty).
//   extdrop panel X Y REL             same onto open panel at logical coords (X, Y) (group under cursor or root).
//   extdrop_copy tile NAME REL | extdrop_copy panel X Y REL
//                                     same as extdrop with copy = true (Ctrl held); fails if rejected.
//   extdrop_deny tile NAME REL | extdrop_deny panel X Y REL
//                                     simulates external drop; fails unless drop is rejected (returns empty)
//                                     AND the filesystem is unchanged.
//   mkdir REL                         creates directory REL (and parents) under test desktop.
//   rmdir REL                         recursively removes directory REL under test desktop.
//   ren REL NEWNAME                   renames REL to sibling NEWNAME under test desktop.
//   touch REL                         creates dummy .bat file REL ("rem test") under test desktop.
//   rm REL                            deletes file REL under test desktop.
//   gone T                            fails unless window T is removed from g().tiles (for `tile NAME`) or
//                                     absent/hidden (distinct from `hidden`, which requires the window to exist).
//   autostart on|off                  verifies BOTH: (1) HKCU\<shell::autostart_key()> value "GlassFolders"
//                                     equals the quoted current exe path ("\"<exe>\"") for `on` or is absent
//                                     for `off`, AND (2) Panel's `autostart` property equals true / false.
//
// Settings UI layout contract for deterministic SwitchRow click (settings-autostart.gfs):
//   In app.slint Settings page (width >= 372px, pad = 16px, top = 48px), the "Start with Windows" /
//   "Запуск с Windows" SwitchRow is the 4th row (y = 132px, height = 44px) of the first "Folder"/"Папка" Card
//   (at x = pad = 16px, y = top + 24px = 72px, width = cw = width - 32px, height = 4 * 44px = 176px).
//   At panel width = 372px, the 40x20 Switch sits at window x = 302..342 (center 322), y = 216..236 (center 226).
//
// OUT = env GF_OUT, else <script dir>/out. Log: OUT/log.txt. Any failed command stops the run, exit 1.
// Env GF_TIMEOUT (s, default 60): whole run limit.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::ComponentHandle;

use crate::plat::{self, Ptr};

const STEP_MS: u64 = 20; // pause after each command: lets the event loop render

thread_local! {
    static EXIT: Cell<Option<i32>> = const { Cell::new(None) };
}

/// Exit code after the script finished (None = no script ran).
pub fn exit_code() -> Option<i32> {
    EXIT.with(|e| e.get())
}

#[derive(Clone, Debug)]
enum Cmd {
    Target(String),
    Open(String),
    Close,
    Ptr(Ptr, Option<(f32, f32)>),
    Key(String),
    Type(String),
    Wait(u64),
    Shot(String),
    Frames(String, u64),
    Shown(String, bool),
    Exists(String, bool),
    Log(String),
    Fits(String),
    Menu(u32),
    DragTo(f32, f32, String),
    Page(i32),
    InWork(String),
    ZoomEnd(String),
    RendersReset,
    RendersMax(String, usize),
    /// external drop: target, desktop-relative source, copy, expect refusal
    ExtDrop(DropTo, String, bool, bool),
    /// item panel GROUP NAME [absent]
    Item(String, String, bool),
    /// tileitem FOLDER NAME [absent]
    TileItem(String, String, bool),
    Fs(FsOp),
    Gone(String),
    Opt(String, i32),
    Autostart(bool),
}

#[derive(Clone, Debug)]
enum DropTo {
    Tile(String),
    Panel(f32, f32),
}

/// Desktop file system changes made while the app runs (watcher tests). Paths relative to the desktop.
#[derive(Clone, Debug)]
enum FsOp {
    Mkdir(String),
    Rmdir(String),
    Ren(String, String),
    Touch(String),
    Rm(String),
}

impl std::fmt::Debug for Ptr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ptr::Move => write!(f, "move"),
            Ptr::Down(b) => write!(f, "down {b:?}"),
            Ptr::Up(b) => write!(f, "up {b:?}"),
            Ptr::Leave => write!(f, "leave"),
        }
    }
}

struct Run {
    cmds: Vec<(usize, String, Cmd)>, // (line no, source text, command)
    i: usize,
    target: String,
    at: (f32, f32),
    desktop: PathBuf,
    out: PathBuf,
    log: String,
    start: Instant,
    done: bool,
}

fn num(s: Option<&str>) -> Result<f32, String> {
    s.ok_or("missing number")?.parse::<f32>().map_err(|e| e.to_string())
}

fn parse(text: &str) -> Result<Vec<(usize, String, Cmd)>, String> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut v = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (word, rest) = line.split_once(char::is_whitespace).map_or((line, ""), |(a, b)| (a, b.trim()));
        let mut a = rest.split_whitespace();
        let btn = |s: &str| if s == "right" { PointerEventButton::Right } else { PointerEventButton::Left };
        let err = |e: String| format!("line {}: {line}: {e}", n + 1);
        let mut push = |c: Cmd| v.push((n + 1, line.to_string(), c));
        match word {
            "target" => push(Cmd::Target(rest.into())),
            "open" => push(Cmd::Open(rest.into())),
            "close" => push(Cmd::Close),
            "move" | "click" | "rclick" => {
                let (x, y) = (num(a.next()).map_err(err)?, num(a.next()).map_err(err)?);
                push(Cmd::Ptr(Ptr::Move, Some((x, y))));
                let b = if word == "rclick" { PointerEventButton::Right } else { PointerEventButton::Left };
                if word != "move" {
                    push(Cmd::Ptr(Ptr::Down(b), None));
                    push(Cmd::Ptr(Ptr::Up(b), None));
                }
            }
            "down" => push(Cmd::Ptr(Ptr::Down(btn(rest)), None)),
            "up" => push(Cmd::Ptr(Ptr::Up(btn(rest)), None)),
            "leave" => push(Cmd::Ptr(Ptr::Leave, None)),
            "drag" => {
                let mut f = || num(a.next()).map_err(err);
                let (x1, y1, x2, y2) = (f()?, f()?, f()?, f()?);
                let steps = a.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(10).max(1);
                push(Cmd::Ptr(Ptr::Move, Some((x1, y1))));
                push(Cmd::Ptr(Ptr::Down(PointerEventButton::Left), None));
                for k in 1..=steps {
                    let t = k as f32 / steps as f32;
                    push(Cmd::Ptr(Ptr::Move, Some((x1 + (x2 - x1) * t, y1 + (y2 - y1) * t))));
                    push(Cmd::Wait(16));
                }
                push(Cmd::Ptr(Ptr::Up(PointerEventButton::Left), None));
            }
            "key" => push(Cmd::Key(rest.into())),
            "type" => push(Cmd::Type(rest.into())),
            "wait" => push(Cmd::Wait(num(Some(rest)).map_err(err)? as u64)),
            "shot" => push(Cmd::Shot(rest.into())),
            "frames" => {
                let name = a.next().ok_or_else(|| err("missing name".into()))?.to_string();
                push(Cmd::Frames(name, num(a.next()).map_err(err)? as u64));
            }
            "shown" | "hidden" => push(Cmd::Shown(rest.into(), word == "shown")),
            "exists" | "missing" => push(Cmd::Exists(rest.into(), word == "exists")),
            "log" => push(Cmd::Log(rest.into())),
            "fits" => push(Cmd::Fits(rest.into())),
            "dragto" => {
                // dragto x y tile NAME: press at (x, y) of the target window, drag onto that tile's center
                let mut a = rest.splitn(3, ' ');
                let x = num(a.next()).map_err(err)?;
                let y = num(a.next()).map_err(err)?;
                push(Cmd::DragTo(x, y, a.next().unwrap_or("").trim().to_string()));
            }
            "menu" => push(Cmd::Menu(num(Some(rest)).map_err(err)? as u32)),
            "page" => push(Cmd::Page(num(Some(rest)).map_err(err)? as i32)),
            "inwork" => push(Cmd::InWork(rest.into())),
            "zoomend" => push(Cmd::ZoomEnd(rest.into())),
            "renders" => {
                let sub = a.next().ok_or_else(|| err("missing renders subcommand".into()))?;
                if sub == "reset" {
                    push(Cmd::RendersReset);
                } else {
                    let kw = a.next().ok_or_else(|| err("expected 'max'".into()))?;
                    if kw != "max" {
                        return Err(err(format!("expected 'max', got '{kw}'")));
                    }
                    let max = num(a.next()).map_err(err)? as usize;
                    push(Cmd::RendersMax(sub.into(), max));
                }
            }
            "extdrop" | "extdrop_copy" | "extdrop_deny" => {
                // extdrop tile NAME REL | extdrop panel X Y REL (REL may hold spaces: rest of the line)
                let to = match a.next() {
                    Some("tile") => DropTo::Tile(a.next().ok_or_else(|| err("missing tile name".into()))?.to_string()),
                    Some("panel") => {
                        let x = num(a.next()).map_err(err)?;
                        DropTo::Panel(x, num(a.next()).map_err(err)?)
                    }
                    _ => return Err(err("expected tile NAME or panel X Y".into())),
                };
                let rel = a.collect::<Vec<_>>().join(" ");
                if rel.is_empty() {
                    return Err(err("missing path".into()));
                }
                push(Cmd::ExtDrop(to, rel, word == "extdrop_copy", word == "extdrop_deny"));
            }
            "item" | "tileitem" => {
                let w: Vec<&str> = a.collect();
                let absent = w.last() == Some(&"absent");
                let w = if absent { &w[..w.len() - 1] } else { &w[..] };
                if word == "item" {
                    if w.len() != 3 || w[0] != "panel" {
                        return Err(err("expected: item panel GROUP NAME [absent]".into()));
                    }
                    push(Cmd::Item(w[1].into(), w[2].into(), !absent));
                } else {
                    if w.len() != 2 {
                        return Err(err("expected: tileitem FOLDER NAME [absent]".into()));
                    }
                    push(Cmd::TileItem(w[0].into(), w[1].into(), !absent));
                }
            }
            "mkdir" => push(Cmd::Fs(FsOp::Mkdir(rest.into()))),
            "rmdir" => push(Cmd::Fs(FsOp::Rmdir(rest.into()))),
            "touch" => push(Cmd::Fs(FsOp::Touch(rest.into()))),
            "rm" => push(Cmd::Fs(FsOp::Rm(rest.into()))),
            "ren" => {
                let (from, to) = rest.split_once(char::is_whitespace).ok_or_else(|| err("expected: ren REL NEWNAME".into()))?;
                push(Cmd::Fs(FsOp::Ren(from.into(), to.trim().into())));
            }
            "gone" => push(Cmd::Gone(rest.into())),
            "opt" => {
                let k = a.next().ok_or_else(|| err("expected: opt KEY V".into()))?.to_string();
                let v = a.next().ok_or_else(|| err("expected: opt KEY V".into()))?.parse::<i32>().map_err(|e| err(e.to_string()))?;
                push(Cmd::Opt(k, v));
            }
            "autostart" => {
                let want = match rest {
                    "on" => true,
                    "off" => false,
                    _ => return Err(err("expected: autostart on|off".into())),
                };
                push(Cmd::Autostart(want));
            }
            _ => return Err(err("unknown command".into())),
        }
    }
    Ok(v)
}

/// Run `f` with the window named by `t` (panel | hint | tile NAME).
fn with_window<R>(t: &str, f: impl FnOnce(&slint::Window) -> R) -> Result<R, String> {
    let g = crate::g();
    match t.split_once(' ').map_or((t, ""), |(a, b)| (a, b.trim())) {
        ("panel", _) => crate::main_panel().map(|(p, _)| f(p.window())).ok_or_else(|| "no panel".into()),
        ("hint", _) => g.hint.borrow().as_ref().map(|h| f(h.window())).ok_or_else(|| "no hint".into()),
        ("fly", _) => g.fly.borrow().as_ref().map(|h| f(h.window())).ok_or_else(|| "no fly".into()),
        ("ask", _) => g.ask.borrow().as_ref().map(|h| f(h.window())).ok_or_else(|| "no rename box".into()),
        ("tile", name) => {
            let tiles = g.tiles.borrow();
            let t = tiles.iter().find(|(d, _)| d.file_name().is_some_and(|n| n.to_string_lossy() == name));
            t.map(|(_, t)| f(t.window())).ok_or_else(|| format!("no tile {name}"))
        }
        _ => Err(format!("bad target '{t}'")),
    }
}

/// File `p` is called `name` (file name or name without extension).
fn is_named(p: &Path, name: &str) -> bool {
    p.file_name().is_some_and(|n| n.to_string_lossy() == name) || p.file_stem().is_some_and(|n| n.to_string_lossy() == name)
}

/// All paths under `dir` (relative, sorted): "did anything change on disk".
fn tree(dir: &Path) -> Vec<PathBuf> {
    fn walk(base: &Path, d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.strip_prefix(base).unwrap_or(&p).to_path_buf());
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(base, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn key_of(name: &str) -> Option<Key> {
    Some(match name {
        "Escape" => Key::Escape,
        "Return" => Key::Return,
        "Tab" => Key::Tab,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        _ => return None,
    })
}

/// Do one command. Ok(pause ms before the next one).
fn exec(r: &mut Run, c: &Cmd) -> Result<u64, String> {
    match c {
        Cmd::Target(t) => {
            with_window(t, |_| ())?;
            r.target = t.clone();
        }
        Cmd::Open(name) => {
            let dir = r.desktop.join(name);
            if !dir.is_dir() {
                return Err(format!("no folder {}", dir.display()));
            }
            let (p, a) = crate::main_panel().ok_or("no panel")?;
            crate::open_folder(&p, &a, &dir);
        }
        Cmd::Close => {
            let (p, a) = crate::main_panel().ok_or("no panel")?;
            crate::close(&p, &a);
        }
        Cmd::Ptr(ev, pos) => {
            if let Some(p) = pos {
                r.at = *p;
            }
            let (x, y) = r.at;
            if !with_window(&r.target, |w| plat::inject(w, *ev, x, y))? {
                return Err("window not ours".into());
            }
        }
        Cmd::Key(k) => {
            let k = key_of(k).ok_or_else(|| format!("unknown key {k}"))?;
            with_window(&r.target, |w| {
                w.dispatch_event(WindowEvent::KeyPressed { text: k.into() });
                w.dispatch_event(WindowEvent::KeyReleased { text: k.into() });
            })?;
        }
        Cmd::Type(t) => {
            with_window(&r.target, |w| {
                for ch in t.chars() {
                    let s: slint::SharedString = ch.to_string().into();
                    w.dispatch_event(WindowEvent::KeyPressed { text: s.clone() });
                    w.dispatch_event(WindowEvent::KeyReleased { text: s });
                }
            })?;
        }
        Cmd::Wait(ms) => return Ok(*ms),
        Cmd::Shot(name) => {
            let path = r.out.join(format!("{name}.png"));
            let (w, h) = with_window(&r.target, |win| plat::shot(win, &path))??;
            r.note(&format!("  -> {} ({w}x{h})", path.display()));
        }
        Cmd::Frames(name, ms) => {
            if !with_window(&r.target, |w| plat::record(w, *ms))? {
                return Err("window not ours".into());
            }
            let (out, name) = (r.out.clone(), name.clone());
            slint::Timer::single_shot(Duration::from_millis(ms + 20), move || {
                for (i, (w, h, px)) in plat::take_frames().iter().enumerate() {
                    let _ = plat::save_png(&out.join(format!("{name}_{i:03}.png")), *w, *h, px);
                }
            });
        }
        Cmd::Shown(t, want) => {
            let on = with_window(t, plat::is_shown)?;
            if on != *want {
                return Err(format!("{t} is {}", if on { "shown" } else { "hidden" }));
            }
        }
        Cmd::Exists(rel, want) => {
            let on = r.desktop.join(rel).exists();
            if on != *want {
                return Err(format!("{rel} {}", if on { "exists" } else { "missing" }));
            }
        }
        Cmd::Log(t) => r.note(&format!("  {t}")),
        Cmd::Menu(id) => plat::set_menu_pick(*id),
        Cmd::DragTo(x, y, to) => {
            // end point = center of window `to`, in target window coords (both have real screen rects)
            let dst = with_window(to, |w| plat::rect_of(w))?.ok_or("no window")?;
            let (src, s) = with_window(&r.target, |w| (plat::rect_of(w), w.scale_factor()))?;
            let src = src.ok_or("no window")?;
            let ex = ((dst.left + dst.right) as f32 / 2.0 - src.left as f32) / s;
            let ey = ((dst.top + dst.bottom) as f32 / 2.0 - src.top as f32) / s;
            let (x, y) = (*x, *y);
            let mut steps = vec![(Ptr::Move, x, y), (Ptr::Down(PointerEventButton::Left), x, y)];
            for i in 1..=12 {
                let t = i as f32 / 12.0;
                steps.push((Ptr::Move, x + (ex - x) * t, y + (ey - y) * t));
            }
            steps.push((Ptr::Up(PointerEventButton::Left), ex, ey));
            for (ev, px, py) in steps {
                with_window(&r.target, |w| plat::inject(w, ev, px, py))?;
            }
            r.at = (ex, ey);
        }
        Cmd::Fits(t) => {
            // real OS window size == what Slint draws (a framed window kept >= ~39px tall: empty strip)
            let (real, drawn) = with_window(t, |w| (plat::rect_of(w), w.size()))?;
            let real = real.ok_or("no window")?;
            let (rw, rh) = ((real.right - real.left) as u32, (real.bottom - real.top) as u32);
            r.note(&format!("  {t}: window {rw}x{rh}, drawn {}x{}", drawn.width, drawn.height));
            if (rw, rh) != (drawn.width, drawn.height) {
                return Err(format!("{t} window {rw}x{rh} != drawn {}x{}", drawn.width, drawn.height));
            }
        }
        Cmd::Page(n) => {
            let (p, _) = crate::main_panel().ok_or("no panel")?;
            crate::set_panel_page(&p, *n);
        }
        Cmd::InWork(t) => {
            let rect = with_window(t, plat::rect_of)?.ok_or("no window")?;
            let wa = crate::win::work_area();
            r.note(&format!(
                "  {t}: [{},{},{},{}] in wa [{},{},{},{}]",
                rect.left, rect.top, rect.right, rect.bottom, wa.left, wa.top, wa.right, wa.bottom
            ));
            if rect.left < wa.left || rect.top < wa.top || rect.right > wa.right || rect.bottom > wa.bottom {
                return Err(format!(
                    "{t} [{},{},{},{}] outside work_area [{},{},{},{}]",
                    rect.left, rect.top, rect.right, rect.bottom, wa.left, wa.top, wa.right, wa.bottom
                ));
            }
        }
        Cmd::ZoomEnd(t) => {
            let got = with_window(&r.target, plat::last_zoom_rect)?
                .ok_or_else(|| format!("no last_zoom_rect on {}", r.target))?;
            let want = with_window(t, plat::rect_of)?.ok_or_else(|| format!("no window {t}"))?;
            r.note(&format!(
                "  zoomend: got [{},{},{},{}] vs {t} [{},{},{},{}]",
                got.left, got.top, got.right, got.bottom, want.left, want.top, want.right, want.bottom
            ));
            if (got.left - want.left).abs() > 1
                || (got.top - want.top).abs() > 1
                || (got.right - want.right).abs() > 1
                || (got.bottom - want.bottom).abs() > 1
            {
                return Err(format!(
                    "zoomend [{},{},{},{}] != {t} [{},{},{},{}]",
                    got.left, got.top, got.right, got.bottom, want.left, want.top, want.right, want.bottom
                ));
            }
        }
        Cmd::ExtDrop(to, rel, copy, deny) => {
            let src = r.desktop.join(rel);
            let before = tree(&r.desktop);
            // real drop handler of that window (OLE path minus OLE); window cloned out: the drop may
            // add / remove tiles (borrows of g().tiles must not be held)
            let taken = match to {
                DropTo::Tile(name) => {
                    let t = crate::g()
                        .tiles
                        .borrow()
                        .iter()
                        .find(|(d, _)| d.file_name().is_some_and(|n| n.to_string_lossy() == name.as_str()))
                        .map(|(_, t)| t.clone_strong());
                    let t = t.ok_or_else(|| format!("no tile {name}"))?;
                    plat::drop_at(t.window(), 0.0, 0.0, &[src], *copy)
                }
                DropTo::Panel(x, y) => {
                    let (p, _) = crate::main_panel().ok_or("no panel")?;
                    plat::drop_at(p.window(), *x, *y, &[src], *copy)
                }
            };
            match (*deny, taken) {
                (false, false) => return Err(format!("drop of {rel} refused")),
                (true, true) => return Err(format!("drop of {rel} accepted, expected refusal")),
                (true, false) if tree(&r.desktop) != before => return Err("refused drop changed files".into()),
                _ => {}
            }
        }
        Cmd::Item(group, name, want) => {
            let (_, a) = crate::main_panel().ok_or("no panel")?;
            let st = a.borrow();
            let f = st.folder.as_ref().ok_or("no folder in panel")?;
            let on = f.groups.iter().filter(|g| g.name == *group).any(|g| g.items.iter().any(|p| is_named(p, name)));
            if !*want && on {
                return Err(format!("{name} still in group {group}"));
            }
            if *want && !on {
                let have: Vec<String> = f.groups.iter().map(|g| format!("{}: {:?}", g.name, g.items.iter().filter_map(|p| p.file_name()).collect::<Vec<_>>())).collect();
                return Err(format!("{name} not in group {group} ({})", have.join("; ")));
            }
        }
        Cmd::TileItem(folder, name, want) => {
            let g = crate::g();
            let items = g.tile_items.borrow();
            let list = items
                .iter()
                .find(|(d, _)| d.file_name().is_some_and(|n| n.to_string_lossy() == folder.as_str()))
                .map(|(_, v)| v.clone())
                .ok_or_else(|| format!("no tile items for {folder}"))?;
            let on = list.iter().any(|p| is_named(p, name));
            if on != *want {
                return Err(format!("tile {folder}: {name} {}", if on { "present" } else { "missing" }));
            }
        }
        Cmd::Fs(op) => {
            let d = &r.desktop;
            let res = match op {
                FsOp::Mkdir(rel) => std::fs::create_dir_all(d.join(rel)),
                FsOp::Rmdir(rel) => std::fs::remove_dir_all(d.join(rel)),
                FsOp::Ren(rel, new) => {
                    let from = d.join(rel);
                    let to = from.parent().unwrap_or(d).join(new);
                    std::fs::rename(&from, &to)
                }
                FsOp::Touch(rel) => std::fs::write(d.join(rel), "rem test"),
                FsOp::Rm(rel) => std::fs::remove_file(d.join(rel)),
            };
            res.map_err(|e| format!("{op:?}: {e}"))?;
        }
        Cmd::Gone(t) => {
            let present = match t.split_once(' ') {
                // tile: must be dropped from the app's tile list (not merely hidden)
                Some(("tile", name)) => crate::g()
                    .tiles
                    .borrow()
                    .iter()
                    .any(|(d, _)| d.file_name().is_some_and(|n| n.to_string_lossy() == name.trim())),
                _ => with_window(t, plat::is_shown).unwrap_or(false),
            };
            if present {
                return Err(format!("{t} still there"));
            }
        }
        Cmd::RendersReset => plat::reset_zview_renders(),
        Cmd::RendersMax(t, max) => {
            let _ = with_window(t, |_| ())?;
            let n = plat::zview_renders();
            r.note(&format!("  renders {t}: {n} (max {max})"));
            if n > *max {
                return Err(format!("renders {t}: {n} > max {max}"));
            }
        }
        Cmd::Opt(k, v) => crate::set_opt(k, *v),
        Cmd::Autostart(want) => {
            let key = crate::shell::autostart_key();
            let reg_on = crate::shell::autostart_enabled();
            let reg_val = crate::shell::autostart_value_at(&key);
            if reg_on != *want {
                return Err(format!("autostart_enabled() = {reg_on}, expected {want} (key {key})"));
            }
            if *want {
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                let expected = format!("\"{}\"", exe.display());
                if reg_val.as_deref() != Some(expected.as_str()) {
                    return Err(format!("registry GlassFolders = {reg_val:?}, expected {expected:?}"));
                }
            } else if reg_val.is_some() {
                return Err(format!("registry GlassFolders still present: {reg_val:?}"));
            }
            let (p, _) = crate::main_panel().ok_or("no panel")?;
            let ui_on = p.get_autostart();
            if ui_on != *want {
                return Err(format!("panel autostart = {ui_on}, expected {want}"));
            }
        }
    }
    Ok(STEP_MS)
}

impl Run {
    fn note(&mut self, s: &str) {
        self.log.push_str(&format!("[{:>6} ms] {s}\n", self.start.elapsed().as_millis()));
    }

    fn finish(&mut self, code: i32) {
        if self.done {
            return;
        }
        self.done = true;
        // frames still being kept: write them first
        let frames = plat::take_frames();
        if !frames.is_empty() {
            self.note(&format!("  ({} frames dropped: run ended while recording)", frames.len()));
        }
        self.note(if code == 0 { "PASS" } else { "FAIL" });
        let _ = std::fs::write(self.out.join("log.txt"), &self.log);
        EXIT.with(|e| e.set(Some(code)));
        let _ = slint::quit_event_loop();
    }
}

fn step(run: Rc<RefCell<Run>>) {
    let mut r = run.borrow_mut();
    if r.done {
        return;
    }
    let Some((line, src, cmd)) = r.cmds.get(r.i).cloned() else {
        r.finish(0);
        return;
    };
    r.i += 1;
    let res = exec(&mut r, &cmd);
    match res {
        Ok(ms) => {
            if !matches!(cmd, Cmd::Wait(16)) {
                r.note(&format!("{line}: {src}"));
            }
            drop(r);
            slint::Timer::single_shot(Duration::from_millis(ms), move || step(run));
        }
        Err(e) => {
            r.note(&format!("{line}: {src}  ERROR: {e}"));
            r.finish(1);
        }
    }
}

/// Start the scenario after the app is set up. Bad script / output dir -> exit 1 at once.
pub fn start(script: &Path, desktop: PathBuf) {
    let out = std::env::var("GF_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| script.parent().unwrap_or(Path::new(".")).join("out"));
    let _ = std::fs::create_dir_all(&out);
    let mut run = Run {
        cmds: Vec::new(),
        i: 0,
        target: "panel".into(),
        at: (0.0, 0.0),
        desktop,
        out,
        log: String::new(),
        start: Instant::now(),
        done: false,
    };
    run.note(&format!("script {}", script.display()));
    match std::fs::read_to_string(script).map_err(|e| e.to_string()).and_then(|t| parse(&t)) {
        Ok(c) => run.cmds = c,
        Err(e) => {
            run.note(&format!("ERROR: {e}"));
            run.finish(1);
            return;
        }
    }
    let run = Rc::new(RefCell::new(run));
    let secs = std::env::var("GF_TIMEOUT").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
    let r2 = run.clone();
    slint::Timer::single_shot(Duration::from_secs(secs), move || {
        let mut r = r2.borrow_mut();
        r.note(&format!("ERROR: timeout {secs} s"));
        r.finish(1);
    });
    // let tiles + panels come up first
    slint::Timer::single_shot(Duration::from_millis(200), move || step(run));
}
