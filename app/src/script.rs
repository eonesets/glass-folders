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
