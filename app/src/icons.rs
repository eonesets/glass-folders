// Кратко: значки ярлыков грузятся в фоне (несколько потоков) и хранятся в кэше, поэтому окно папки
// открывается сразу, а значки догружаются и плавно проявляются.
//
// Shell icon extraction costs ~30 ms per file (130 files = 4 s on the UI thread before).
// Workers (own COM STA each) extract pixels; the UI thread keeps the cache (path, px) -> Image.
// get() = cache only; misses go to want(). After new icons arrive, the on_ready callback runs
// on the UI thread (batched), and the app patches panels / tiles in place.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

type Key = (PathBuf, i32);

struct Pool {
    q: Mutex<VecDeque<Key>>,
    cv: Condvar,
}

struct Entry {
    img: Image,
    mtime: Option<SystemTime>,
}

const WORKERS: usize = 4;
/// New icons are handed to the app at most this often (fewer model updates while many arrive).
const BATCH: Duration = Duration::from_millis(30);

thread_local! {
    static CACHE: RefCell<HashMap<Key, Entry>> = RefCell::new(HashMap::new());
    static PENDING: RefCell<HashSet<Key>> = RefCell::new(HashSet::new());
    static READY: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
    static FLUSH: Cell<bool> = const { Cell::new(false) };
    static POOL: Arc<Pool> = start();
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn start() -> Arc<Pool> {
    let pool = Arc::new(Pool { q: Mutex::new(VecDeque::new()), cv: Condvar::new() });
    for _ in 0..WORKERS {
        let pool = pool.clone();
        std::thread::spawn(move || {
            crate::win::com_init();
            loop {
                let key = {
                    let mut q = pool.q.lock().unwrap();
                    loop {
                        if let Some(k) = q.pop_front() {
                            break k;
                        }
                        q = pool.cv.wait(q).unwrap();
                    }
                };
                let t = mtime(&key.0);
                let px: Option<SharedPixelBuffer<Rgba8Pixel>> = crate::win::icon_pixels(&key.0, key.1);
                let _ = slint::invoke_from_event_loop(move || arrived(key, px, t));
            }
        });
    }
    pool
}

fn arrived(key: Key, px: Option<SharedPixelBuffer<Rgba8Pixel>>, t: Option<SystemTime>) {
    PENDING.with(|p| p.borrow_mut().remove(&key));
    // failed = empty image (cached too: no endless retries)
    let img = px.map(Image::from_rgba8).unwrap_or_default();
    CACHE.with(|c| c.borrow_mut().insert(key, Entry { img, mtime: t }));
    if !FLUSH.with(|f| f.replace(true)) {
        slint::Timer::single_shot(BATCH, || {
            FLUSH.with(|f| f.set(false));
            if let Some(f) = READY.with(|r| r.borrow().clone()) {
                f();
            }
        });
    }
}

/// Called on the UI thread after new icons landed in the cache.
pub fn on_ready(f: impl Fn() + 'static) {
    READY.with(|r| *r.borrow_mut() = Some(Rc::new(f)));
}

/// Cached icon (maybe empty = shell has none). None = not loaded yet: ask with want().
/// A file changed since it was cached: old icon now, fresh one queued.
pub fn get(path: &Path, px: i32) -> Option<Image> {
    let key = (path.to_path_buf(), px);
    let hit = CACHE.with(|c| c.borrow().get(&key).map(|e| (e.img.clone(), e.mtime)));
    let (img, t) = hit?;
    if t != mtime(path) {
        want([path.to_path_buf()], px, false);
    }
    Some(img)
}

/// Queue icons to load. urgent = needed on screen now: before the background prefetch, in order.
pub fn want(paths: impl IntoIterator<Item = PathBuf>, px: i32, urgent: bool) {
    POOL.with(|pool| {
        let mut q = pool.q.lock().unwrap();
        let mut front = Vec::new();
        PENDING.with(|pend| {
            let mut pend = pend.borrow_mut();
            for p in paths {
                let key = (p, px);
                if pend.contains(&key) {
                    if urgent {
                        // already queued behind the prefetch: move it up
                        if let Some(i) = q.iter().position(|k| *k == key) {
                            q.remove(i);
                            front.push(key);
                        }
                    }
                    continue;
                }
                pend.insert(key.clone());
                if urgent {
                    front.push(key);
                } else {
                    q.push_back(key);
                }
            }
        });
        for k in front.into_iter().rev() {
            q.push_front(k);
        }
        pool.cv.notify_all();
    });
}

/// Drop icons of other sizes (icon size setting changed).
pub fn keep_sizes(px: &[i32]) {
    CACHE.with(|c| c.borrow_mut().retain(|k, _| px.contains(&k.1)));
}
