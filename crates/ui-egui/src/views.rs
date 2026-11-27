//! File views of the review diff: rendered Markdown, image compare, SVG and notebook cells (Figma frames 25, 69, 26, 78, 79, 80).
//! Every rendered view reads the same diff the source view draws, and says how to get back to it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use craft::egui::text::LayoutJob;
use craft::egui::{self, Color32, ColorImage, Context, FontId, Rect, RichText, Sense, Stroke, TextFormat, TextureHandle, TextureOptions, Ui};
use craft::flow::{Fmt, Inline};
use craft::image_codecs::{DecodeOptions, Limits};
use craft::markdown::{Refs, parse_inlines};
use pr_craft_core::ChangeRef;
use pr_craft_core::diff::{FileDiff, FileStatus};
use pr_craft_core::review::LineRef;
use pr_craft_core::view::{self, Cell, CellChange, CellKind, FileView, Mark, MdBlockView, MdItem, MdKind};
use pr_craft_engine::view::{self as engine, Side};
use pr_craft_forge::review::ChangeMeta;

use crate::keymap;
use crate::theme;

/// The most pixels of one image a view decodes, and of the canvas a Difference covers; a larger one shows facts only.
const MAX_PIXELS: u64 = 16 << 20;
const MAX_EDGE: u32 = 8192;
/// Decoded pixels the loaded rasters of all files may hold together. Past it, files far from the current one are dropped.
const PIXEL_BUDGET: u64 = 32 << 20;
/// Files read at once.
const WORKERS: usize = 4;

// ---- background work ----

type Job = Box<dyn FnOnce() + Send>;

/// Jobs waiting for a worker. An urgent job goes in front of the others.
struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    ready: Condvar,
    closed: AtomicBool,
}

impl Queue {
    fn jobs(&self) -> MutexGuard<'_, VecDeque<Job>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, job: Job, urgent: bool) {
        let mut jobs = self.jobs();
        if urgent {
            jobs.push_front(job);
        } else {
            jobs.push_back(job);
        }
        drop(jobs);
        self.ready.notify_one();
    }

    /// The next job, waiting for one; `None` once the pool is closed and nothing is left.
    fn next(&self) -> Option<Job> {
        let mut jobs = self.jobs();
        loop {
            if let Some(job) = jobs.pop_front() {
                return Some(job);
            }
            if self.closed.load(Ordering::Relaxed) {
                return None;
            }
            jobs = self.ready.wait(jobs).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// A fixed set of threads that read and decode files, so a change with a hundred images never starts a hundred reads.
struct Pool {
    queue: Arc<Queue>,
    workers: usize,
}

impl Pool {
    fn new() -> Self {
        let queue = Arc::new(Queue { jobs: Mutex::new(VecDeque::new()), ready: Condvar::new(), closed: AtomicBool::new(false) });
        let mut workers = 0;
        for n in 0..WORKERS {
            let queue = Arc::clone(&queue);
            let worker = std::thread::Builder::new().name(format!("file-view-{n}")).spawn(move || {
                // The lock is held only while waiting for the next job, never while one runs.
                while let Some(job) = queue.next() {
                    job();
                }
            });
            workers += usize::from(worker.is_ok());
        }
        Pool { queue, workers }
    }

    /// Queues `work`; its result lands in the slot. An urgent job runs before the ones already waiting. Dropping the
    /// slot before a worker takes the job cancels it, and a panic in `work` fails the slot instead of killing a worker.
    fn start<T: Send + 'static>(&self, ctx: &Context, urgent: bool, work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Slot<T> {
        if self.workers == 0 {
            return Slot::Failed("no thread could be started to read the file".into());
        }
        let (tx, rx) = mpsc::channel();
        let (cancelled, started) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (flag, begun, ctx) = (Arc::clone(&cancelled), Arc::clone(&started), ctx.clone());
        let job: Job = Box::new(move || {
            if flag.load(Ordering::Relaxed) {
                return;
            }
            begun.store(true, Ordering::Relaxed);
            let result = catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|_| Err("reading stopped: this file made a decoder fail".into()));
            // The slot may be gone by now; nobody is left to tell.
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.queue.push(job, urgent);
        Slot::Loading { rx, _cancel: Cancel(cancelled), started }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let mut jobs = self.queue.jobs();
        jobs.clear();
        self.queue.closed.store(true, Ordering::Relaxed);
        drop(jobs);
        self.queue.ready.notify_all();
    }
}

/// Sets the flag its job checks when the slot that wanted the result is dropped.
struct Cancel(Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

enum Slot<T> {
    Loading { rx: Receiver<Result<T, String>>, _cancel: Cancel, started: Arc<AtomicBool> },
    Ready(T),
    Failed(String),
}

impl<T> Slot<T> {
    fn poll(&mut self) {
        let Slot::Loading { rx, .. } = self else { return };
        *self = match rx.try_recv() {
            Ok(Ok(value)) => Slot::Ready(value),
            Ok(Err(error)) => Slot::Failed(error),
            Err(TryRecvError::Disconnected) => Slot::Failed("reading stopped before it finished".into()),
            Err(TryRecvError::Empty) => return,
        };
    }

    /// Waiting for a worker, not yet started.
    fn queued(&self) -> bool {
        matches!(self, Slot::Loading { started, .. } if !started.load(Ordering::Relaxed))
    }
}

// ---- what a file loads into ----

/// One side of a change: not there, not readable, or ready to show.
enum Version<T> {
    Absent,
    Failed(String),
    Shown(T),
}

struct Raster {
    bytes: usize,
    width: u32,
    height: u32,
    /// Held until the first frame uploads it.
    image: Option<ColorImage>,
    texture: Option<TextureHandle>,
}

impl Raster {
    fn new(bytes: usize, image: ColorImage) -> Self {
        Raster { bytes, width: image.size[0] as u32, height: image.size[1] as u32, image: Some(image), texture: None }
    }

    fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    fn texture(&mut self, ctx: &Context, name: &str) -> Option<&TextureHandle> {
        if self.texture.is_none()
            && let Some(image) = self.image.take()
        {
            self.texture = Some(ctx.load_texture(name, image, TextureOptions::LINEAR));
        }
        self.texture.as_ref()
    }
}

/// How far each pixel is from its counterpart, 0 (same) to 255, over the larger of the two images.
struct Magnitudes {
    width: usize,
    height: usize,
    values: Vec<u8>,
}

/// What a Difference can be made from.
#[derive(Clone)]
enum Mags {
    /// A side is missing or could not be read.
    Absent,
    /// The two canvases together cover more pixels than a Difference may hold.
    TooBig(u64, u64),
    Ready(Arc<Magnitudes>),
}

struct Compare {
    base: Version<Raster>,
    head: Version<Raster>,
    mags: Mags,
}

struct Vector {
    raster: Raster,
    width: f32,
    height: f32,
    counts: BTreeMap<&'static str, usize>,
    notes: Vec<String>,
}

/// What a cut left out of a block: whole lines, and characters of lines that were too long.
#[derive(Clone, Copy, Default)]
struct Left {
    lines: usize,
    chars: usize,
}

/// Text cut to the lines and line lengths a block may lay out.
struct Clipped {
    text: String,
    left: Left,
}

/// A notebook cell with its text already cut and its Markdown already laid out.
struct ShownCell {
    cell: Cell,
    change: CellChange,
    markdown: Option<LayoutJob>,
    source: Clipped,
}

struct Notebook {
    cells: Vec<ShownCell>,
    removed: Vec<Cell>,
}

/// What a block draws, prepared once when its file loads.
enum Body {
    Text { jobs: Vec<LayoutJob>, left: Left },
    Mono(Clipped),
    Code { language: Option<String>, text: Clipped },
    Rule,
}

struct Prepared {
    view: MdBlockView,
    body: Body,
}

enum Entry {
    Block(Prepared),
    /// Text the diff removed, shown where it was.
    Removed(LayoutJob, Left),
    /// Blocks and removed paragraphs the file has beyond what is laid out.
    Omitted(usize, usize),
}

struct Doc {
    entries: Vec<Entry>,
}

enum Loaded {
    Markdown(Doc),
    Images(Compare),
    Svg { base: Version<Vector>, head: Version<Vector> },
    Notebook(Notebook),
}

impl Loaded {
    /// Decoded pixels this file holds, for the budget.
    fn pixels(&self) -> u64 {
        let of = |v: &Version<Raster>| if let Version::Shown(r) = v { r.pixels() } else { 0 };
        let vector = |v: &Version<Vector>| if let Version::Shown(v) = v { v.raster.pixels() } else { 0 };
        match self {
            Loaded::Images(c) => {
                let mags = if let Mags::Ready(m) = &c.mags { m.values.len() as u64 / 4 } else { 0 };
                of(&c.base) + of(&c.head) + mags
            }
            Loaded::Svg { base, head } => vector(base) + vector(head),
            Loaded::Markdown(_) | Loaded::Notebook(_) => 0,
        }
    }
}

/// What the sliders of one image pair are set to, and the Difference picture they last made.
struct PairUi {
    swipe: f32,
    onion: f32,
    ignore: f32,
    /// The picture on screen; it stays until the one for a new threshold arrives.
    shown: Option<Shown>,
    pending: Option<(f32, Slot<(ColorImage, f32)>)>,
    failed: Option<(f32, String)>,
}

impl Default for PairUi {
    fn default() -> Self {
        PairUi { swipe: 0.5, onion: 0.5, ignore: 2.0, shown: None, pending: None, failed: None }
    }
}

struct Shown {
    ignore: f32,
    percent: f32,
    texture: TextureHandle,
}

/// Everything the review screen keeps about file views: the remembered choice per extension, this session's
/// choices, what each file's view loaded, and where the reader is in each.
pub(crate) struct Views {
    remembered: BTreeMap<String, FileView>,
    chosen: HashMap<String, FileView>,
    loads: HashMap<String, Slot<Loaded>>,
    pairs: HashMap<String, PairUi>,
    pool: Pool,
    /// Files asked for this frame and the last; a file the reader is looking at is never dropped for the budget.
    wanted: HashSet<String>,
    wanted_before: HashSet<String>,
    /// The block of a rendered Markdown file that J, K and C work on.
    cursor: HashMap<String, usize>,
    /// Focus the cursor block's comment button on this frame only.
    focus_cursor: Option<String>,
    unchanged_shown: HashSet<String>,
}

/// The views `file` offers. A deleted file has nothing to render but the images and SVG it had.
pub(crate) fn offered(file: &FileDiff) -> Vec<FileView> {
    let deleted_text = file.status == FileStatus::Deleted && matches!(view::extension(&file.path).as_str(), "md" | "markdown" | "ipynb");
    view::views_for(&file.path).iter().copied().filter(|v| !(deleted_text && *v != FileView::Source)).collect()
}

/// One loaded file as the budget sees it.
struct Held {
    /// Files between it and the current file.
    distance: usize,
    pixels: u64,
    protected: bool,
}

/// Which of `held` to drop, farthest from the current file first, until the rest fit in `budget` pixels.
fn to_evict(held: &[Held], budget: u64) -> Vec<usize> {
    let mut total: u64 = held.iter().map(|h| h.pixels).sum();
    let mut order: Vec<usize> = (0..held.len()).filter(|i| !held[*i].protected && held[*i].pixels > 0).collect();
    order.sort_by_key(|i| std::cmp::Reverse(held[*i].distance));
    let mut dropped = Vec::new();
    for i in order {
        if total <= budget {
            break;
        }
        total -= held[i].pixels;
        dropped.push(i);
    }
    dropped
}

impl Views {
    pub(crate) fn new(remembered: BTreeMap<String, FileView>) -> Self {
        Views {
            remembered,
            chosen: HashMap::new(),
            loads: HashMap::new(),
            pairs: HashMap::new(),
            pool: Pool::new(),
            wanted: HashSet::new(),
            wanted_before: HashSet::new(),
            cursor: HashMap::new(),
            focus_cursor: None,
            unchanged_shown: HashSet::new(),
        }
    }

    /// The view of `file` now: this session's choice, else the remembered one for its extension, else its first.
    pub(crate) fn current(&self, file: &FileDiff) -> FileView {
        let offered = offered(file);
        let wanted = self.chosen.get(&file.path).or_else(|| self.remembered.get(&view::extension(&file.path))).copied();
        wanted.filter(|v| offered.contains(v)).or_else(|| offered.first().copied()).unwrap_or(FileView::Source)
    }

    /// Records this session's choice for one file; the per-extension choice is saved by `review.set_view` and read back with [`Views::remember`].
    pub(crate) fn choose(&mut self, path: &str, view: FileView) {
        self.chosen.insert(path.to_string(), view);
    }

    pub(crate) fn remember(&mut self, saved: BTreeMap<String, FileView>) {
        self.remembered = saved;
    }

    /// Takes in finished reads, then drops the loaded files farthest from `current` while the decoded pixels exceed the budget.
    pub(crate) fn tidy(&mut self, files: &[FileDiff], current: usize) {
        self.loads.values_mut().for_each(Slot::poll);
        self.wanted_before = std::mem::take(&mut self.wanted);
        let at = |path: &str| files.iter().position(|f| f.path == path);
        // A read nobody is looking at any more is cancelled; it would only hold a place in the queue.
        let looked_at = |path: &String| at(path) == Some(current) || self.wanted_before.contains(path);
        self.loads.retain(|path, slot| !matches!(slot, Slot::Loading { .. }) || looked_at(path));
        let mut paths = Vec::new();
        let mut held = Vec::new();
        for (path, slot) in &self.loads {
            let Slot::Ready(loaded) = slot else { continue };
            let index = at(path);
            paths.push(path.clone());
            held.push(Held {
                distance: index.map_or(usize::MAX, |i| i.abs_diff(current)),
                pixels: loaded.pixels(),
                protected: index == Some(current) || self.wanted_before.contains(path),
            });
        }
        for i in to_evict(&held, PIXEL_BUDGET) {
            self.loads.remove(&paths[i]);
            self.pairs.remove(&paths[i]);
        }
    }

    /// Starts reading what `file`'s rendered view draws, once, and marks it as in use this frame. The current file
    /// (`urgent`) is read before files that are only near the window, even if it was already waiting behind them.
    pub(crate) fn want(&mut self, ctx: &Context, change: &ChangeRef, meta: &ChangeMeta, file: &FileDiff, urgent: bool) {
        self.wanted.insert(file.path.clone());
        if let Some(slot) = self.loads.get(&file.path)
            && !(urgent && slot.queued())
        {
            return;
        }
        let (change, meta, file) = (change.clone(), meta.clone(), file.clone());
        let path = file.path.clone();
        // Replacing a queued slot drops it, which cancels the job it waited with.
        let slot = self.pool.start(ctx, urgent, move || load(&change, &meta, &file));
        self.loads.insert(path, slot);
    }

    fn loaded(&self, path: &str) -> Option<&Loaded> {
        match self.loads.get(path) {
            Some(Slot::Ready(loaded)) => Some(loaded),
            _ => None,
        }
    }

    /// The blocks of a rendered Markdown file, once loaded.
    fn blocks(&self, path: &str) -> Option<&[Entry]> {
        match self.loaded(path) {
            Some(Loaded::Markdown(doc)) => Some(&doc.entries),
            _ => None,
        }
    }

    /// Whether J and K walk blocks of this file now.
    pub(crate) fn walks_blocks(&self, file: &FileDiff) -> bool {
        self.current(file) == FileView::Rendered && self.blocks(&file.path).is_some()
    }

    /// Moves the block cursor to the next or previous changed block and focuses its comment button. False when there
    /// is none that way, so the caller moves on to another file.
    pub(crate) fn step_block(&mut self, path: &str, forward: bool) -> bool {
        let Some(entries) = self.blocks(path) else { return false };
        let changed: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| matches!(entry, Entry::Block(b) if b.view.mark != Mark::Unchanged))
            .map(|(i, _)| i)
            .collect();
        let next = match (self.cursor.get(path).copied(), forward) {
            (None, true) => changed.first(),
            (None, false) => changed.last(),
            (Some(here), true) => changed.iter().find(|i| **i > here),
            (Some(here), false) => changed.iter().rev().find(|i| **i < here),
        };
        let Some(&item) = next else { return false };
        self.cursor.insert(path.to_string(), item);
        self.focus_cursor = Some(path.to_string());
        true
    }

    /// The block `C` comments on: the cursor, else the first changed block, with the source line it posts to.
    pub(crate) fn comment_target(&self, path: &str) -> Option<(usize, Option<LineRef>)> {
        let entries = self.blocks(path)?;
        let block = |i: usize| match entries.get(i) {
            Some(Entry::Block(b)) => Some((i, b.view.anchor)),
            _ => None,
        };
        let first_changed = entries.iter().position(|entry| matches!(entry, Entry::Block(b) if b.view.mark != Mark::Unchanged));
        self.cursor.get(path).copied().or(first_changed).and_then(block)
    }

    /// The source line block `item` of `path` posts to, and what to call the block.
    pub(crate) fn block_anchor(&self, path: &str, item: usize) -> Option<(Option<LineRef>, &'static str)> {
        match self.blocks(path)?.get(item)? {
            Entry::Block(b) => Some((b.view.anchor, kind_name(b.view.block.kind))),
            Entry::Removed(..) | Entry::Omitted(..) => None,
        }
    }

    pub(crate) fn set_cursor(&mut self, path: &str, item: usize) {
        self.cursor.insert(path.to_string(), item);
    }
}

pub(crate) fn kind_name(kind: MdKind) -> &'static str {
    match kind {
        MdKind::Heading(_) => "heading",
        MdKind::Paragraph => "paragraph",
        MdKind::List => "list",
        MdKind::Quote => "quote",
        MdKind::Code => "code block",
        MdKind::Table => "table",
        MdKind::Rule => "rule",
    }
}

// ---- loading, on a worker ----

fn load(change: &ChangeRef, meta: &ChangeMeta, file: &FileDiff) -> Result<Loaded, String> {
    match view::extension(&file.path).as_str() {
        "md" | "markdown" => {
            let head = read_side(change, meta, Side::Head, &file.path, file.status != FileStatus::Deleted);
            let Version::Shown(bytes) = head else { return Err(version_error(head, "the new version")) };
            let text = String::from_utf8(bytes).map_err(|_| "the file is not UTF-8 text".to_string())?;
            Ok(Loaded::Markdown(prepare(view::md_items(&text, file))))
        }
        "ipynb" => load_notebook(change, meta, file),
        "svg" => {
            let (base, head) = fetch_both(change, meta, file);
            Ok(Loaded::Svg { base: vector(base), head: vector(head) })
        }
        _ => {
            let (base, head) = fetch_both(change, meta, file);
            let (base, head) = (raster(base), raster(head));
            let mags = match (&base, &head) {
                (Version::Shown(a), Version::Shown(b)) => match (&a.image, &b.image) {
                    (Some(a), Some(b)) => difference(a, b),
                    _ => Mags::Absent,
                },
                _ => Mags::Absent,
            };
            Ok(Loaded::Images(Compare { base, head, mags }))
        }
    }
}

fn version_error<T>(version: Version<T>, what: &str) -> String {
    match version {
        Version::Failed(error) => format!("could not read {what}: {error}"),
        _ => format!("{what} is not in the change"),
    }
}

/// One side of `file`; a side the diff says does not exist is not asked for.
fn read_side(change: &ChangeRef, meta: &ChangeMeta, side: Side, path: &str, exists: bool) -> Version<Vec<u8>> {
    if !exists {
        return Version::Absent;
    }
    match engine::blob_at(change, meta, path, side) {
        Ok(Some(bytes)) => Version::Shown(bytes),
        Ok(None) => Version::Failed(format!("{path} is not on {} at that commit", change.forge.name())),
        Err(error) => Version::Failed(error.to_string()),
    }
}

/// Both sides of `file` at once.
fn fetch_both(change: &ChangeRef, meta: &ChangeMeta, file: &FileDiff) -> (Version<Vec<u8>>, Version<Vec<u8>>) {
    let base_path = file.old_path.as_deref().unwrap_or(&file.path);
    std::thread::scope(|scope| {
        let base = scope.spawn(|| read_side(change, meta, Side::Base, base_path, file.status != FileStatus::Added));
        let head = read_side(change, meta, Side::Head, &file.path, file.status != FileStatus::Deleted);
        (base.join().unwrap_or_else(|_| Version::Failed("reading stopped".into())), head)
    })
}

fn load_notebook(change: &ChangeRef, meta: &ChangeMeta, file: &FileDiff) -> Result<Loaded, String> {
    let (base, head) = fetch_both(change, meta, file);
    let parse = |bytes: Vec<u8>| view::parse_notebook(&String::from_utf8_lossy(&bytes));
    let Version::Shown(head) = head else { return Err(version_error(head, "the new version")) };
    let base = match base {
        Version::Shown(bytes) => Some(parse(bytes).map_err(|e| format!("the old version: {e}"))?),
        Version::Absent => None,
        failed @ Version::Failed(_) => return Err(version_error(failed, "the old version")),
    };
    let head = parse(head)?;
    let (changes, removed) = view::cell_changes(base.as_ref(), &head);
    let cells = head.cells.into_iter().zip(changes).map(|(cell, change)| shown_cell(cell, change)).collect();
    Ok(Loaded::Notebook(Notebook { cells, removed }))
}

fn shown_cell(cell: Cell, change: CellChange) -> ShownCell {
    let source = clip(&cell.source, theme::MAX_BLOCK_LINES);
    let baked = theme::baked();
    let markdown = (cell.kind == CellKind::Markdown).then(|| job(&runs(&source.text, ""), baked.sizes.body, baked.palette.graphite));
    ShownCell { cell, change, markdown, source }
}

fn raster(bytes: Version<Vec<u8>>) -> Version<Raster> {
    match bytes {
        Version::Shown(bytes) => match decode(&bytes) {
            Ok(image) => Version::Shown(Raster::new(bytes.len(), image)),
            Err(error) => Version::Failed(error),
        },
        Version::Absent => Version::Absent,
        Version::Failed(error) => Version::Failed(error),
    }
}

/// Decodes with the pixel limit, so a huge image is refused before it is allocated.
fn decode(bytes: &[u8]) -> Result<ColorImage, String> {
    let limits = Limits { max_width: MAX_EDGE, max_height: MAX_EDGE, max_pixels: MAX_PIXELS, max_alloc: 512 << 20 };
    let image = craft::image_codecs::decode_with(bytes, &DecodeOptions { limits, keep_orientation: false }).map_err(|e| e.to_string())?;
    Ok(ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], &image.to_rgba8()))
}

/// Whether the canvas that holds both images stays within the pixel limit.
fn canvas_fits(width: u64, height: u64) -> bool {
    width.saturating_mul(height) <= MAX_PIXELS
}

fn difference(base: &ColorImage, head: &ColorImage) -> Mags {
    let (width, height) = (base.size[0].max(head.size[0]), base.size[1].max(head.size[1]));
    if !canvas_fits(width as u64, height as u64) {
        return Mags::TooBig(width as u64, height as u64);
    }
    let pixel =
        |image: &ColorImage, x: usize, y: usize| (x < image.size[0] && y < image.size[1]).then(|| image.pixels[y * image.size[0] + x].to_array());
    let mut values = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            values.push(match (pixel(base, x, y), pixel(head, x, y)) {
                (Some(a), Some(b)) => a.iter().zip(b).map(|(p, q)| p.abs_diff(q)).max().unwrap_or(0),
                _ => 255,
            });
        }
    }
    Mags::Ready(Arc::new(Magnitudes { width, height, values }))
}

/// The Difference picture: black where the pixel changed less than `ignore` percent, a bright grey where it changed
/// more; and the share of pixels shown changed.
fn difference_picture(mags: &Magnitudes, ignore: f32) -> (ColorImage, f32) {
    let floor = ((ignore / 100.0 * 255.0).ceil() as u8).max(1);
    let shown: Vec<u8> = mags.values.iter().map(|v| if *v >= floor { v.saturating_mul(3).max(96) } else { 0 }).collect();
    let changed = mags.values.iter().filter(|v| **v >= floor).count();
    let percent = if mags.values.is_empty() { 0.0 } else { changed as f32 * 100.0 / mags.values.len() as f32 };
    (ColorImage::from_gray([mags.width, mags.height], &shown), percent)
}

fn vector(bytes: Version<Vec<u8>>) -> Version<Vector> {
    match bytes {
        Version::Shown(bytes) => match render_svg(&bytes) {
            Ok(vector) => Version::Shown(vector),
            Err(error) => Version::Failed(error),
        },
        Version::Absent => Version::Absent,
        Version::Failed(error) => Version::Failed(error),
    }
}

/// Parses an SVG within craft's limits on a thread with the stack its parser needs, draws it, and counts its tree.
fn render_svg(bytes: &[u8]) -> Result<Vector, String> {
    std::thread::scope(|scope| {
        let job = std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(scope, || draw_svg(bytes)).map_err(|e| e.to_string())?;
        job.join().unwrap_or_else(|_| Err("drawing the SVG stopped".into()))
    })
}

fn draw_svg(bytes: &[u8]) -> Result<Vector, String> {
    use craft::svg::tree::draw::tiny_skia::Transform;
    use craft::svg::tree::{Limits, image_resolver, preflight, text, usvg};
    let limits = Limits { tree_depth: 48, depth: 96, ..Limits::default() };
    let source = text(bytes, &limits).map_err(|e| e.to_string())?;
    let checked = preflight(&source, &limits).map_err(|e| e.to_string())?;
    let mut notes: Vec<String> = checked.unsupported_elements().into_iter().map(|(tag, n)| format!("<{tag}> ×{n} is not drawn")).collect();
    if source.contains("<text") {
        notes.push("text is not drawn: no fonts are loaded here".into());
    }
    let options = usvg::Options { image_href_resolver: image_resolver(limits), ..usvg::Options::default() };
    let tree = checked.parse(&options).map_err(|e| e.to_string())?;
    let size = tree.size();
    let (width, height) = (size.width(), size.height());
    if width <= 0.0 || height <= 0.0 {
        return Err("the SVG has no size".into());
    }
    let scale = theme::SVG_RENDER / width.max(height);
    let (px_w, px_h) = ((width * scale).ceil().max(1.0) as u32, (height * scale).ceil().max(1.0) as u32);
    let pixmap = craft::svg::tree::draw::render(&tree, Transform::from_scale(scale, scale), px_w, px_h, 0, 256 << 20).map_err(|r| r.message)?;
    let image = ColorImage::from_rgba_premultiplied([px_w as usize, px_h as usize], pixmap.data());
    let mut counts = BTreeMap::new();
    count_nodes(tree.root(), &mut counts);
    Ok(Vector { raster: Raster::new(bytes.len(), image), width, height, counts, notes })
}

fn count_nodes(group: &craft::svg::tree::usvg::Group, counts: &mut BTreeMap<&'static str, usize>) {
    use craft::svg::tree::usvg::Node;
    for node in group.children() {
        let name = match node {
            Node::Group(inner) => {
                count_nodes(inner, counts);
                "groups"
            }
            Node::Path(_) => "shapes",
            Node::Image(_) => "embedded images",
            Node::Text(_) => "text runs",
        };
        *counts.entry(name).or_default() += 1;
    }
}

// ---- drawing ----

/// Draws the rendered `view` of `file`. `after_block` draws what belongs under a Markdown block (its composer and
/// drafts). Returns what the reader pressed in a Markdown file.
pub(crate) fn show(
    ui: &mut Ui,
    views: &mut Views,
    file: &FileDiff,
    view: FileView,
    width: f32,
    after_block: &mut dyn FnMut(&mut Ui, usize, &MdBlockView),
) -> Option<Pressed> {
    let hint = format!("Rendered from the new version. {} shows the source diff of {}.", keymap::label(ui, "review.toggle_source"), file.path);
    ui.label(RichText::new(hint).size(theme::sz(ui).body).color(theme::pal(ui).text_tertiary));
    let path = file.path.as_str();
    let focus = views.focus_cursor.as_deref() == Some(path);
    if focus {
        views.focus_cursor = None;
    }
    match views.loads.get(path) {
        None => {
            note(ui, format!("{path} loads when it scrolls into view."));
            return None;
        }
        Some(Slot::Loading { .. }) => {
            crate::live_label(ui, format!("Reading {path} from the forge…"), theme::pal(ui).text_secondary);
            return None;
        }
        Some(Slot::Failed(error)) => {
            let key = keymap::label(ui, "review.toggle_source");
            crate::live_label(ui, format!("Could not show {path} rendered: {error}. Press {key} for the source diff."), theme::pal(ui).rose_text);
            return None;
        }
        Some(Slot::Ready(_)) => {}
    }
    let cursor = views.cursor.get(path).copied();
    let Some(Slot::Ready(loaded)) = views.loads.get_mut(path) else { return None };
    let pair = views.pairs.entry(path.to_string()).or_default();
    match loaded {
        Loaded::Markdown(doc) => return markdown(ui, doc, cursor, focus, after_block),
        Loaded::Images(compare) => images(ui, compare, pair, view, width, &views.pool),
        Loaded::Svg { base, head } => svg(ui, base, head, width),
        Loaded::Notebook(notebook) => notebook_ui(ui, notebook, path, &mut views.unchanged_shown),
    }
    None
}

fn note(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).size(theme::sz(ui).body).color(theme::pal(ui).text_secondary));
}

/// Every rendered view that is not Markdown has no line to pin a comment to (rule 15).
fn no_pins(ui: &mut Ui, what: &str) {
    let key = keymap::label(ui, "review.toggle_source");
    note(ui, format!("Comments are not pinned to a point on {what}. Press {key}, then comment on a line of the source."));
}

/// `line` cut to `max` characters, and how many characters were left out.
fn cap_chars(line: &str, max: usize) -> (&str, usize) {
    match line.char_indices().nth(max) {
        Some((at, _)) => (line.get(..at).unwrap_or(line), line.get(at..).map_or(0, |rest| rest.chars().count())),
        None => (line, 0),
    }
}

/// The first `max_lines` lines of `text`, each cut to [`theme::MAX_LINE_CHARS`], the whole cut to
/// [`theme::MAX_BLOCK_CHARS`], and what was left out.
fn clip(text: &str, max_lines: usize) -> Clipped {
    let mut left = Left { lines: text.lines().count().saturating_sub(max_lines), chars: 0 };
    let mut kept = Vec::new();
    for line in text.lines().take(max_lines) {
        let (line, cut) = cap_chars(line, theme::MAX_LINE_CHARS);
        left.chars += cut;
        kept.push(line);
    }
    let joined = kept.join("\n");
    let (joined, cut) = cap_chars(joined.trim_end(), theme::MAX_BLOCK_CHARS);
    left.chars += cut;
    Clipped { text: joined.to_string(), left }
}

fn more_note(ui: &mut Ui, left: Left) {
    let what = match (left.lines, left.chars) {
        (0, 0) => return,
        (lines, 0) => format!("{lines} more lines"),
        (0, chars) => format!("{chars} more characters"),
        (lines, chars) => format!("{lines} more lines and {chars} more characters"),
    };
    note(ui, format!("… {what}, press {} for the source", keymap::label(ui, "review.toggle_source")));
}

// ---- Markdown ----

struct Run {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
    strike: bool,
    link: bool,
    new: bool,
}

/// The inline runs of `text`; with `old` given, each word that `old` lacks is marked new.
fn runs(text: &str, old: &str) -> Vec<Run> {
    let mut out = Vec::new();
    for span in parse_inlines(text, &Fmt::default(), 0, &Refs::new()) {
        let Inline::Text(piece, fmt) = span else { continue };
        let words = if old.is_empty() { vec![(piece, false)] } else { view::mark_words(old, &piece) };
        out.extend(words.into_iter().map(|(text, new)| Run {
            text,
            bold: fmt.bold,
            italic: fmt.italic,
            code: fmt.code,
            strike: fmt.strike,
            link: fmt.link.is_some(),
            new,
        }));
    }
    out
}

/// Bold has no font weight here, so it is the darker ink; new words are underlined as well as tinted.
fn job(runs: &[Run], size: f32, ink: Color32) -> LayoutJob {
    let palette = theme::baked().palette;
    let mut job = LayoutJob::default();
    for run in runs {
        let colour = if run.link {
            palette.ember_text
        } else if run.bold {
            palette.graphite
        } else {
            ink
        };
        let line = |on: bool| if on { Stroke::new(1.0, colour) } else { Stroke::NONE };
        let background = if run.code {
            palette.border
        } else if run.new {
            palette.moss_bg
        } else {
            Color32::TRANSPARENT
        };
        let font_id = if run.code { FontId::monospace(size * 0.92) } else { FontId::proportional(size) };
        let format = TextFormat {
            font_id,
            color: colour,
            italics: run.italic,
            underline: line(run.link || run.new),
            strikethrough: line(run.strike),
            background,
            ..TextFormat::default()
        };
        job.append(&run.text, 0.0, format);
    }
    job
}

/// A bullet's marker becomes a dot, a quote's `>` a bar; the indent stays.
fn list_line(line: &str, kind: MdKind) -> String {
    let indent = line.len() - line.trim_start().len();
    let rest = line.trim_start();
    let pad = " ".repeat(indent);
    if kind == MdKind::Quote {
        return format!("▎ {}", rest.trim_start_matches('>').trim_start());
    }
    match rest.strip_prefix(['-', '*', '+']).filter(|r| r.starts_with(' ')) {
        Some(item) => format!("{pad}• {}", item.trim_start()),
        None => format!("{pad}{rest}"),
    }
}

/// A fenced block's language and the lines between its fences. An unclosed fence runs to the end of the file, so its
/// last line is code, not a fence.
fn code_parts(lines: &[String]) -> (Option<String>, Vec<&str>) {
    let Some(open) = lines.first().map(|l| l.trim()) else { return (None, Vec::new()) };
    let marker = open.chars().next().unwrap_or('`');
    let width = open.chars().take_while(|c| *c == marker).count();
    let language = Some(open.trim_start_matches(marker).trim()).filter(|l| !l.is_empty()).map(str::to_string);
    let body = lines.get(1..).unwrap_or_default();
    let closed = body.last().is_some_and(|l| {
        let t = l.trim();
        !t.is_empty() && t.chars().all(|c| c == marker) && t.chars().count() >= width
    });
    let inner = if closed { body.get(..body.len() - 1).unwrap_or_default() } else { body };
    (language, inner.iter().map(String::as_str).collect())
}

/// What the closing note says when a file has more blocks than are laid out.
fn omitted_text(blocks: usize, removed: usize, source_key: &str) -> String {
    let what = match (blocks, removed) {
        (blocks, 0) => format!("{blocks} more blocks"),
        (0, removed) => format!("{removed} more removed paragraphs"),
        (blocks, removed) => format!("{blocks} more blocks and {removed} more removed paragraphs"),
    };
    format!("… {what}, press {source_key} for the source")
}

/// Lays out every block once, when its file loads: inline runs are parsed and text is cut to what a block may show.
fn prepare(items: Vec<MdItem>) -> Doc {
    let entries = items
        .into_iter()
        .map(|item| match item {
            MdItem::Removed(text) => {
                let clipped = clip(&text, theme::MAX_BLOCK_LINES);
                let baked = theme::baked();
                let mut struck = job(&runs(&clipped.text, ""), baked.sizes.body, baked.palette.text_secondary);
                struck.sections.iter_mut().for_each(|s| s.format.strikethrough = Stroke::new(1.0, baked.palette.text_secondary));
                Entry::Removed(struck, clipped.left)
            }
            MdItem::Omitted { blocks, removed } => Entry::Omitted(blocks, removed),
            MdItem::Block(view) => {
                let body = body_of(&view);
                Entry::Block(Prepared { view, body })
            }
        })
        .collect();
    Doc { entries }
}

/// The first lines of a block, each cut to the line limit, and what that left out.
fn capped(lines: &[String]) -> (Vec<&str>, Left) {
    let mut left = Left { lines: lines.len().saturating_sub(theme::MAX_BLOCK_LINES), chars: 0 };
    let kept = lines
        .iter()
        .take(theme::MAX_BLOCK_LINES)
        .map(|line| {
            let (line, cut) = cap_chars(line, theme::MAX_LINE_CHARS);
            left.chars += cut;
            line
        })
        .collect();
    (kept, left)
}

fn body_of(view: &MdBlockView) -> Body {
    let lines = &view.block.lines;
    // The removed text only tells which words are new, so a long one is cut like the rest.
    let old = cap_chars(&view.old, theme::MAX_BLOCK_CHARS).0;
    let baked = theme::baked();
    match view.block.kind {
        MdKind::Heading(level) => {
            let text = lines.first().map(|l| l.trim().trim_start_matches('#').trim().trim_end_matches('#').trim_end()).unwrap_or_default();
            let (text, chars) = cap_chars(text, theme::MAX_LINE_CHARS);
            let size = if level <= 2 { baked.sizes.heading } else { baked.sizes.title };
            Body::Text { jobs: vec![job(&runs(text, old), size, baked.palette.graphite)], left: Left { lines: 0, chars } }
        }
        MdKind::Paragraph => {
            let (kept, mut left) = capped(lines);
            let joined = kept.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
            let (text, chars) = cap_chars(&joined, theme::MAX_BLOCK_CHARS);
            left.chars += chars;
            Body::Text { jobs: vec![job(&runs(text, old), baked.sizes.body, baked.palette.graphite)], left }
        }
        MdKind::List | MdKind::Quote => {
            let (kept, left) = capped(lines);
            let jobs = kept.iter().map(|line| job(&runs(&list_line(line, view.block.kind), old), baked.sizes.body, baked.palette.graphite)).collect();
            Body::Text { jobs, left }
        }
        MdKind::Code => {
            let (language, inner) = code_parts(lines);
            Body::Code { language, text: clip(&inner.join("\n"), theme::MAX_BLOCK_LINES) }
        }
        // shortcut: a table shows as its source text, aligned; lay it out as a grid when a table's columns matter.
        MdKind::Table => Body::Mono(clip(&lines.join("\n"), theme::MAX_BLOCK_LINES)),
        MdKind::Rule => Body::Rule,
    }
}

/// What the reader pressed in a rendered Markdown file: a block to put the cursor on, or its comment button.
pub(crate) enum Pressed {
    Select(usize),
    Comment(usize),
}

fn markdown(
    ui: &mut Ui,
    doc: &Doc,
    cursor: Option<usize>,
    focus: bool,
    after_block: &mut dyn FnMut(&mut Ui, usize, &MdBlockView),
) -> Option<Pressed> {
    let mut pressed = None;
    for (i, entry) in doc.entries.iter().enumerate() {
        match entry {
            Entry::Removed(text, left) => removed_block(ui, text, *left),
            Entry::Omitted(blocks, removed) => note(ui, omitted_text(*blocks, *removed, &keymap::label(ui, "review.toggle_source"))),
            Entry::Block(block) => {
                let selected = cursor == Some(i);
                let hit = block_ui(ui, block, selected, focus && selected);
                pressed = hit.map(|comment| if comment { Pressed::Comment(i) } else { Pressed::Select(i) }).or(pressed);
                after_block(ui, i, &block.view);
            }
        }
        ui.add_space(theme::SPACE_2);
    }
    pressed
}

fn removed_block(ui: &mut Ui, text: &LayoutJob, left: Left) {
    egui::Frame::new().fill(theme::pal(ui).rose_bg).inner_margin(theme::SPACE_2).show(ui, |ui| {
        ui.label(RichText::new("Removed paragraph").size(theme::sz(ui).body).color(theme::pal(ui).rose_text));
        ui.add(egui::Label::new(theme::restyle(ui, text)).wrap());
        more_note(ui, left);
    });
}

/// One block: its mark in words, its text, and a comment button when it can take one. `Some(true)` when the comment
/// button was pressed, `Some(false)` when the block was clicked.
fn block_ui(ui: &mut Ui, block: &Prepared, selected: bool, focus: bool) -> Option<bool> {
    let view = &block.view;
    let fill = match (selected, view.mark) {
        (true, _) => theme::pal(ui).ember_subtle,
        (_, Mark::Unchanged) => Color32::TRANSPARENT,
        _ => theme::pal(ui).moss_bg,
    };
    let mut pressed = None;
    egui::Frame::new().fill(fill).inner_margin(theme::SPACE_2).show(ui, |ui| {
        if let Some(word) = view.mark.word() {
            let marker = if selected { "▸ " } else { "" };
            ui.label(
                RichText::new(format!("{marker}{word} {}", kind_name(view.block.kind))).size(theme::sz(ui).body).color(theme::pal(ui).moss_text),
            );
        }
        if body_ui(ui, &block.body) {
            pressed = Some(false);
        }
        // Changed blocks always offer a comment; any block does once the cursor is on it.
        if (view.mark != Mark::Unchanged || selected) && comment_button(ui, view, focus) {
            pressed = Some(true);
        }
    });
    pressed
}

fn comment_button(ui: &mut Ui, view: &MdBlockView, focus: bool) -> bool {
    match view.anchor {
        Some(anchor) => {
            let button = ui.button(format!(
                "Comment on this {}  {}  · line {}",
                kind_name(view.block.kind),
                keymap::label(ui, "review.comment"),
                anchor.shown()
            ));
            if focus {
                button.request_focus();
                button.scroll_to_me(Some(egui::Align::Center));
            }
            button.clicked()
        }
        None => {
            // Rule 15: say why, do not hide the control.
            let key = keymap::label(ui, "review.toggle_source");
            note(ui, format!("Outside the diff: the forge takes comments only on lines the diff shows. Press {key} to see where."));
            false
        }
    }
}

/// Draws the block's text; true when it was clicked.
fn body_ui(ui: &mut Ui, body: &Body) -> bool {
    match body {
        Body::Text { jobs, left } => {
            let mut clicked = false;
            for job in jobs {
                clicked |= ui.add(egui::Label::new(theme::restyle(ui, job)).wrap().sense(Sense::click())).clicked();
            }
            more_note(ui, *left);
            clicked
        }
        Body::Mono(text) => mono(ui, text),
        Body::Code { language, text } => {
            egui::Frame::new()
                .fill(theme::pal(ui).raised)
                .stroke((1.0, theme::pal(ui).border))
                .inner_margin(theme::SPACE_2)
                .show(ui, |ui| {
                    if let Some(language) = language {
                        ui.label(RichText::new(language).size(theme::sz(ui).body).color(theme::pal(ui).text_tertiary));
                    }
                    mono(ui, text)
                })
                .inner
        }
        Body::Rule => {
            ui.separator();
            false
        }
    }
}

fn mono(ui: &mut Ui, text: &Clipped) -> bool {
    let clicked = ui
        .add(
            egui::Label::new(RichText::new(&text.text).monospace().size(theme::sz(ui).code).color(theme::pal(ui).graphite))
                .extend()
                .sense(Sense::click()),
        )
        .clicked();
    more_note(ui, text.left);
    clicked
}

// ---- images ----

/// The scale that fits `w` × `h` into `max_w` × `max_h`; small images grow, up to four times, large ones only shrink.
fn fit(w: f32, h: f32, max_w: f32, max_h: f32) -> f32 {
    let room = (max_w / w.max(1.0)).min(max_h / h.max(1.0));
    room.min(if w.max(h) < theme::SMALL_IMAGE { 4.0 } else { 1.0 })
}

fn size_text(bytes: usize) -> String {
    match bytes {
        0..1000 => format!("{bytes} B"),
        1000..1_000_000 => format!("{:.0} KB", bytes as f64 / 1000.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_000_000.0),
    }
}

fn dims_text(r: &Raster) -> String {
    format!("{} × {}", r.width, r.height)
}

/// What a side that is not a picture says.
fn missing(ui: &mut Ui, version: &Version<Raster>, absent: &str) {
    match version {
        Version::Absent => note(ui, absent),
        Version::Failed(error) => crate::live_label(ui, format!("Could not show this version: {error}"), theme::pal(ui).rose_text),
        Version::Shown(_) => {}
    }
}

fn images(ui: &mut Ui, compare: &mut Compare, state: &mut PairUi, view: FileView, width: f32, pool: &Pool) {
    no_pins(ui, "an image");
    facts(ui, compare);
    let both = matches!((&compare.base, &compare.head), (Version::Shown(_), Version::Shown(_)));
    match view {
        FileView::ImageSwipe | FileView::ImageOnion if both => overlay(ui, compare, state, view, width),
        FileView::ImageDifference if both => match compare.mags.clone() {
            Mags::Ready(mags) => difference_ui(ui, &mags, state, width, pool),
            Mags::TooBig(w, h) => {
                note(ui, format!("A Difference needs a {w} × {h} canvas, over the {} million pixel limit. Showing 2-up.", MAX_PIXELS >> 20));
                two_up(ui, compare, width);
            }
            Mags::Absent => {
                note(ui, "A Difference could not be made from these versions. Showing 2-up.");
                two_up(ui, compare, width);
            }
        },
        _ => {
            if !both && view != FileView::ImageTwoUp {
                note(ui, "This view needs both versions; the file exists on one side only. Showing it as 2-up.");
            }
            two_up(ui, compare, width);
        }
    }
}

fn facts(ui: &mut Ui, compare: &Compare) {
    let (Version::Shown(base), Version::Shown(head)) = (&compare.base, &compare.head) else { return };
    egui::Grid::new("image-facts").num_columns(4).spacing([theme::SPACE_3, 2.0]).show(ui, |ui| {
        let cell = |ui: &mut Ui, text: String| ui.label(RichText::new(text).size(theme::sz(ui).body).color(theme::pal(ui).graphite));
        cell(ui, "Dimensions".into());
        cell(ui, dims_text(base));
        cell(ui, dims_text(head));
        let moved = (base.width, base.height) != (head.width, head.height);
        cell(ui, if moved { "changed".into() } else { "same".into() });
        ui.end_row();
        cell(ui, "File size".into());
        cell(ui, size_text(base.bytes));
        cell(ui, size_text(head.bytes));
        let delta = (head.bytes as f64 - base.bytes as f64) * 100.0 / (base.bytes.max(1) as f64);
        cell(ui, format!("{delta:+.0}%").replace('-', "−"));
        ui.end_row();
    });
}

/// One picture at `scale`, with its caption.
fn picture(ui: &mut Ui, raster: &mut Raster, name: &str, caption: String, max_w: f32) {
    let scale = fit(raster.width as f32, raster.height as f32, max_w, theme::IMAGE_MAX_HEIGHT);
    let size = egui::vec2(raster.width as f32 * scale, raster.height as f32 * scale);
    ui.label(RichText::new(caption).size(theme::sz(ui).body).color(theme::pal(ui).text_secondary));
    let alt = format!("{name}: {} × {} image", raster.width, raster.height);
    if let Some(texture) = raster.texture(ui.ctx(), name) {
        ui.add(egui::Image::new(egui::load::SizedTexture::new(texture.id(), size)).alt_text(alt));
    }
}

fn two_up(ui: &mut Ui, compare: &mut Compare, width: f32) {
    let each = ((width - theme::SPACE_3 * 3.0) / 2.0).max(theme::SMALL_IMAGE);
    ui.horizontal_top(|ui| {
        for (name, version, absent) in [
            ("Before", &mut compare.base, "Before: no file; this change adds it."),
            ("After", &mut compare.head, "After: no file; this change deletes it."),
        ] {
            ui.vertical(|ui| {
                ui.set_max_width(each);
                match version {
                    Version::Shown(raster) => {
                        let caption = format!("{name}  ·  {}  ·  {}", size_text(raster.bytes), dims_text(raster));
                        picture(ui, raster, name, caption, each);
                    }
                    other => missing(ui, other, absent),
                }
            });
        }
    });
}

fn overlay(ui: &mut Ui, compare: &mut Compare, state: &mut PairUi, view: FileView, width: f32) {
    let (Version::Shown(base), Version::Shown(head)) = (&mut compare.base, &mut compare.head) else { return };
    let swipe = view == FileView::ImageSwipe;
    let (slider, label) = if swipe { (&mut state.swipe, "Swipe position") } else { (&mut state.onion, "After opacity") };
    rail(ui, |ui| ui.add(egui::Slider::new(slider, 0.0..=1.0).text(label).show_value(false)));
    let at = *slider;
    let word = if swipe {
        format!("Before on the left of the line, After on the right: line at {:.0}%", at * 100.0)
    } else {
        format!("After over Before at {:.0}% opacity", at * 100.0)
    };
    note(ui, word);
    let (w, h) = ((base.width.max(head.width)) as f32, (base.height.max(head.height)) as f32);
    let scale = fit(w, h, width - theme::SPACE_3, theme::IMAGE_MAX_HEIGHT);
    let (response, painter) = ui.allocate_painter(egui::vec2(w * scale, h * scale), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, label));
    let place = |r: &Raster| Rect::from_min_size(response.rect.min, egui::vec2(r.width as f32 * scale, r.height as f32 * scale));
    let (base_rect, head_rect) = (place(base), place(head));
    let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    if let Some(texture) = base.texture(ui.ctx(), "Before") {
        painter.image(texture.id(), base_rect, uv, Color32::WHITE);
    }
    let (clip, tint) = if swipe {
        let mut clip = response.rect;
        clip.min.x += response.rect.width() * at;
        (clip, Color32::WHITE)
    } else {
        (response.rect, Color32::WHITE.gamma_multiply(at))
    };
    if let Some(texture) = head.texture(ui.ctx(), "After") {
        painter.with_clip_rect(clip).image(texture.id(), head_rect, uv, tint);
    }
    if swipe {
        let x = response.rect.min.x + response.rect.width() * at;
        painter.line_segment([egui::pos2(x, response.rect.min.y), egui::pos2(x, response.rect.max.y)], Stroke::new(2.0, theme::pal(ui).ember_text));
    }
}

/// Takes in a finished Difference picture, and starts one for the slider's value when the picture on screen is for another.
fn refresh_difference(ctx: &Context, mags: &Arc<Magnitudes>, state: &mut PairUi, settled: bool, pool: &Pool) {
    if let Some((ignore, mut slot)) = state.pending.take() {
        slot.poll();
        match slot {
            Slot::Ready((image, percent)) => {
                let texture = ctx.load_texture("Difference", image, TextureOptions::NEAREST);
                state.shown = Some(Shown { ignore, percent, texture });
            }
            Slot::Failed(error) => state.failed = Some((ignore, error)),
            loading => state.pending = Some((ignore, loading)),
        }
    }
    let wanted = state.ignore;
    let have = |known: Option<f32>| known.is_some_and(|k| (k - wanted).abs() < f32::EPSILON);
    let covered =
        have(state.shown.as_ref().map(|s| s.ignore)) || have(state.pending.as_ref().map(|p| p.0)) || have(state.failed.as_ref().map(|f| f.0));
    if settled && !covered {
        let mags = Arc::clone(mags);
        let slot = pool.start(ctx, true, move || Ok(difference_picture(&mags, wanted)));
        state.pending = Some((wanted, slot));
    }
}

/// egui paints a slider's rail with the idle widget fill, which is nearly the pane colour; the control colour keeps the rail 3:1.
fn rail<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        let control = theme::pal(ui).control;
        ui.visuals_mut().widgets.inactive.bg_fill = control;
        add(ui)
    })
    .inner
}

fn difference_ui(ui: &mut Ui, mags: &Arc<Magnitudes>, state: &mut PairUi, width: f32, pool: &Pool) {
    let slider = rail(ui, |ui| ui.add(egui::Slider::new(&mut state.ignore, 0.0..=25.0).text("Ignore changes under").suffix(" %").step_by(1.0)));
    refresh_difference(ui.ctx(), mags, state, !slider.dragged(), pool);
    if let Some((_, error)) = &state.failed {
        crate::live_label(ui, format!("Could not make the Difference: {error}"), theme::pal(ui).rose_text);
    }
    if state.pending.is_some() {
        note(ui, if state.shown.is_some() { "Updating the Difference…" } else { "Making the Difference…" });
    }
    let Some(shown) = &state.shown else { return };
    ui.label(
        RichText::new(format!("Difference · before − after · {:.0} % of pixels changed", shown.percent))
            .size(theme::sz(ui).body)
            .color(theme::pal(ui).graphite),
    );
    note(ui, "Black means identical. Bright pixels changed; a side that is smaller than the other counts as changed.");
    let scale = fit(mags.width as f32, mags.height as f32, width - theme::SPACE_3, theme::IMAGE_MAX_HEIGHT);
    let size = egui::vec2(mags.width as f32 * scale, mags.height as f32 * scale);
    let alt = format!("Difference of before and after: {:.0} % of pixels changed", shown.percent);
    ui.add(egui::Image::new(egui::load::SizedTexture::new(shown.texture.id(), size)).alt_text(alt));
}

// ---- SVG ----

fn svg(ui: &mut Ui, base: &mut Version<Vector>, head: &mut Version<Vector>, width: f32) {
    no_pins(ui, "an SVG");
    let each = ((width - theme::SPACE_3 * 3.0) / 2.0).max(theme::SMALL_IMAGE);
    ui.horizontal_top(|ui| {
        for (name, version, absent) in
            [("Before", &mut *base, "Before: no file; this change adds it."), ("After", &mut *head, "After: no file; this change deletes it.")]
        {
            ui.vertical(|ui| {
                ui.set_max_width(each);
                match version {
                    Version::Shown(vector) => svg_side(ui, name, vector),
                    Version::Absent => note(ui, absent),
                    Version::Failed(error) => crate::live_label(ui, format!("Could not draw {name}: {error}"), theme::pal(ui).rose_text),
                }
            });
        }
    });
    ui.add_space(theme::SPACE_2);
    structure(ui, base, head);
}

fn svg_side(ui: &mut Ui, name: &str, vector: &mut Vector) {
    let caption =
        format!("{name}  ·  {}  ·  {} × {} · shown at {:.0} px", size_text(vector.raster.bytes), vector.width, vector.height, theme::SVG_SHOWN);
    ui.label(RichText::new(caption).size(theme::sz(ui).body).color(theme::pal(ui).text_secondary));
    let (w, h) = (vector.raster.width as f32, vector.raster.height as f32);
    let scale = theme::SVG_SHOWN / w.max(h);
    if let Some(texture) = vector.raster.texture(ui.ctx(), name) {
        let alt = format!("{name}: SVG drawn at {:.0} px", theme::SVG_SHOWN);
        ui.add(egui::Image::new(egui::load::SizedTexture::new(texture.id(), egui::vec2(w * scale, h * scale))).alt_text(alt));
    }
    for note_text in &vector.notes {
        note(ui, note_text.clone());
    }
}

/// How many of each kind of node the parsed tree holds, before and after. Attributes and text are not compared.
fn structure(ui: &mut Ui, base: &Version<Vector>, head: &Version<Vector>) {
    ui.label(RichText::new("Node counts (parsed tree)").size(theme::sz(ui).body).color(theme::pal(ui).graphite));
    note(ui, format!("Attribute and text changes are not compared; press {} for the source diff.", keymap::label(ui, "review.toggle_source")));
    let empty = BTreeMap::new();
    let (before, after) = (counts_of(base, &empty), counts_of(head, &empty));
    let kinds: BTreeSet<&'static str> = before.keys().chain(after.keys()).copied().collect();
    if kinds.is_empty() {
        note(ui, "Neither version draws anything.");
        return;
    }
    egui::Grid::new("svg-structure").num_columns(4).spacing([theme::SPACE_3, 2.0]).show(ui, |ui| {
        for kind in kinds {
            let (a, b) = (before.get(&kind).copied().unwrap_or(0), after.get(&kind).copied().unwrap_or(0));
            let (ink, size) = (theme::pal(ui).graphite, theme::sz(ui).body);
            let text = |s: String| RichText::new(s).size(size).color(ink);
            ui.label(text(kind.to_string()));
            ui.label(text(a.to_string()));
            ui.label(text(format!("→ {b}")));
            ui.label(text(if a == b { "same count".into() } else { "count changed".into() }));
            ui.end_row();
        }
    });
}

fn counts_of<'a>(version: &'a Version<Vector>, empty: &'a BTreeMap<&'static str, usize>) -> &'a BTreeMap<&'static str, usize> {
    match version {
        Version::Shown(vector) => &vector.counts,
        _ => empty,
    }
}

// ---- notebooks ----

fn notebook_ui(ui: &mut Ui, notebook: &Notebook, path: &str, shown: &mut HashSet<String>) {
    no_pins(ui, "a notebook cell");
    let unchanged = notebook.cells.iter().filter(|c| c.change == CellChange::Unchanged).count();
    let all = shown.contains(path);
    if unchanged > 0 {
        ui.horizontal(|ui| {
            note(ui, format!("{unchanged} unchanged {}", if unchanged == 1 { "cell" } else { "cells" }));
            let label = if all { "Hide unchanged" } else { "Show" };
            if ui.button(label).clicked() && !shown.remove(path) {
                shown.insert(path.to_string());
            }
        });
    }
    for cell in &notebook.cells {
        if cell.change != CellChange::Unchanged || all {
            cell_ui(ui, cell);
        }
    }
    for cell in &notebook.removed {
        let first = cap_chars(cell.source.lines().next().unwrap_or_default(), theme::MAX_LINE_CHARS).0;
        note(ui, format!("Removed {} cell: {first}", cell_kind(cell.kind)));
    }
}

fn cell_kind(kind: CellKind) -> &'static str {
    match kind {
        CellKind::Markdown => "markdown",
        CellKind::Code => "code",
        CellKind::Raw => "raw",
    }
}

fn change_word(change: CellChange) -> &'static str {
    match change {
        CellChange::Unchanged => "unchanged",
        CellChange::Modified => "modified",
        CellChange::Added => "added",
        CellChange::OutputChanged => "output changed",
        CellChange::MediaChanged => "output changed (image not drawn)",
    }
}

fn cell_ui(ui: &mut Ui, shown: &ShownCell) {
    let cell = &shown.cell;
    let count = cell.count.map_or(" ".to_string(), |n| n.to_string());
    let word = change_word(shown.change);
    let head = if cell.kind == CellKind::Code { format!("In [{count}] · code · {word}") } else { format!("{} · {word}", cell_kind(cell.kind)) };
    let fill = if shown.change == CellChange::Unchanged { theme::pal(ui).raised } else { theme::pal(ui).moss_bg };
    egui::Frame::new().fill(fill).stroke((1.0, theme::pal(ui).border)).inner_margin(theme::SPACE_2).show(ui, |ui| {
        ui.label(RichText::new(head).size(theme::sz(ui).body).color(theme::pal(ui).text_secondary));
        match &shown.markdown {
            Some(job) => {
                ui.add(egui::Label::new(theme::restyle(ui, job)).wrap());
            }
            None => {
                ui.add(
                    egui::Label::new(RichText::new(&shown.source.text).monospace().size(theme::sz(ui).code).color(theme::pal(ui).graphite)).extend(),
                );
            }
        }
        more_note(ui, shown.source.left);
        for output in &cell.outputs {
            ui.label(RichText::new("Output").size(theme::sz(ui).body).color(theme::pal(ui).text_tertiary));
            ui.add(egui::Label::new(RichText::new(output).monospace().size(theme::sz(ui).code).color(theme::pal(ui).graphite)).extend());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1×1 PNG, made for this test.
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn grey(w: usize, h: usize, v: u8) -> ColorImage {
        ColorImage::from_gray([w, h], &vec![v; w * h])
    }

    fn ready(a: &ColorImage, b: &ColorImage) -> Arc<Magnitudes> {
        match difference(a, b) {
            Mags::Ready(mags) => mags,
            _ => panic!("the canvas fits"),
        }
    }

    #[test]
    fn a_png_decodes_and_garbage_is_an_error_not_a_blank() {
        let bytes = craft::flow::base64_decode(PNG).expect("base64");
        let image = decode(&bytes).expect("a valid PNG decodes");
        assert_eq!(image.size, [1, 1]);
        assert!(decode(b"not an image").is_err());
    }

    #[test]
    fn identical_images_differ_nowhere_and_one_changed_pixel_shows_alone() {
        let a = grey(4, 4, 10);
        assert_eq!(difference_picture(&ready(&a, &a), 0.0).1, 0.0);
        let mut b = grey(4, 4, 10);
        b.pixels[5] = Color32::from_gray(200);
        let mags = ready(&a, &b);
        assert_eq!(mags.values.iter().filter(|v| **v > 0).count(), 1);
        assert!(difference_picture(&mags, 0.0).1 > 0.0);
    }

    #[test]
    fn a_small_change_vanishes_under_the_ignore_threshold_and_a_bigger_canvas_counts_as_changed() {
        let (a, b) = (grey(4, 4, 100), grey(4, 4, 105));
        let mags = ready(&a, &b);
        assert!(difference_picture(&mags, 0.0).1 > 0.0);
        assert_eq!(difference_picture(&mags, 10.0).1, 0.0, "5/255 is under 10 %");
        let wider = ready(&a, &grey(8, 4, 100));
        assert_eq!(wider.width, 8);
        assert!(difference_picture(&wider, 25.0).1 >= 50.0, "the added half is all changed");
    }

    #[test]
    fn two_images_that_each_fit_can_still_need_a_canvas_over_the_pixel_limit() {
        // 8000 × 100 and 100 × 8000 are each 0.8 million pixels; the canvas holding both is 8000 × 8000.
        assert!(canvas_fits(8000, 100) && canvas_fits(100, 8000));
        assert!(!canvas_fits(8000, 8000));
        assert!(canvas_fits(4000, 4000), "exactly the limit still fits");
    }

    #[test]
    fn pictures_fit_their_room_and_only_small_ones_grow() {
        assert!(fit(2000.0, 1000.0, 500.0, 800.0) <= 0.25 + f32::EPSILON);
        assert!(fit(100.0, 4000.0, 900.0, 400.0) < 0.11);
        assert_eq!(fit(4000.0, 4000.0, 8000.0, 8000.0), 1.0);
        assert!(fit(24.0, 24.0, 900.0, 400.0) > 1.0);
    }

    #[test]
    fn markdown_runs_carry_bold_code_and_the_words_new_to_the_paragraph() {
        let runs = runs("Retries **6 times** with `backoff`", "Retries 3 times");
        assert!(runs.iter().any(|r| r.bold && r.text.contains("times")));
        assert!(runs.iter().any(|r| r.code && r.text == "backoff"));
        let new: Vec<&str> = runs.iter().filter(|r| r.new).map(|r| r.text.trim()).collect();
        assert!(new.contains(&"6") && new.contains(&"with") && !new.contains(&"Retries"), "{new:?}");
    }

    #[test]
    fn an_svg_is_drawn_counted_and_its_unsupported_parts_named() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12" viewBox="0 0 24 12"><script>1</script><g><path d="M0 0h10v10z"/></g><rect width="4" height="4"/></svg>"#;
        let vector = render_svg(svg).expect("a valid SVG draws");
        assert_eq!((vector.width, vector.height), (24.0, 12.0));
        assert!(vector.counts.get("shapes").copied().unwrap_or(0) >= 2, "{:?}", vector.counts);
        assert!(vector.notes.iter().any(|n| n.contains("script")), "{:?}", vector.notes);
        assert!(render_svg(b"<svg").is_err());
    }

    #[test]
    fn a_deleted_markdown_file_offers_source_only_and_a_deleted_image_keeps_its_modes() {
        let file = |path: &str, status| FileDiff {
            path: path.into(),
            old_path: None,
            status,
            hunks: Vec::new(),
            additions: 0,
            deletions: 0,
            binary: false,
            generated: false,
            hash: 0,
        };
        assert_eq!(offered(&file("a.md", FileStatus::Deleted)), [FileView::Source]);
        assert!(offered(&file("a.png", FileStatus::Deleted)).len() > 1);
        assert!(offered(&file("a.md", FileStatus::Modified)).len() > 1);
    }

    #[test]
    fn an_unclosed_fence_keeps_its_last_line_and_a_closed_one_drops_only_the_fence() {
        let lines = |text: &[&str]| text.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        let yaml = lines(&["```yaml", "a: 1", "b: 2"]);
        let (language, inner) = code_parts(&yaml);
        assert_eq!((language.as_deref(), inner), (Some("yaml"), vec!["a: 1", "b: 2"]));
        let closed = lines(&["```", "a", "```"]);
        assert_eq!(code_parts(&closed).1, ["a"]);
        let nested = lines(&["~~~~", "x", "~~~", "y", "~~~~"]);
        assert_eq!(code_parts(&nested).1, ["x", "~~~", "y"], "a shorter fence line is code");
        assert!(code_parts(&lines(&["```"])).1.is_empty());
    }

    #[test]
    fn long_text_is_cut_to_its_limit_and_says_how_much_was_left() {
        let text: Vec<String> = (0..100).map(|n| n.to_string()).collect();
        let cut = clip(&text.join("\n"), 60);
        assert_eq!((cut.text.lines().count(), cut.left.lines, cut.left.chars), (60, 40, 0));
        let short = clip("a\nb\n", 60);
        assert_eq!((short.text.as_str(), short.left.lines, short.left.chars), ("a\nb", 0, 0));
    }

    #[test]
    fn the_budget_drops_the_farthest_unprotected_files_first_and_stops_once_it_fits() {
        let held = [
            Held { distance: 0, pixels: 10, protected: true },
            Held { distance: 5, pixels: 10, protected: false },
            Held { distance: 2, pixels: 10, protected: false },
            Held { distance: 9, pixels: 10, protected: true },
            Held { distance: 7, pixels: 0, protected: false },
        ];
        assert_eq!(to_evict(&held, 40), Vec::<usize>::new(), "within the budget nothing goes");
        assert_eq!(to_evict(&held, 30), [1], "the farthest unprotected one");
        assert_eq!(to_evict(&held, 20), [1, 2]);
        assert_eq!(to_evict(&held, 0), [1, 2], "protected files stay even over the budget");
    }

    fn plain_file(path: &str) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: Vec::new(),
            additions: 0,
            deletions: 0,
            binary: false,
            generated: false,
            hash: 0,
        }
    }

    #[test]
    fn one_megabyte_lines_are_cut_in_every_kind_of_block_and_say_so() {
        let (heading, paragraph, code, quote) = ("h".repeat(1 << 20), "p".repeat(1 << 20), "c".repeat(1 << 20), "q".repeat(1 << 20));
        let text = format!("# {heading}\n\n{paragraph}\n\n```\n{code}\n```\n\n> {quote}\n\n| a |\n|---|\n| {} |\n", "t".repeat(1 << 20));
        let doc = prepare(view::md_items(&text, &plain_file("big.md")));
        let mut seen = 0;
        for entry in &doc.entries {
            let Entry::Block(block) = entry else { continue };
            let (laid_out, left) = match &block.body {
                Body::Text { jobs, left } => (jobs.iter().map(|j| j.text.chars().count()).sum::<usize>(), *left),
                Body::Mono(clipped) | Body::Code { text: clipped, .. } => (clipped.text.chars().count(), clipped.left),
                Body::Rule => continue,
            };
            assert!(laid_out <= theme::MAX_BLOCK_CHARS + 16, "{:?} laid out {laid_out} characters", block.view.block.kind);
            assert!(left.chars >= (1 << 20) - theme::MAX_LINE_CHARS - 16, "{:?} says it left {} out", block.view.block.kind, left.chars);
            seen += 1;
        }
        assert_eq!(seen, 5, "heading, paragraph, code, quote and table");
        let cell = shown_cell(
            view::parse_notebook(&format!(r#"{{"nbformat":4,"cells":[{{"cell_type":"code","source":"{}"}}]}}"#, "x".repeat(1 << 20)))
                .expect("parses")
                .cells
                .remove(0),
            CellChange::Added,
        );
        assert!(cell.source.text.chars().count() <= theme::MAX_LINE_CHARS && cell.source.left.chars > 0);
    }

    #[test]
    fn a_queued_urgent_job_runs_before_the_jobs_already_waiting() {
        let (queue, log) =
            (Queue { jobs: Mutex::new(VecDeque::new()), ready: Condvar::new(), closed: AtomicBool::new(false) }, Arc::new(Mutex::new(Vec::new())));
        for (name, urgent) in [("a", false), ("b", false), ("c", true)] {
            let log = Arc::clone(&log);
            queue.push(Box::new(move || log.lock().expect("log").push(name)), urgent);
        }
        queue.closed.store(true, Ordering::Relaxed);
        while let Some(job) = queue.next() {
            job();
        }
        assert_eq!(*log.lock().expect("log"), ["c", "a", "b"]);
    }

    fn settle<T>(slot: &mut Slot<T>) {
        for _ in 0..300 {
            slot.poll();
            if !matches!(slot, Slot::Loading { .. }) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn a_job_that_panics_fails_its_slot_and_the_pool_keeps_every_worker() {
        let (ctx, pool) = (Context::default(), Pool::new());
        // More panics than workers: if a panic killed its worker, the job after them would never run.
        let mut bad: Vec<Slot<u8>> = (0..WORKERS * 2).map(|_| pool.start(&ctx, false, || panic!("a decoder bug"))).collect();
        let mut good = pool.start(&ctx, false, || Ok(7u8));
        bad.iter_mut().for_each(settle);
        settle(&mut good);
        assert!(bad.iter().all(|slot| matches!(slot, Slot::Failed(error) if error.contains("decoder"))));
        assert!(matches!(good, Slot::Ready(7)));
    }

    #[test]
    fn a_queued_read_nobody_looks_at_any_more_is_cancelled_and_the_ones_in_view_stay() {
        let mut views = Views::new(BTreeMap::new());
        let files = [plain_file("cur.md"), plain_file("near.md"), plain_file("gone.md")];
        let mut flags = HashMap::new();
        let mut senders = Vec::new();
        for file in &files {
            let (tx, rx) = mpsc::channel::<Result<Loaded, String>>();
            senders.push(tx);
            let cancelled = Arc::new(AtomicBool::new(false));
            flags.insert(file.path.clone(), Arc::clone(&cancelled));
            views.loads.insert(file.path.clone(), Slot::Loading { rx, _cancel: Cancel(cancelled), started: Arc::new(AtomicBool::new(false)) });
        }
        views.wanted.insert("near.md".into());
        views.tidy(&files, 0);
        assert!(views.loads.contains_key("cur.md") && views.loads.contains_key("near.md"));
        assert!(!views.loads.contains_key("gone.md"), "dropped from the queue");
        let flag = |path: &str| flags[path].load(Ordering::Relaxed);
        assert!(flag("gone.md") && !flag("cur.md") && !flag("near.md"), "dropping the slot cancels its job");
    }

    #[test]
    fn j_and_k_leave_a_rendered_file_at_its_last_changed_block_and_when_it_has_none() {
        let patch = "diff --git a/d.md b/d.md\n--- a/d.md\n+++ b/d.md\n@@ -1,3 +1,5 @@\n # T\n \n+New paragraph.\n+\n Intro.\n";
        let file = pr_craft_core::diff::parse_unified(patch).remove(0);
        let mut views = Views::new(BTreeMap::new());
        let changed = prepare(view::md_items("# T\n\nNew paragraph.\n\nIntro.\n", &file));
        let untouched = prepare(view::md_items("# T\n\nIntro.\n", &plain_file("u.md")));
        views.loads.insert("d.md".into(), Slot::Ready(Loaded::Markdown(changed)));
        views.loads.insert("u.md".into(), Slot::Ready(Loaded::Markdown(untouched)));
        assert!(views.step_block("d.md", true), "the first changed block");
        assert!(!views.step_block("d.md", true), "no more changed blocks: the caller moves to the next file");
        assert!(!views.step_block("d.md", false), "nor backwards from the only one");
        assert!(!views.step_block("u.md", true) && !views.step_block("u.md", false), "no changed block at all");
    }

    fn frame(ctx: &Context, mut draw: impl FnMut(&mut Ui)) {
        ctx.run_ui(egui::RawInput::default(), &mut draw).drop_without_applying_deltas();
    }

    #[test]
    fn every_image_mode_uploads_the_pictures_it_draws_and_the_difference_arrives_from_a_worker() {
        let (ctx, pool) = (Context::default(), Pool::new());
        let (a, b) = (grey(8, 6, 10), grey(8, 6, 90));
        let mags = difference(&a, &b);
        let mut compare = Compare { base: Version::Shown(Raster::new(10, a)), head: Version::Shown(Raster::new(12, b)), mags };
        let mut state = PairUi::default();
        for view in [FileView::ImageTwoUp, FileView::ImageSwipe, FileView::ImageOnion, FileView::ImageDifference] {
            frame(&ctx, |ui| images(ui, &mut compare, &mut state, view, 600.0, &pool));
        }
        let uploaded = |v: &Version<Raster>| matches!(v, Version::Shown(r) if r.texture.is_some());
        assert!(uploaded(&compare.base) && uploaded(&compare.head), "both versions reach the GPU");
        for _ in 0..200 {
            if state.shown.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            frame(&ctx, |ui| images(ui, &mut compare, &mut state, FileView::ImageDifference, 600.0, &pool));
        }
        assert!(state.shown.is_some(), "the Difference picture was made off the UI thread");
    }

    #[test]
    fn a_difference_over_the_canvas_limit_shows_two_up_and_says_why() {
        let ctx = Context::default();
        let (a, b) = (grey(4, 4, 1), grey(4, 4, 1));
        let mut compare =
            Compare { base: Version::Shown(Raster::new(1, a)), head: Version::Shown(Raster::new(1, b)), mags: Mags::TooBig(8000, 8000) };
        let (mut state, pool) = (PairUi::default(), Pool::new());
        frame(&ctx, |ui| images(ui, &mut compare, &mut state, FileView::ImageDifference, 600.0, &pool));
        let uploaded = |v: &Version<Raster>| matches!(v, Version::Shown(r) if r.texture.is_some());
        assert!(uploaded(&compare.base) && uploaded(&compare.head), "2-up drew both");
        assert!(state.shown.is_none() && state.pending.is_none(), "no Difference was started");
    }

    #[test]
    fn every_markdown_block_gets_its_slot_for_a_composer_and_drafts() {
        let patch = "diff --git a/d.md b/d.md\n--- a/d.md\n+++ b/d.md\n@@ -1,3 +1,3 @@\n # T\n \n-Old.\n+New.\n";
        let file = pr_craft_core::diff::parse_unified(patch).remove(0);
        let doc = prepare(view::md_items("# T\n\nNew.\n", &file));
        let blocks = doc.entries.iter().filter(|e| matches!(e, Entry::Block(_))).count();
        let (ctx, mut slots) = (Context::default(), Vec::new());
        frame(&ctx, |ui| {
            markdown(ui, &doc, None, false, &mut |_, item, block| slots.push((item, block.anchor)));
        });
        assert_eq!(slots.len(), blocks);
        assert!(
            slots.iter().any(|(_, anchor)| *anchor == Some(LineRef { old: None, new: Some(3) })),
            "the changed block posts on its added line: {slots:?}"
        );
    }
}

#[cfg(test)]
mod omitted_tests {
    use super::*;

    #[test]
    fn a_file_with_thousands_of_blocks_lays_out_a_bounded_number_and_ends_with_the_count_left_out() {
        let blocks = view::MD_HEAD_BLOCKS * 3;
        let file = FileDiff {
            path: "huge.md".into(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: Vec::new(),
            additions: 0,
            deletions: 0,
            binary: false,
            generated: false,
            hash: 0,
        };
        let doc = prepare(view::md_items(&"a\n\n".repeat(blocks), &file));
        let laid_out = doc.entries.iter().filter(|e| matches!(e, Entry::Block(_))).count();
        assert_eq!(laid_out, view::MD_HEAD_BLOCKS, "nothing changed, so only the head is kept");
        let Some(Entry::Omitted(left_out, 0)) = doc.entries.last() else { panic!("the list ends with the count left out") };
        assert_eq!(laid_out + left_out, blocks);
        let text = omitted_text(*left_out, 0, "⇧S");
        assert!(text.contains(&format!("{left_out} more blocks")) && text.contains("⇧S"), "{text}");
        assert!(omitted_text(0, 4, "⇧S").contains("4 more removed paragraphs"));
    }
}
