// Кратко: тесты v1.1 (TDD, пишутся до кода): анимация zoom, масштаб картинки, кэш снимка, внешний дроп,
// автоподхват папок, автозапуск в реестре, порядок окон при Win+D. Спека: .pipeline/SPEC.md.
//
// API contracts these tests expect (production code must provide them):
//   A1 crate::zoom_rect(tile: RECT, panel: RECT, p: f32) -> RECT        exact tile at p=0, panel at p=1, per-edge lerp
//   A2 plat::scale_pixels(src: &[Bgra], sw, sh, dst: &mut [Bgra], dw, dh, fade: f32)   pub(crate); Bgra fields b,g,r,a pub(crate)
//      aspect kept (uniform scale, centered), premultiplied * fade, every dst pixel rewritten
//   A4 plat::ZoomSnap: Default, frame(&mut self) -> bool, mark_dirty(&mut self), clear(&mut self)
//   B  model::can_drop_into(src, dst_dir) -> bool; model::copy_into(src, dst_dir) -> io::Result<PathBuf>;
//      model::external_drop(srcs: &[PathBuf], dst_dir, desktop, copy: bool) -> Vec<PathBuf>;
//      plat::drop_effect(has_hdrop: bool, ctrl: bool, allowed: bool) -> u32 (DROPEFFECT_NONE/COPY/MOVE.0)
//   C  model::sync_plan(desktop, current: &[PathBuf]) -> FolderDiff { added, removed, kept: Vec<PathBuf> };
//      model::ignored_change(rel: &Path) -> bool
//   D  shell::autostart_key() -> String; shell::autostart_enabled() / set_autostart(on) (key = autostart_key());
//      shell::autostart_enabled_at(key) -> bool; shell::set_autostart_at(key, on) -> bool   (key = subkey under HKCU);
//      win::tray_icon_rgba() -> (u32, u32, Vec<u8>);
//      win::tray_menu_items(en: bool, autostart: bool) -> Vec<(u32, String, bool, bool)>  (id, label, enabled, checked; 0=sep)
//   E  desk::desktop_host() -> Option<HWND>; desk::is_desktop_class(&str) -> bool;
//      plat::ZPlace { Bottom, AboveHost } (Debug, PartialEq, Clone, Copy);
//      plat::z_place(kind: Kind, pinned: bool, show_desktop: bool) -> Option<ZPlace>; plat::z_layer(kind, pinned) -> Option<u8>

use std::fs;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::RECT;

use crate::model;
use crate::plat::{self, Bgra, Kind, ZPlace, ZoomSnap};

// ---------- helpers ----------

fn rect(l: i32, t: i32, w: i32, h: i32) -> RECT {
    RECT { left: l, top: t, right: l + w, bottom: t + h }
}

fn edges(r: RECT) -> [i32; 6] {
    [r.left, r.top, r.right, r.bottom, r.right - r.left, r.bottom - r.top]
}

/// Temp dir removed on drop (also when a test fails).
struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("gf_v11_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
    fn dir(&self, rel: &str) -> PathBuf {
        let d = self.p(rel);
        fs::create_dir_all(&d).unwrap();
        d
    }
    fn file(&self, rel: &str, text: &str) -> PathBuf {
        let f = self.p(rel);
        if let Some(d) = f.parent() {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(&f, text).unwrap();
        f
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn set_uses(root: &Path, name: &str, n: u32) {
    let mut c = model::load_cfg(root);
    c.uses.insert(name.into(), n);
    model::save_cfg(root, &c).unwrap();
}

fn uses(root: &Path, name: &str) -> Option<u32> {
    model::load_cfg(root).uses.get(name).copied()
}

fn sorted(mut v: Vec<PathBuf>) -> Vec<PathBuf> {
    v.sort();
    v
}

// ---------- A1: zoom_rect ----------

const TILE: RECT = RECT { left: 1000, top: 500, right: 1108, bottom: 614 }; // 2x2 tile 108x114
const SETTINGS_2X2: RECT = RECT { left: 900, top: 250, right: 1272, bottom: 1052 }; // 372x802
const BIG: RECT = RECT { left: 300, top: 120, right: 1500, bottom: 1020 }; // 1200x900

fn assert_near(a: RECT, b: RECT, what: &str) {
    for (x, y) in edges(a).iter().zip(edges(b)) {
        assert!((x - y).abs() <= 1, "{what}: {:?} != {:?}", edges(a), edges(b));
    }
}

#[test]
fn zoom_rect_starts_at_tile_small_settings() {
    assert_near(crate::zoom_rect(TILE, SETTINGS_2X2, 0.0), TILE, "p=0 must be the tile (2x2 settings)");
}

#[test]
fn zoom_rect_ends_at_panel_small_settings() {
    assert_near(crate::zoom_rect(TILE, SETTINGS_2X2, 1.0), SETTINGS_2X2, "p=1 must be the panel (2x2 settings)");
}

#[test]
fn zoom_rect_starts_at_tile_big_folder() {
    assert_near(crate::zoom_rect(TILE, BIG, 0.0), TILE, "p=0 must be the tile (big folder)");
}

#[test]
fn zoom_rect_ends_at_panel_big_folder() {
    assert_near(crate::zoom_rect(TILE, BIG, 1.0), BIG, "p=1 must be the panel (big folder)");
}

/// Every edge and the size move one way only (no 1 px back-and-forth = shake).
fn check_monotonic(tile: RECT, panel: RECT) {
    let n = 600;
    let frames: Vec<[i32; 6]> = (0..=n).map(|i| edges(crate::zoom_rect(tile, panel, i as f32 / n as f32))).collect();
    let (from, to) = (edges(tile), edges(panel));
    for k in 0..6 {
        let dir = (to[k] - from[k]).signum();
        for w in frames.windows(2) {
            let step = w[1][k] - w[0][k];
            if dir == 0 {
                assert_eq!(step, 0, "edge {k} must stay put: {:?} -> {:?}", w[0], w[1]);
            } else {
                assert!(step * dir >= 0, "edge {k} goes backwards: {:?} -> {:?} (tile {:?}, panel {:?})", w[0], w[1], from, to);
            }
        }
    }
    for f in &frames {
        assert!(f[4] >= 1 && f[5] >= 1, "empty rect {f:?}");
    }
}

#[test]
fn zoom_rect_monotonic_small_settings() {
    check_monotonic(TILE, SETTINGS_2X2);
}

#[test]
fn zoom_rect_monotonic_big_folder() {
    check_monotonic(TILE, BIG);
}

#[test]
fn zoom_rect_monotonic_tile_right_of_panel() {
    // edges move in opposite directions: left/top go up-left, right/bottom grow
    check_monotonic(rect(2300, 1200, 108, 114), rect(1400, 300, 900, 700));
}

#[test]
fn zoom_rect_is_linear_per_edge() {
    // halfway = halfway on every edge (lerp(a, b, p).round())
    let m = crate::zoom_rect(TILE, SETTINGS_2X2, 0.5);
    let mid = |a: i32, b: i32| ((a + b) as f32 / 2.0).round() as i32;
    let want = RECT {
        left: mid(TILE.left, SETTINGS_2X2.left),
        top: mid(TILE.top, SETTINGS_2X2.top),
        right: mid(TILE.right, SETTINGS_2X2.right),
        bottom: mid(TILE.bottom, SETTINGS_2X2.bottom),
    };
    assert_near(m, want, "p=0.5");
}

// ---------- A2: scale_pixels ----------

fn px(r: u8, g: u8, b: u8, a: u8) -> Bgra {
    Bgra { b, g, r, a }
}

/// Transparent sw x sh image with an opaque white square of side `side` in the middle.
fn square_img(sw: usize, sh: usize, side: usize) -> Vec<Bgra> {
    let mut v = vec![Bgra::default(); sw * sh];
    let (x0, y0) = ((sw - side) / 2, (sh - side) / 2);
    for y in y0..y0 + side {
        for x in x0..x0 + side {
            v[y * sw + x] = px(255, 255, 255, 255);
        }
    }
    v
}

/// Bounding box (x0, y0, x1, y1) of pixels with alpha >= 128.
fn bbox(d: &[Bgra], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let mut b: Option<(usize, usize, usize, usize)> = None;
    for y in 0..h {
        for x in 0..w {
            if d[y * w + x].a >= 128 {
                b = Some(match b {
                    None => (x, y, x, y),
                    Some((a, c, e, f)) => (a.min(x), c.min(y), e.max(x), f.max(y)),
                });
            }
        }
    }
    b
}

fn scale(src: &[Bgra], sw: usize, sh: usize, dw: usize, dh: usize, fade: f32) -> Vec<Bgra> {
    let mut dst = vec![Bgra::default(); dw * dh];
    plat::scale_pixels(src, sw, sh, &mut dst, dw, dh, fade);
    dst
}

fn check_square_kept(sw: usize, sh: usize, side: usize, dw: usize, dh: usize) {
    let d = scale(&square_img(sw, sh, side), sw, sh, dw, dh, 1.0);
    let (x0, y0, x1, y1) = bbox(&d, dw, dh).expect("square vanished");
    let (bw, bh) = ((x1 - x0 + 1) as i32, (y1 - y0 + 1) as i32);
    assert!(bw >= 8 && bh >= 8, "square too small: {bw}x{bh}");
    assert!((bw - bh).abs() <= 2, "{sw}x{sh} -> {dw}x{dh}: square became {bw}x{bh} (stretched)");
    let (cx, cy) = ((x0 + x1) as f32 / 2.0, (y0 + y1) as f32 / 2.0);
    let (mx, my) = ((dw - 1) as f32 / 2.0, (dh - 1) as f32 / 2.0);
    assert!((cx - mx).abs() <= 2.0 && (cy - my).abs() <= 2.0, "not centered: ({cx}, {cy}) vs ({mx}, {my})");
}

#[test]
fn scale_pixels_keeps_aspect_big_folder() {
    check_square_kept(1200, 900, 200, 108, 114);
}

#[test]
fn scale_pixels_keeps_aspect_small_settings() {
    check_square_kept(372, 802, 300, 108, 114);
}

#[test]
fn scale_pixels_same_aspect_is_plain_resize() {
    let src = vec![px(255, 255, 255, 255); 40 * 20];
    let d = scale(&src, 40, 20, 20, 10, 1.0);
    assert!(d.iter().all(|p| p.a == 255 && p.r == 255 && p.g == 255 && p.b == 255), "opaque white must stay opaque white");
}

#[test]
fn scale_pixels_fade_half() {
    let src = vec![px(255, 255, 255, 255); 16 * 16];
    let d = scale(&src, 16, 16, 16, 16, 0.5);
    for p in &d {
        assert!((126..=129).contains(&p.a), "alpha {} not ~128", p.a);
        assert!(p.r <= p.a && p.g <= p.a && p.b <= p.a, "not premultiplied");
    }
}

#[test]
fn scale_pixels_fade_zero_is_transparent() {
    let src = vec![px(200, 100, 50, 255); 30 * 30];
    let d = scale(&src, 30, 30, 12, 17, 0.0);
    assert!(d.iter().all(|p| p.a == 0 && p.r == 0 && p.g == 0 && p.b == 0));
}

#[test]
fn scale_pixels_one_pixel() {
    let d = scale(&[px(255, 255, 255, 255)], 1, 1, 1, 1, 1.0);
    assert_eq!((d[0].r, d[0].g, d[0].b, d[0].a), (255, 255, 255, 255));
}

#[test]
fn scale_pixels_one_pixel_up_to_tile() {
    let d = scale(&[px(255, 255, 255, 255)], 1, 1, 108, 114, 1.0);
    let c = d[57 * 108 + 54];
    assert_eq!(c.a, 255, "center must show the pixel");
}

#[test]
fn scale_pixels_down_to_one_pixel() {
    let src = vec![px(255, 255, 255, 255); 372 * 802];
    let d = scale(&src, 372, 802, 1, 1, 1.0); // no panic, no write past the buffer
    let p = d[0];
    assert!(p.r <= p.a && p.g <= p.a && p.b <= p.a, "1x1 result not premultiplied");
}

#[test]
fn scale_pixels_premultiplied_with_fade() {
    // valid premultiplied source (channel <= alpha), varying alpha
    let (sw, sh) = (1200, 900);
    let src: Vec<Bgra> = (0..sw * sh)
        .map(|i| {
            let a = (i % 256) as u8;
            px(a, a / 2, a / 3, a)
        })
        .collect();
    let d = scale(&src, sw, sh, 108, 114, 0.3);
    for p in &d {
        assert!(p.r <= p.a && p.g <= p.a && p.b <= p.a, "premultiplied broken: {} {} {} {}", p.r, p.g, p.b, p.a);
        assert!(p.a <= 78, "fade 0.3 of 255 must be <= ~77, got {}", p.a);
    }
}

#[test]
fn scale_pixels_rewrites_every_dst_pixel() {
    // stale pixels from an earlier frame must never survive (outside the content = cleared)
    for (sw, sh) in [(372usize, 802usize), (1200, 900)] {
        let src = vec![px(255, 255, 255, 255); sw * sh];
        // marker r=9, a=201 can't come from a white source (white premultiplied: r == a)
        let mut dst = vec![px(9, 9, 9, 201); 108 * 114];
        plat::scale_pixels(&src, sw, sh, &mut dst, 108, 114, 1.0);
        assert!(dst.iter().all(|p| !(p.r == 9 && p.a == 201)), "{sw}x{sh}: stale pixel left in dst");
    }
}

// ---------- A4: snapshot cache during zoom ----------

#[test]
fn zoom_snap_renders_once_when_static() {
    let mut s = ZoomSnap::default();
    assert!(s.frame(), "first zoom frame must render the scene");
    for _ in 0..30 {
        assert!(!s.frame(), "static content: no re-render per frame");
    }
}

#[test]
fn zoom_snap_rerenders_after_dirty() {
    let mut s = ZoomSnap::default();
    assert!(s.frame());
    s.mark_dirty();
    s.mark_dirty(); // twice before a frame = still one render
    assert!(s.frame(), "dirty window must re-render");
    assert!(!s.frame(), "then cached again");
}

#[test]
fn zoom_snap_clear_starts_over() {
    let mut s = ZoomSnap::default();
    assert!(s.frame());
    assert!(!s.frame());
    s.clear(); // zoom view ended; next open / close renders anew
    assert!(s.frame());
    assert!(!s.frame());
}

// ---------- B: external drop ----------

#[test]
fn can_drop_file_into_other_folder() {
    let t = Tmp::new("cd_file");
    let f = t.file("Games/chess.bat", "rem");
    let dst = t.dir("Work/Tools");
    assert!(model::can_drop_into(&f, &dst));
}

#[test]
fn cannot_drop_folder_into_itself() {
    let t = Tmp::new("cd_self");
    let w = t.dir("Work");
    assert!(!model::can_drop_into(&w, &w));
}

#[test]
fn cannot_drop_folder_into_own_subfolder() {
    let t = Tmp::new("cd_sub");
    let w = t.dir("Work");
    assert!(!model::can_drop_into(&w, &t.dir("Work/Tools")));
    assert!(!model::can_drop_into(&w, &t.dir("Work/Tools/Deep/Deeper")));
}

#[test]
fn can_drop_folder_into_sibling_with_same_prefix() {
    // "Work2" is not inside "Work": compare path components, not strings
    let t = Tmp::new("cd_prefix");
    let w = t.dir("Work");
    assert!(model::can_drop_into(&w, &t.dir("Work2")));
    assert!(model::can_drop_into(&w, &t.dir("Workshop/Sub")));
}

#[test]
fn copy_into_keeps_source() {
    let t = Tmp::new("cp_basic");
    let f = t.file("loose.bat", "rem one");
    let dst = t.dir("Games");
    let got = model::copy_into(&f, &dst).unwrap();
    assert_eq!(got, dst.join("loose.bat"));
    assert!(f.exists(), "copy must keep the source");
    assert_eq!(fs::read_to_string(&got).unwrap(), "rem one");
}

#[test]
fn copy_into_numbers_without_overwrite() {
    let t = Tmp::new("cp_num");
    let f = t.file("loose.bat", "new");
    let old = t.file("Games/loose.bat", "old");
    let dst = t.p("Games");
    let a = model::copy_into(&f, &dst).unwrap();
    assert_eq!(a, dst.join("loose (2).bat"));
    let b = model::copy_into(&f, &dst).unwrap();
    assert_eq!(b, dst.join("loose (3).bat"));
    assert_eq!(fs::read_to_string(&old).unwrap(), "old", "existing file overwritten");
    assert_eq!(fs::read_to_string(&a).unwrap(), "new");
}

#[test]
fn copy_into_same_folder_makes_numbered_copy() {
    let t = Tmp::new("cp_same");
    let f = t.file("Games/chess.bat", "rem");
    let got = model::copy_into(&f, &t.p("Games")).unwrap();
    assert_eq!(got, t.p("Games/chess (2).bat"));
    assert!(f.exists());
}

#[test]
fn copy_into_copies_folder_tree() {
    let t = Tmp::new("cp_tree");
    t.file("Pack/a.bat", "a");
    t.file("Pack/Sub/b.bat", "b");
    let dst = t.dir("Work");
    t.dir("Work/Pack"); // name taken -> "Pack (2)"
    let got = model::copy_into(&t.p("Pack"), &dst).unwrap();
    assert_eq!(got, dst.join("Pack (2)"));
    assert_eq!(fs::read_to_string(got.join("a.bat")).unwrap(), "a");
    assert_eq!(fs::read_to_string(got.join("Sub/b.bat")).unwrap(), "b");
    assert!(t.p("Pack/Sub/b.bat").exists(), "copy must keep the source tree");
}

#[test]
fn move_into_numbers_without_overwrite() {
    let t = Tmp::new("mv_num");
    let f = t.file("loose.bat", "new");
    let old = t.file("Games/loose.bat", "old");
    let got = model::move_into(&f, &t.p("Games")).unwrap();
    assert_eq!(got, t.p("Games/loose (2).bat"));
    assert!(!f.exists(), "move must remove the source");
    assert_eq!(fs::read_to_string(&old).unwrap(), "old");
}

#[test]
fn external_drop_moves_and_carries_uses() {
    let t = Tmp::new("ed_uses");
    let desk = t.0.clone();
    let src = t.file("Games/chess.bat", "rem");
    set_uses(&t.p("Games"), "chess.bat", 5);
    let dst = t.dir("Work/Tools");
    let got = model::external_drop(&[src.clone()], &dst, &desk, false);
    assert_eq!(got, vec![dst.join("chess.bat")]);
    assert!(!src.exists());
    assert_eq!(uses(&t.p("Work"), "chess.bat"), Some(5), "launch count must follow into the target folder");
    assert_eq!(uses(&t.p("Games"), "chess.bat"), None, "source folder must forget it");
}

#[test]
fn external_drop_uses_follow_numbered_name() {
    let t = Tmp::new("ed_num");
    let desk = t.0.clone();
    let src = t.file("Games/chess.bat", "new");
    set_uses(&t.p("Games"), "chess.bat", 7);
    t.file("Work/chess.bat", "old");
    set_uses(&t.p("Work"), "chess.bat", 1);
    let got = model::external_drop(&[src], &t.p("Work"), &desk, false);
    assert_eq!(got, vec![t.p("Work/chess (2).bat")]);
    assert_eq!(fs::read_to_string(t.p("Work/chess.bat")).unwrap(), "old", "existing overwritten");
    assert_eq!(uses(&t.p("Work"), "chess (2).bat"), Some(7));
    assert_eq!(uses(&t.p("Work"), "chess.bat"), Some(1), "existing item's count must stay");
}

#[test]
fn external_drop_from_group_subfolder() {
    let t = Tmp::new("ed_group");
    let desk = t.0.clone();
    let src = t.file("Work/Docs/alpha.bat", "rem");
    set_uses(&t.p("Work"), "alpha.bat", 3);
    let dst = t.dir("Games");
    let got = model::external_drop(&[src], &dst, &desk, false);
    assert_eq!(got, vec![dst.join("alpha.bat")]);
    assert_eq!(uses(&t.p("Games"), "alpha.bat"), Some(3));
    assert_eq!(uses(&t.p("Work"), "alpha.bat"), None);
}

#[test]
fn external_drop_inside_same_folder_keeps_uses() {
    let t = Tmp::new("ed_same");
    let desk = t.0.clone();
    let src = t.file("Work/Docs/alpha.bat", "rem");
    set_uses(&t.p("Work"), "alpha.bat", 4);
    let dst = t.dir("Work/Tools");
    let got = model::external_drop(&[src], &dst, &desk, false);
    assert_eq!(got, vec![dst.join("alpha.bat")]);
    assert_eq!(uses(&t.p("Work"), "alpha.bat"), Some(4), "same folder: count must not be lost");
}

#[test]
fn external_drop_copy_keeps_source() {
    let t = Tmp::new("ed_copy");
    let desk = t.0.clone();
    let src = t.file("copyme.bat", "rem");
    let dst = t.dir("Games");
    let got = model::external_drop(&[src.clone()], &dst, &desk, true);
    assert_eq!(got, vec![dst.join("copyme.bat")]);
    assert!(src.exists(), "Ctrl drop = copy");
    let again = model::external_drop(&[src.clone()], &dst, &desk, true);
    assert_eq!(again, vec![dst.join("copyme (2).bat")]);
}

#[test]
fn external_drop_loose_desktop_file() {
    let t = Tmp::new("ed_loose");
    let desk = t.0.clone();
    let src = t.file("loose.bat", "rem");
    let dst = t.dir("Games");
    let got = model::external_drop(&[src.clone()], &dst, &desk, false);
    assert_eq!(got, vec![dst.join("loose.bat")]);
    assert!(!src.exists());
}

#[test]
fn external_drop_skips_self_and_ancestor() {
    let t = Tmp::new("ed_refuse");
    let desk = t.0.clone();
    let work = t.dir("Work");
    let chess = t.file("Games/chess.bat", "rem");
    let dst = t.dir("Work/Tools");
    let got = model::external_drop(&[work.clone(), chess], &dst, &desk, false);
    assert_eq!(got, vec![dst.join("chess.bat")], "refused item skipped, valid one still dropped");
    assert!(work.is_dir() && dst.is_dir(), "folder must not move into itself");
    let none = model::external_drop(&[work.clone()], &work, &desk, true);
    assert!(none.is_empty(), "copy into itself refused too");
}

#[test]
fn external_drop_skips_missing_source() {
    let t = Tmp::new("ed_missing");
    let desk = t.0.clone();
    let dst = t.dir("Games");
    let got = model::external_drop(&[t.p("ghost.bat")], &dst, &desk, false);
    assert!(got.is_empty());
}

#[test]
fn external_drop_same_folder_move_is_noop() {
    let t = Tmp::new("ed_same_dir");
    let desk = t.0.clone();
    let src = t.file("Games/chess.bat", "orig");
    set_uses(&t.p("Games"), "chess.bat", 6);
    let _ = model::external_drop(&[src.clone()], &t.p("Games"), &desk, false);
    assert!(src.exists(), "original file must remain in place");
    assert_eq!(fs::read_to_string(&src).unwrap(), "orig");
    assert!(!t.p("Games/chess (2).bat").exists(), "same-folder move must not duplicate as (2)");
    assert_eq!(uses(&t.p("Games"), "chess.bat"), Some(6), "launch count must stay intact");
}

#[test]
fn drop_effect_decision_table() {
    use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE};
    assert_eq!(plat::drop_effect(true, false, true), DROPEFFECT_MOVE.0, "HDROP + allowed -> MOVE");
    assert_eq!(plat::drop_effect(true, true, true), DROPEFFECT_COPY.0, "HDROP + Ctrl + allowed -> COPY");
    assert_eq!(plat::drop_effect(true, false, false), DROPEFFECT_NONE.0, "HDROP + disallowed -> NONE");
    assert_eq!(plat::drop_effect(true, true, false), DROPEFFECT_NONE.0, "HDROP + Ctrl + disallowed -> NONE");
    assert_eq!(plat::drop_effect(false, false, true), DROPEFFECT_NONE.0, "non-HDROP -> NONE");
    assert_eq!(plat::drop_effect(false, true, true), DROPEFFECT_NONE.0, "non-HDROP + Ctrl -> NONE");
}

// ---------- C: desktop folder sync ----------

fn hide(p: &Path) {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN};
    unsafe { SetFileAttributesW(&HSTRING::from(p.as_os_str()), FILE_ATTRIBUTE_HIDDEN) }.unwrap();
}

#[test]
fn sync_plan_added_removed_kept() {
    let t = Tmp::new("sync_basic");
    let (a, b) = (t.dir("A"), t.dir("B"));
    let gone = t.p("C"); // tile exists, folder deleted
    let d = model::sync_plan(&t.0, &[a.clone(), gone.clone()]);
    assert_eq!(sorted(d.added), vec![b]);
    assert_eq!(sorted(d.removed), vec![gone]);
    assert_eq!(sorted(d.kept), vec![a]);
}

#[test]
fn sync_plan_rename_is_remove_plus_add() {
    let t = Tmp::new("sync_ren");
    let new = t.dir("New");
    let old = t.p("Old");
    let d = model::sync_plan(&t.0, &[old.clone()]);
    assert_eq!(d.added, vec![new]);
    assert_eq!(d.removed, vec![old]);
    assert!(d.kept.is_empty());
}

#[test]
fn sync_plan_no_change() {
    let t = Tmp::new("sync_same");
    let (a, b) = (t.dir("A"), t.dir("B"));
    t.file("loose.bat", "rem"); // files are not tiles
    let d = model::sync_plan(&t.0, &[a.clone(), b.clone()]);
    assert!(d.added.is_empty(), "{:?}", d.added);
    assert!(d.removed.is_empty(), "{:?}", d.removed);
    assert_eq!(sorted(d.kept), vec![a, b]);
}

#[test]
fn sync_plan_keeps_our_hidden_folders() {
    // tiled folders get FILE_ATTRIBUTE_HIDDEN from the app: still ours, not "removed"
    let t = Tmp::new("sync_hidden");
    let ours = t.dir("Ours");
    hide(&ours);
    let other = t.dir("UserHidden");
    hide(&other); // hidden by the user, never had a tile: not a new tile
    let d = model::sync_plan(&t.0, &[ours.clone()]);
    assert!(d.added.is_empty(), "{:?}", d.added);
    assert!(d.removed.is_empty(), "{:?}", d.removed);
    assert_eq!(d.kept, vec![ours]);
}

#[test]
fn ignored_change_only_cfg_file() {
    assert!(model::ignored_change(Path::new(".glassfolder.json")));
    assert!(model::ignored_change(Path::new("Work\\.glassfolder.json")));
    assert!(model::ignored_change(Path::new("Work/Docs/.glassfolder.json")));
    assert!(!model::ignored_change(Path::new("Work")));
    assert!(!model::ignored_change(Path::new("Work\\alpha.lnk")));
    assert!(!model::ignored_change(Path::new("New folder")));
    assert!(!model::ignored_change(Path::new("Work\\glassfolder.json")));
}

// ---------- D: autostart in the registry ----------

mod reg {
    use windows::core::{w, HSTRING};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteTreeW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ,
    };

    const REAL_RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

    fn read_val(subkey: &str) -> Option<String> {
        let mut buf = vec![0u16; 2048];
        let mut len = (buf.len() * 2) as u32;
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(subkey),
                w!("GlassFolders"),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as _),
                Some(&mut len),
            )
        };
        if r != ERROR_SUCCESS {
            return None;
        }
        let n = (len as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..n]))
    }

    fn restore_real_run(want: &Option<String>) {
        let mut hkey = HKEY::default();
        let opened = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(REAL_RUN_KEY), Some(0), KEY_SET_VALUE, &mut hkey)
        };
        if opened != ERROR_SUCCESS {
            return;
        }
        unsafe {
            match want {
                Some(v) => {
                    let wide: Vec<u16> = v.encode_utf16().chain(std::iter::once(0)).collect();
                    let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
                    let _ = RegSetValueExW(hkey, w!("GlassFolders"), Some(0), REG_SZ, Some(bytes));
                }
                None => {
                    let _ = RegDeleteValueW(hkey, w!("GlassFolders"));
                }
            }
            let _ = RegCloseKey(hkey);
        }
    }

    /// Snapshots the user's REAL HKCU Run\GlassFolders value before a test; if mutated, restores it and fails.
    pub struct RealRunGuard {
        before: Option<String>,
        checked: bool,
    }

    impl RealRunGuard {
        pub fn new() -> Self {
            Self { before: read_val(REAL_RUN_KEY), checked: false }
        }
        pub fn assert_unchanged(mut self) {
            self.checked = true;
            let after = read_val(REAL_RUN_KEY);
            if after != self.before {
                restore_real_run(&self.before);
                panic!("real HKCU\\{REAL_RUN_KEY}\\GlassFolders was mutated: {:?} -> {:?} (restored)", self.before, after);
            }
        }
    }

    impl Drop for RealRunGuard {
        fn drop(&mut self) {
            if !self.checked {
                let after = read_val(REAL_RUN_KEY);
                if after != self.before {
                    restore_real_run(&self.before);
                }
            }
        }
    }

    /// Isolated HKCU test key, deleted on drop.
    pub struct Key(pub String);

    impl Key {
        pub fn new(name: &str) -> Key {
            let k = Key(format!("Software\\GlassFoldersTest_{}_{}", std::process::id(), name));
            k.wipe();
            k
        }
        fn wipe(&self) {
            unsafe {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(self.0.as_str()));
            }
        }
        /// "GlassFolders" value, read straight from the registry.
        pub fn value(&self) -> Option<String> {
            read_val(&self.0)
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            self.wipe();
        }
    }
}

fn exe_value() -> String {
    format!("\"{}\"", std::env::current_exe().unwrap().display())
}

#[test]
fn autostart_on_check_off() {
    let real = reg::RealRunGuard::new();
    let k = reg::Key::new("cycle");
    assert!(!crate::shell::autostart_enabled_at(&k.0), "fresh key: off");
    assert!(crate::shell::set_autostart_at(&k.0, true));
    assert!(crate::shell::autostart_enabled_at(&k.0));
    assert_eq!(k.value(), Some(exe_value()), "value = quoted exe path");
    assert!(crate::shell::set_autostart_at(&k.0, false));
    assert!(!crate::shell::autostart_enabled_at(&k.0));
    assert_eq!(k.value(), None, "value removed");
    real.assert_unchanged();
}

#[test]
fn autostart_off_twice_is_fine() {
    let real = reg::RealRunGuard::new();
    let k = reg::Key::new("off2");
    assert!(crate::shell::set_autostart_at(&k.0, false));
    assert!(crate::shell::set_autostart_at(&k.0, false));
    assert!(!crate::shell::autostart_enabled_at(&k.0));
    real.assert_unchanged();
}

#[test]
fn autostart_on_twice_keeps_one_value() {
    let real = reg::RealRunGuard::new();
    let k = reg::Key::new("on2");
    assert!(crate::shell::set_autostart_at(&k.0, true));
    assert!(crate::shell::set_autostart_at(&k.0, true));
    assert_eq!(k.value(), Some(exe_value()));
    real.assert_unchanged();
}

#[test]
fn autostart_key_from_env() {
    let real = reg::RealRunGuard::new();
    // only this test touches GF_AUTOSTART_KEY
    std::env::remove_var("GF_AUTOSTART_KEY");
    assert_eq!(crate::shell::autostart_key(), "Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let k = reg::Key::new("env");
    std::env::set_var("GF_AUTOSTART_KEY", &k.0);
    assert_eq!(crate::shell::autostart_key(), k.0);
    let on = crate::shell::set_autostart(true);
    let seen = crate::shell::autostart_enabled();
    let val = k.value();
    let off = crate::shell::set_autostart(false);
    let after = crate::shell::autostart_enabled();
    std::env::remove_var("GF_AUTOSTART_KEY");
    assert!(on && seen, "env key: enable must work");
    assert_eq!(val, Some(exe_value()), "written into the env key");
    assert!(off && !after, "env key: disable must work");
    real.assert_unchanged();
}

// ---------- D2: tray icon RGBA & tray menu labels ----------

#[test]
fn tray_icon_rgba_is_valid_32x32() {
    let (w, h, px) = crate::win::tray_icon_rgba();
    assert_eq!((w, h), (32, 32), "tray icon must be 32x32");
    assert_eq!(px.len(), 32 * 32 * 4, "RGBA byte length");
    assert!(px.chunks_exact(4).any(|c| c[3] > 0), "icon must have visible (non-zero alpha) pixels");
    assert!(px.chunks_exact(4).any(|c| c[0] > 0 || c[1] > 0 || c[2] > 0), "icon must have non-black color");
}

#[test]
fn tray_menu_items_localized_and_checked() {
    let ru_off = crate::win::tray_menu_items(false, false);
    let ru_on = crate::win::tray_menu_items(false, true);
    let en_off = crate::win::tray_menu_items(true, false);
    let en_on = crate::win::tray_menu_items(true, true);

    for (label, list) in [("ru_off", &ru_off), ("ru_on", &ru_on), ("en_off", &en_off), ("en_on", &en_on)] {
        let actions: Vec<_> = list.iter().filter(|(id, _, _, _)| *id != 0).collect();
        let seps = list.iter().filter(|(id, _, _, _)| *id == 0).count();
        assert!(actions.len() >= 3, "{label}: need at least autostart, refresh, quit");
        assert!(seps >= 1, "{label}: need separator before quit");
        assert!(actions.iter().all(|(_, text, enabled, _)| !text.is_empty() && *enabled), "{label}: items enabled & labeled");
        let mut ids: Vec<u32> = actions.iter().map(|(id, _, _, _)| *id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), actions.len(), "{label}: action ids must be unique");
    }

    assert_eq!(ru_off.iter().filter(|(_, _, _, c)| *c).count(), 0, "autostart=false -> no checked item (RU)");
    assert_eq!(ru_on.iter().filter(|(_, _, _, c)| *c).count(), 1, "autostart=true -> autostart item checked (RU)");
    assert_eq!(en_off.iter().filter(|(_, _, _, c)| *c).count(), 0, "autostart=false -> no checked item (EN)");
    assert_eq!(en_on.iter().filter(|(_, _, _, c)| *c).count(), 1, "autostart=true -> autostart item checked (EN)");

    let ru_texts: Vec<&str> = ru_off.iter().filter(|(id, _, _, _)| *id != 0).map(|(_, s, _, _)| s.as_str()).collect();
    let en_texts: Vec<&str> = en_off.iter().filter(|(id, _, _, _)| *id != 0).map(|(_, s, _, _)| s.as_str()).collect();
    assert_eq!(ru_texts.len(), en_texts.len(), "RU and EN menus must have same item count");
    for (r, e) in ru_texts.iter().zip(en_texts.iter()) {
        assert_ne!(r, e, "RU and EN labels must differ: {r:?} vs {e:?}");
    }
}

// ---------- E: Win+D ----------

#[test]
fn desktop_classes() {
    assert!(crate::desk::is_desktop_class("Progman"));
    assert!(crate::desk::is_desktop_class("WorkerW"));
    assert!(!crate::desk::is_desktop_class("Shell_TrayWnd"));
    assert!(!crate::desk::is_desktop_class("Notepad"));
    assert!(!crate::desk::is_desktop_class(""));
}

#[test]
fn desktop_host_holds_icon_view() {
    use windows::core::{w, BOOL};
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, FindWindowExW, GetClassNameW};
    // Independent probe (no desk.rs code): every top-level Progman/WorkerW with a direct SHELLDLL_DefView child.
    unsafe extern "system" fn probe(h: HWND, lp: LPARAM) -> BOOL {
        unsafe {
            let mut buf = [0u16; 64];
            let n = GetClassNameW(h, &mut buf) as usize;
            let class = String::from_utf16_lossy(&buf[..n]);
            if class == "Progman" || class == "WorkerW" {
                if FindWindowExW(Some(h), None, w!("SHELLDLL_DefView"), None).is_ok_and(|v| !v.is_invalid()) {
                    (*(lp.0 as *mut Vec<HWND>)).push(h);
                }
            }
        }
        true.into()
    }
    let mut hosts: Vec<HWND> = Vec::new();
    let _ = unsafe { EnumWindows(Some(probe), LPARAM(&mut hosts as *mut _ as isize)) };
    if hosts.is_empty() {
        // No Explorer desktop on this Win32 desktop (isolated CI/sandbox desktop): host must be None, not a bogus HWND.
        assert_eq!(crate::desk::desktop_host(), None, "no Explorer desktop exists, desktop_host must be None");
        eprintln!("desktop_host_holds_icon_view: no Explorer desktop on this Win32 desktop, checked None only");
        return;
    }
    let h = crate::desk::desktop_host().expect("Explorer desktop window not found");
    assert!(hosts.contains(&h), "desktop_host {h:?} is not one of the real DefView hosts {hosts:?}");
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
    let class = String::from_utf16_lossy(&buf[..n]);
    assert!(class == "Progman" || class == "WorkerW", "host class {class}");
    let view = unsafe { FindWindowExW(Some(h), None, w!("SHELLDLL_DefView"), None) };
    assert!(view.is_ok_and(|v| !v.is_invalid()), "host must contain SHELLDLL_DefView");
}

#[test]
fn z_place_normal_mode_bottom() {
    assert_eq!(plat::z_place(Kind::Tile, false, false), Some(ZPlace::Bottom));
    assert_eq!(plat::z_place(Kind::Panel, true, false), Some(ZPlace::Bottom));
}

#[test]
fn z_place_show_desktop_above_host() {
    assert_eq!(plat::z_place(Kind::Tile, false, true), Some(ZPlace::AboveHost));
    assert_eq!(plat::z_place(Kind::Panel, true, true), Some(ZPlace::AboveHost));
}

#[test]
fn z_place_leaves_other_windows_alone() {
    for sd in [false, true] {
        assert_eq!(plat::z_place(Kind::Panel, false, sd), None, "open (not pinned) panel is not managed");
        assert_eq!(plat::z_place(Kind::Hint, false, sd), None, "hint is not managed");
    }
}

#[test]
fn z_layer_pinned_above_tile() {
    let tile = plat::z_layer(Kind::Tile, false).expect("tile has a layer");
    let pinned = plat::z_layer(Kind::Panel, true).expect("pinned panel has a layer");
    assert!(pinned > tile, "pinned panel must stay above tiles");
    assert_eq!(plat::z_layer(Kind::Panel, false), None);
    assert_eq!(plat::z_layer(Kind::Hint, false), None);
}

// Regression (QA, Block E): leaving Show Desktop must restore managed windows to HWND_BOTTOM.
#[test]
fn fg_change_entering_show_desktop_raises() {
    assert_eq!(crate::plat::on_foreground_change(false, true), Some(crate::plat::LayerAction::RaiseAboveHost));
}

#[test]
fn fg_change_desktop_stays_foreground_raises() {
    assert_eq!(crate::plat::on_foreground_change(true, true), Some(crate::plat::LayerAction::RaiseAboveHost));
}

#[test]
fn fg_change_exiting_show_desktop_restores_bottom() {
    assert_eq!(
        crate::plat::on_foreground_change(true, false),
        Some(crate::plat::LayerAction::RestoreBottom),
        "leaving Show Desktop must put tiles / pinned panels back to HWND_BOTTOM"
    );
}

#[test]
fn fg_change_app_to_app_is_noop() {
    assert_eq!(crate::plat::on_foreground_change(false, false), None);
}
