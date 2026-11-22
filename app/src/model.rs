// Кратко: чтение папки со стола (группы = подпапки), настройки в .glassfolder.json, раскладка групп по сетке.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Fluent-ish palette, 10 colors (same as mockup).
pub const PAL: [&str; 10] = [
    "#0078d4", "#00b7c3", "#10893e", "#8cbd18", "#ffb900", "#f7630c", "#e81123", "#e3008c",
    "#8764b8", "#7a7574",
];

pub const CELL: f32 = 76.0; // square slot
pub const GAP: f32 = 8.0;
pub const PITCH: f32 = CELL + GAP; // one grid step, same inside and between groups
// Panel grid: columns = PITCH, rows = half PITCH. Group of r slot rows = 2r+1 grid rows
// (extra half row = header 30 + bottom pad 12). So groups align on a free canvas.
pub const ROW: f32 = PITCH / 2.0;
pub const HEAD: f32 = 30.0; // group header (app.slint uses the same)

pub const LOOSE: &str = "Разное";
const CFG: &str = ".glassfolder.json";
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Config {
    pub color: Option<String>,
    pub cols: Option<u32>,
    pub order: Vec<String>,
    pub groups: BTreeMap<String, GroupCfg>,
    /// Tile on desktop: [x, y, w, h] physical px. None = auto place, default size.
    pub tile: Option<[i32; 4]>,
    /// launch count by item file name (tile shows most used first)
    pub uses: BTreeMap<String, u32>,
    /// Pinned open on the desktop: panel top-left [x, y] physical px (tile hidden). None = not pinned.
    pub pin: Option<[i32; 2]>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct GroupCfg {
    /// size in slots (cols x rows). None = auto from item count.
    pub w: Option<u32>,
    pub h: Option<u32>,
    pub color: Option<String>,
    /// place on panel grid: column, half-row. None = auto (first free spot).
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// item order by file name; unlisted items follow alphabetically.
    pub items: Vec<String>,
}

pub struct Group {
    pub name: String,
    pub dir: PathBuf,
    pub items: Vec<PathBuf>,
}

pub struct Folder {
    pub name: String,
    pub dir: PathBuf,
    pub cfg: Config,
    pub groups: Vec<Group>,
}

impl Folder {
    /// None = colorless (default). Color only when user picks one.
    pub fn tint(&self) -> Option<String> {
        self.cfg.color.clone()
    }
    pub fn group_tint(&self, i: usize) -> Option<String> {
        let g = &self.groups[i];
        self.cfg.groups.get(&g.name).and_then(|c| c.color.clone())
    }
    /// All items, most launched first; ties keep panel order (stable sort).
    pub fn top_items(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.groups.iter().flat_map(|g| g.items.iter().cloned()).collect();
        let n = |p: &PathBuf| file_name(p).and_then(|k| self.cfg.uses.get(&k).copied()).unwrap_or(0);
        v.sort_by_key(|p| std::cmp::Reverse(n(p)));
        v
    }
}

/// +1 launch of `item` (inside folder `root`). Fresh cfg from disk, so nothing else is lost.
pub fn bump_use(root: &Path, item: &Path) {
    let Some(k) = file_name(item) else { return };
    let mut cfg = load_cfg(root);
    *cfg.uses.entry(k).or_insert(0) += 1;
    let _ = save_cfg(root, &cfg);
}

fn is_hidden(p: &Path) -> bool {
    fs::metadata(p)
        .map(|m| m.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0)
        .unwrap_or(true)
}

fn is_item(p: &Path) -> bool {
    if p.is_dir() {
        return true;
    }
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "lnk" | "url" | "exe" | "appref-ms" | "bat" | "cmd")
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| !is_hidden(p)).collect())
        .unwrap_or_default();
    v.sort_by_key(|p| name_of(p).to_lowercase());
    v
}

pub fn name_of(p: &Path) -> String {
    if p.is_dir() {
        p.file_name()
    } else {
        p.file_stem()
    }
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or_default()
}

/// Folders on Desktop = widgets.
pub fn list_folders(desktop: &Path) -> Vec<PathBuf> {
    sorted_entries(desktop).into_iter().filter(|p| p.is_dir()).collect()
}

pub fn scan(dir: &Path) -> Folder {
    let cfg = load_cfg(dir);
    let mut groups = Vec::new();
    let mut loose = Vec::new();
    for p in sorted_entries(dir) {
        if p.is_dir() {
            let items: Vec<PathBuf> = sorted_entries(&p).into_iter().filter(|q| is_item(q)).collect();
            groups.push(Group { name: name_of(&p), dir: p, items });
        } else if is_item(&p) {
            loose.push(p);
        }
    }
    if !loose.is_empty() {
        groups.push(Group { name: LOOSE.into(), dir: dir.to_path_buf(), items: loose });
    }
    // Saved order first, then the rest as scanned (Разное already last).
    let rank = |g: &Group| cfg.order.iter().position(|n| *n == g.name).unwrap_or(usize::MAX);
    groups.sort_by_key(|g| rank(g));
    // user item order inside each group (stable sort keeps alphabetical for the rest)
    for g in &mut groups {
        if let Some(order) = cfg.groups.get(&g.name).map(|c| &c.items).filter(|o| !o.is_empty()) {
            g.items.sort_by_key(|p| order.iter().position(|n| Some(n.as_str()) == file_name(p).as_deref()).unwrap_or(usize::MAX));
        }
    }
    Folder { name: name_of(dir), dir: dir.to_path_buf(), cfg, groups }
}

pub fn load_cfg(dir: &Path) -> Config {
    fs::read_to_string(dir.join(CFG))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write config as hidden file. Existing hidden file: truncate in place
/// (CREATE_ALWAYS fails on hidden files on Windows).
pub fn save_cfg(dir: &Path, cfg: &Config) -> std::io::Result<()> {
    let path = dir.join(CFG);
    let json = serde_json::to_string_pretty(cfg)?;
    let mut f = if path.exists() {
        fs::OpenOptions::new().write(true).truncate(true).open(&path)?
    } else {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .attributes(FILE_ATTRIBUTE_HIDDEN)
            .open(&path)?
    };
    f.write_all(json.as_bytes())
}

impl Config {
    /// Group folder renamed on disk: keep its color/size and place in order.
    pub fn rename_group(&mut self, old: &str, new: &str) {
        if let Some(c) = self.groups.remove(old) {
            self.groups.insert(new.to_string(), c);
        }
        for n in self.order.iter_mut().filter(|n| *n == old) {
            *n = new.to_string();
        }
    }
    /// Group folder gone: drop its settings (a later folder with same name starts clean).
    pub fn forget_group(&mut self, name: &str) {
        self.groups.remove(name);
        self.order.retain(|n| n != name);
    }
}

// ---------- file ops (drag in panel) ----------

pub const NEW_GROUP: &str = "Новая группа";

/// "name" for n = 1, else "name (n)" + optional extension.
fn numbered(stem: &str, ext: Option<&str>, n: u32) -> String {
    let base = if n == 1 { stem.to_string() } else { format!("{stem} ({n})") };
    match ext {
        Some(e) => format!("{base}.{e}"),
        None => base,
    }
}

/// Rename/move without ever replacing an existing target (std::fs::rename replaces files on Windows).
fn move_no_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};
    unsafe { MoveFileExW(&HSTRING::from(src.as_os_str()), &HSTRING::from(dst.as_os_str()), MOVE_FILE_FLAGS(0)) }
        .map_err(|e| std::io::Error::from_raw_os_error((e.code().0 & 0xffff) as i32))
}

/// Move `src` (file or dir) into `dst_dir`. Name taken -> " (2)", " (3)"... Returns new path.
pub fn move_into(src: &Path, dst_dir: &Path) -> std::io::Result<PathBuf> {
    if src.parent() == Some(dst_dir) {
        return Ok(src.to_path_buf());
    }
    let (stem, ext) = if src.is_dir() {
        (src.file_name(), None)
    } else {
        (src.file_stem(), src.extension().map(|e| e.to_string_lossy().into_owned()))
    };
    let stem = stem.map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    move_unique(src, dst_dir, &stem, ext.as_deref(), |_| true)
}

/// Try target names stem, "stem (2)"... in dir until one is free. `ok` filters names we must not use.
fn move_unique(src: &Path, dir: &Path, stem: &str, ext: Option<&str>, ok: impl Fn(&str) -> bool) -> std::io::Result<PathBuf> {
    for n in 1..1000 {
        let name = numbered(stem, ext, n);
        if !ok(&name) {
            continue;
        }
        let dst = dir.join(&name);
        match move_no_replace(src, &dst) {
            Ok(()) => return Ok(dst),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name"))
}

/// Create empty subfolder "Новая группа" (or " (2)"...) in `root`. Returns its name.
pub fn create_group(root: &Path) -> std::io::Result<String> {
    for n in 1..1000 {
        let name = numbered(NEW_GROUP, None, n);
        if name == LOOSE {
            continue;
        }
        match fs::create_dir(root.join(&name)) {
            Ok(()) => return Ok(name),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name"))
}

/// Rename group folder `old` in `root`. Name taken -> " (2)"... Returns final name.
/// "Разное" is reserved (= root files), never used as folder name.
pub fn rename_group(root: &Path, old: &str, new: &str) -> std::io::Result<String> {
    let dst = move_unique(&root.join(old), root, new, None, |n| n != LOOSE)?;
    Ok(name_of(&dst))
}

/// Remove `dir` only if it holds nothing at all (hidden files included). Returns true if removed.
pub fn remove_if_empty(dir: &Path) -> bool {
    let empty = fs::read_dir(dir).map(|mut rd| rd.next().is_none()).unwrap_or(false);
    // remove_dir (RemoveDirectoryW) also refuses non-empty dirs: second safety net
    empty && fs::remove_dir(dir).is_ok()
}

/// User-typed folder name -> valid Windows name. Empty = invalid.
pub fn clean_name(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control() && !"<>:\"/\\|?*".contains(*c)).collect();
    s.trim().trim_end_matches(['.', ' ']).to_string()
}

/// App-wide settings (not per folder): %APPDATA%/glass-folders/settings.json
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    /// shortcut names under icons (panel)
    pub labels: bool,
    /// group titles (panel). Off: only the color dot stays.
    pub names: bool,
    /// group color dots (panel). Off with names off: plain plates.
    pub dots: bool,
    /// 0 dense .. 2 clear
    pub glass: i32,
    /// groups pulled to: 0 nowhere (free), 1 top-left, 2 top-right
    pub gravity: i32,
    /// desktop tiles land on the icon grid (else free place)
    pub snap: bool,
    /// folder name + color dot in the panel header
    pub ptitle: bool,
    /// folder name under the desktop tile. Off: tile shrinks to square cells.
    pub tnames: bool,
    /// folder icons parked away by a running app: path -> home spot [x, y] (screen px).
    /// Non-empty at start = last run was killed -> put them back.
    pub homes: BTreeMap<String, [i32; 2]>,
    /// icon size: 0 small, 1 medium, 2 large = zoom of panels and tiles (ZOOM)
    pub isize: i32,
}

/// Window zoom per icon size setting.
pub const ZOOM: [f32; 3] = [0.85, 1.0, 1.2];

impl Settings {
    pub fn zoom(&self) -> f32 {
        ZOOM[self.isize.clamp(0, 2) as usize]
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings { labels: true, names: true, dots: true, glass: 1, gravity: 1, snap: true, ptitle: true, tnames: true, homes: BTreeMap::new(), isize: 1 }
    }
}

fn settings_path() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("glass-folders").join("settings.json")
}

pub fn load_settings() -> Settings {
    fs::read_to_string(settings_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_settings(s: &Settings) {
    let p = settings_path();
    if let Some(d) = p.parent() {
        let _ = fs::create_dir_all(d);
    }
    if let Ok(j) = serde_json::to_string_pretty(s) {
        let _ = fs::write(p, j);
    }
}

/// Optional hex -> (color, has_color). No color = transparent.
pub fn tint(hex: &Option<String>) -> (slint::Color, bool) {
    match hex {
        Some(h) => (color(h), true),
        None => (slint::Color::from_argb_u8(0, 0, 0, 0), false),
    }
}

pub fn color(hex: &str) -> slint::Color {
    let h = hex.trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).unwrap_or(0x0078d4);
    slint::Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub fn hex(c: slint::Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue())
}


/// File name with extension (key in GroupCfg.items).
pub fn file_name(p: &Path) -> Option<String> {
    p.file_name().map(|s| s.to_string_lossy().into_owned())
}

// ---------- layout: free grid of groups ----------

thread_local! {
    /// Half-rows of group header: 1 (title / dot shown) or 0 (plain plate, hugs its slots).
    static HDR: std::cell::Cell<i32> = const { std::cell::Cell::new(1) };
}

/// Groups have a header row (title or dot) or not. Changes GRect height for a slot count.
pub fn set_header(on: bool) {
    HDR.with(|h| h.set(i32::from(on)));
}

/// Header height of a group in px (with its bottom padding): ROW or 0.
pub fn header_px() -> f32 {
    HDR.with(|h| h.get()) as f32 * ROW
}

/// Group rect on the panel grid. x, w in columns; y, h in half-rows (h = 2 * slot rows + header).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl GRect {
    pub fn slots(w: i32, rows: i32, x: i32, y: i32) -> GRect {
        GRect { x, y, w, h: 2 * rows + HDR.with(|h| h.get()) }
    }
    pub fn rows(&self) -> i32 {
        (self.h - HDR.with(|h| h.get())) / 2
    }
    pub fn hits(&self, o: &GRect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
    /// pixel rect (x, y, w, h) inside the panel grid area
    pub fn px(&self) -> [f32; 4] {
        [self.x as f32 * PITCH, self.y as f32 * ROW, self.w as f32 * PITCH - GAP, self.h as f32 * ROW - GAP]
    }
}

/// Overlaps -> push groups down until none overlap. `first` keeps its place (the one being dragged);
/// the rest go top-to-bottom, each moved just below whatever it hits. Never moves anything up.
pub fn resolve(r: &mut [GRect], first: Option<usize>) {
    let mut order: Vec<usize> = (0..r.len()).filter(|&i| Some(i) != first).collect();
    order.sort_by_key(|&i| (r[i].y, r[i].x));
    let mut placed: Vec<usize> = first.into_iter().collect();
    for i in order {
        while let Some(b) = placed.iter().filter(|&&p| r[p].hits(&r[i])).map(|&p| r[p].y + r[p].h).max() {
            r[i].y = b;
        }
        placed.push(i);
    }
}

/// Grid extent (cols, half-rows) of all rects.
pub fn extent(r: &[GRect]) -> (i32, i32) {
    r.iter().fold((0, 0), |(c, h), g| (c.max(g.x + g.w), h.max(g.y + g.h)))
}

/// Near-square size that fits n items, width capped.
pub fn auto_size(n: usize, maxw: u32) -> (u32, u32) {
    let n = n.max(1) as f64;
    let w = (n.sqrt().ceil() as u32).clamp(1, maxw.max(1));
    let h = ((n / w as f64).ceil() as u32).max(1);
    (w, h)
}

/// Rects of all groups: saved place/size, else auto size + first free spot.
pub fn layout(f: &Folder, gravity: i32) -> Vec<GRect> {
    let total: usize = f.groups.iter().map(|g| g.items.len().max(1)).sum();
    let cols = f.cfg.cols.unwrap_or_else(|| ((total as f64 * 1.6).sqrt().round() as u32).clamp(4, 8)) as i32;
    let mut rects = Vec::new();
    let mut placed = Vec::new(); // groups with saved place
    for (i, g) in f.groups.iter().enumerate() {
        let c = f.cfg.groups.get(&g.name);
        let (mut aw, mut ah) = auto_size(g.items.len(), cols as u32);
        if f.groups.len() == 1 && aw < 3 {
            // single group fills the min 3-cell panel width -> even edges
            aw = 3;
            ah = g.items.len().max(1).div_ceil(3) as u32;
        }
        let w = c.and_then(|c| c.w).unwrap_or(aw).max(1) as i32;
        let h = c.and_then(|c| c.h).unwrap_or(ah).max(1) as i32;
        let pos = c.and_then(|c| c.x.zip(c.y));
        let (x, y) = pos.unwrap_or((-1, -1));
        if pos.is_some() {
            placed.push(i);
        }
        rects.push(GRect::slots(w, h, x.max(0), y.max(0)));
    }
    // saved ones: fix overlaps (a group may have grown since)
    let mut fixed: Vec<GRect> = placed.iter().map(|&i| rects[i]).collect();
    resolve(&mut fixed, None);
    for (k, &i) in placed.iter().enumerate() {
        rects[i] = fixed[k];
    }
    // others: first free spot, row by row
    for i in 0..rects.len() {
        if placed.contains(&i) {
            continue;
        }
        let mut r = rects[i];
        let span = cols.max(r.w);
        'find: for y in 0.. {
            for x in 0..=span - r.w {
                r.x = x;
                r.y = y;
                if !fixed.iter().any(|o| o.hits(&r)) {
                    break 'find;
                }
            }
        }
        rects[i] = r;
        fixed.push(r);
        placed.push(i);
    }
    settle(&mut rects, gravity);
    rects
}

/// Gravity: 0 none, 1 top-left, 2 top-right. Each group slides up as far as it can, then sideways;
/// repeat until nothing moves. Right edge for top-right = current extent. Never creates overlaps.
pub fn settle(r: &mut [GRect], gravity: i32) {
    if gravity == 0 {
        return;
    }
    let right = extent(r).0;
    let dx = if gravity == 2 { 1 } else { -1 };
    let free = |r: &[GRect], i: usize, g: GRect| {
        g.x >= 0 && g.y >= 0 && g.x + g.w <= right && !r.iter().enumerate().any(|(j, o)| j != i && o.hits(&g))
    };
    for _ in 0..64 {
        let mut moved = false;
        let mut order: Vec<usize> = (0..r.len()).collect();
        order.sort_by_key(|&i| (r[i].y, r[i].x * -dx));
        for i in order {
            for step in [(0, -1), (dx, 0)] {
                loop {
                    let g = GRect { x: r[i].x + step.0, y: r[i].y + step.1, ..r[i] };
                    if !free(r, i, g) {
                        break;
                    }
                    r[i] = g;
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }
}

/// Panel grid size in px for rects (+ extra free cols / half-rows), min 3 columns wide.
pub fn grid_px(r: &[GRect], extra_c: i32, extra_r: i32) -> (f32, f32) {
    let (c, h) = extent(r);
    let c = (c + extra_c).max(3);
    let h = (h + extra_r).max(3);
    (c as f32 * PITCH - GAP, h as f32 * ROW - GAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> GRect {
        GRect { x, y, w, h }
    }

    #[test]
    fn top_items_by_uses() {
        let items = |v: &[&str]| v.iter().map(PathBuf::from).collect::<Vec<_>>();
        let mut f = Folder { name: "x".into(), dir: "x".into(), cfg: Config::default(), groups: vec![] };
        f.groups.push(Group { name: "a".into(), dir: "a".into(), items: items(&["a/one.lnk", "a/two.lnk"]) });
        f.groups.push(Group { name: "b".into(), dir: "b".into(), items: items(&["b/three.lnk", "b/four.lnk"]) });
        f.cfg.uses.insert("four.lnk".into(), 5);
        f.cfg.uses.insert("two.lnk".into(), 2);
        assert_eq!(f.top_items(), items(&["b/four.lnk", "a/two.lnk", "a/one.lnk", "b/three.lnk"]));
    }

    #[test]
    fn settle_top_left() {
        let mut v = vec![r(0, 0, 2, 3), r(4, 6, 1, 3), r(1, 9, 1, 3)];
        settle(&mut v, 1);
        // up first (third one stops under the first), then left
        assert_eq!(v, vec![r(0, 0, 2, 3), r(2, 0, 1, 3), r(0, 3, 1, 3)]);
    }

    #[test]
    fn settle_top_right() {
        // right edge = extent (5 cols)
        let mut v = vec![r(0, 0, 2, 3), r(4, 6, 1, 3)];
        settle(&mut v, 2);
        // mirror of top-left: first slides to the right edge, second stops under it
        assert_eq!(v, vec![r(3, 0, 2, 3), r(4, 3, 1, 3)]);
    }

    #[test]
    fn settle_none_keeps() {
        let mut v = vec![r(3, 5, 2, 3)];
        settle(&mut v, 0);
        assert_eq!(v, vec![r(3, 5, 2, 3)]);
    }

    #[test]
    fn settle_no_overlap() {
        let mut v = vec![r(0, 0, 3, 5), r(0, 5, 1, 3), r(2, 9, 2, 3), r(5, 2, 1, 7)];
        settle(&mut v, 1);
        for i in 0..v.len() {
            for j in 0..i {
                assert!(!v[i].hits(&v[j]), "{:?} hits {:?}", v[i], v[j]);
            }
        }
    }
}
