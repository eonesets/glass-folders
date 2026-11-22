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
pub const HEAD: f32 = 26.0; // group header
pub const GPAD: f32 = 4.0; // bottom padding of group

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
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct GroupCfg {
    pub w: Option<u32>,
    pub h: Option<u32>,
    pub color: Option<String>,
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
    /// First n items across groups, for tile preview.
    pub fn first_items(&self, n: usize) -> Vec<PathBuf> {
        self.groups.iter().flat_map(|g| g.items.iter().cloned()).take(n).collect()
    }
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
    Folder { name: name_of(dir), dir: dir.to_path_buf(), cfg, groups }
}

fn load_cfg(dir: &Path) -> Config {
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

/// App-wide settings (not per folder): %APPDATA%/glass-folders/settings.json
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub labels: bool,
    /// 0 dense .. 2 clear
    pub glass: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { labels: true, glass: 1 }
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

// ---------- layout ----------

pub struct SlotGeo {
    pub x: f32,
    pub y: f32,
    pub item: usize,
    pub more: usize,
}

pub struct GroupGeo {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub slots: Vec<SlotGeo>,
}

pub struct Layout {
    pub groups: Vec<GroupGeo>,
    pub w: f32,
    pub h: f32,
}

/// Near-square size that fits n items, width capped.
pub fn auto_size(n: usize, maxw: u32) -> (u32, u32) {
    let n = n.max(1) as f64;
    let w = (n.sqrt().ceil() as u32).clamp(1, maxw.max(1));
    let h = ((n / w as f64).ceil() as u32).max(1);
    (w, h)
}

pub fn layout(f: &Folder) -> Layout {
    let total: usize = f.groups.iter().map(|g| g.items.len().max(1)).sum();
    let cols = f
        .cfg
        .cols
        .unwrap_or_else(|| ((total as f64 * 1.6).sqrt().round() as u32).clamp(4, 8)) as usize;

    let mut bottom = vec![0f32; cols];
    let mut used = 1usize;
    let mut out = Vec::new();
    for g in &f.groups {
        let c = f.cfg.groups.get(&g.name);
        let (mut aw, mut ah) = auto_size(g.items.len(), cols as u32);
        if f.groups.len() == 1 && aw < 3 {
            // single group fills the min 3-cell panel width -> even edges
            aw = 3;
            ah = ((g.items.len().max(1) + 2) / 3) as u32;
        }
        let w = c.and_then(|c| c.w).unwrap_or(aw).clamp(1, cols as u32) as usize;
        let h = c.and_then(|c| c.h).unwrap_or(ah).max(1) as usize;
        let gw = w as f32 * PITCH - GAP;
        let gh = HEAD + h as f32 * PITCH - GAP + GPAD;

        // skyline: lowest spot, leftmost on tie
        let (mut best_c, mut best_y) = (0, f32::MAX);
        for col in 0..=cols - w {
            let y = bottom[col..col + w].iter().cloned().fold(0f32, f32::max);
            if y < best_y {
                best_y = y;
                best_c = col;
            }
        }
        for b in &mut bottom[best_c..best_c + w] {
            *b = best_y + gh + GAP;
        }
        used = used.max(best_c + w);

        let cap = w * h;
        let n = g.items.len();
        let shown = if n > cap { cap - 1 } else { n };
        let mut slots: Vec<SlotGeo> = (0..shown)
            .map(|i| SlotGeo {
                x: (i % w) as f32 * PITCH,
                y: HEAD + (i / w) as f32 * PITCH,
                item: i,
                more: 0,
            })
            .collect();
        if n > cap {
            let i = cap - 1;
            slots.push(SlotGeo {
                x: (i % w) as f32 * PITCH,
                y: HEAD + (i / w) as f32 * PITCH,
                item: 0,
                more: n - shown,
            });
        }
        out.push(GroupGeo { x: best_c as f32 * PITCH, y: best_y, w: gw, h: gh, slots });
    }
    let h = bottom.iter().cloned().fold(0f32, f32::max) - GAP;
    // min 3 cells wide: header (name + buttons) must fit
    let w = (used.max(3)) as f32 * PITCH - GAP;
    Layout { groups: out, w, h: h.max(CELL) }
}
