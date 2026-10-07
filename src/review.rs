//! Generic directory review: scan a directory, rank its frames, and batch
//! move / copy / symlink / delete the marked set.
//!
//! The unit of work is always **one directory** — the whole review root is
//! never processed. The screen is generic: any directory inside an allowed
//! review root can be opened, not only a staging object dir.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::traverse::{filter_label, parse_filter, parse_timestamp};
use astro_inventory::fits_preview::{self, FitError, RenderOptions};

/// Extensions treated as reviewable frames. Non-FITS files are not listed:
/// there is no decoder for them and the screen is FITS-driven by design.
pub const IMAGE_EXTS: &[&str] = &["fits", "fit"];

/// Fallback used when a header carries no IMAGETYP.
const DEFAULT_IMAGETYPE: &str = "Light";

/// Depth cap for directory walks. The CCD tree is `root/tel/obj` and staging
/// is `staging/tel/obj`; a cap keeps a walk over 420 object dirs cheap and
/// stops a symlinked tree from being descended forever.
const MAX_WALK_DEPTH: usize = 4;

// --- Hashing ----------------------------------------------------------------

/// FNV-1a, used for cache keys. Not security-relevant: keys only address a
/// private on-disk preview cache.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn mtime_secs(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// --- Manifest ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    /// Absolute path. The client only ever echoes this back.
    pub path: String,
    pub name: String,
    /// Path relative to the reviewed directory, for display.
    pub rel: String,
    pub size: u64,
    pub mtime: u64,
    pub imagetype: String,
    /// Human-readable filter, e.g. "Ha".
    pub filter: String,
    pub median: f32,
    pub sigma: f32,
    /// sky/σ — the cheap quality metric. Higher is better.
    pub quality: f32,
    pub width: u32,
    pub height: u32,
    pub exptime: Option<f64>,
    /// Key of the rendered preview in the on-disk cache.
    pub key: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub imagetype: String,
    pub filter: String,
    pub frames: Vec<Frame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub dir: String,
    /// Change-detection signature over the directory's file set.
    pub signature: u64,
    pub generated: u64,
    pub scanned: usize,
    pub groups: Vec<Group>,
    pub errors: Vec<String>,
}

/// Preview cache key: path + mtime + size. Re-rendering is forced by any edit.
/// Bump whenever the preview changes shape or the stretch/anchor algorithm
/// changes. The key otherwise covers only path+mtime+size, so a better anchor
/// would never reach a browser that already has the old JPEG on disk.
pub const RENDER_VERSION: u64 = 5;

pub fn preview_key(path: &Path, meta: &fs::Metadata) -> u64 {
    let mut buf = path.to_string_lossy().into_owned().into_bytes();
    buf.extend_from_slice(&mtime_secs(meta).to_le_bytes());
    buf.extend_from_slice(&meta.len().to_le_bytes());
    buf.extend_from_slice(&RENDER_VERSION.to_le_bytes());
    fnv1a(&buf)
}

// --- Scanning ---------------------------------------------------------------

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Collect image files under `dir`, recursively. Calibration directories are
/// deliberately **included** — they are reviewable and actionable like any other.
pub fn list_images(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if is_image(&p) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Count image files sitting **directly** in `dir` (no recursion). Used to
/// decide whether a directory is itself reviewable, so it does not double-count
/// the object directories nested inside it.
fn count_direct_images(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_image(p))
        .count()
}

/// Total bytes of the image files sitting **directly** in `dir`.
fn direct_image_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_image(p))
        .filter_map(|p| fs::metadata(&p).ok())
        .map(|m| m.len())
        .sum()
}

/// Newest image **directly** in `dir`. The timestamp comes from the filename, the
/// same convention as the inventory page's `Latest image` column; a name with no
/// stamp falls back to the file's mtime, so a directory whose files are named
/// without a timestamp still sorts by recency.
fn direct_latest_image(dir: &Path) -> Option<i64> {
    let Ok(entries) = fs::read_dir(dir) else { return None };
    let mut best: Option<i64> = None;
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() || !is_image(&p) {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let ts = parse_timestamp(&name).map(|d| d.timestamp()).or_else(|| {
            fs::metadata(&p)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        });
        best = ts.map_or(best, |t| best.map_or(Some(t), |b| Some(t.max(b))));
    }
    best
}

/// Signature of a directory's file set. Cheap: one stat per file, no reads.
/// Changes when files are added, removed, or modified.
pub fn dir_signature(dir: &Path, files: &[PathBuf]) -> u64 {
    let mut h = fnv1a(dir.to_string_lossy().as_bytes());
    for f in files {
        let Ok(meta) = fs::metadata(f) else { continue };
        let mut buf = f.to_string_lossy().into_owned().into_bytes();
        buf.extend_from_slice(&mtime_secs(&meta).to_le_bytes());
        buf.extend_from_slice(&meta.len().to_le_bytes());
        h = h.wrapping_mul(0x100000001b3) ^ fnv1a(&buf);
    }
    h
}

/// Scan one frame: header + strided stats pass.
fn scan_frame(p: &Path, dir: &Path) -> Result<Frame, String> {
    let meta = fs::metadata(p).map_err(|e| format!("{p:?}: {e}"))?;
    let h = fits_preview::read_header(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let s = fits_preview::stats_pass(p, &h).map_err(|e| format!("{}: {e}", p.display()))?;
    let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
    let rel = p.strip_prefix(dir).unwrap_or(p).to_string_lossy().to_string();
    let imagetype = h
        .imagetype
        .clone()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_IMAGETYPE.to_string());
    let filter = h
        .filter
        .clone()
        .or_else(|| parse_filter(&name))
        .map(|f| filter_label(&f).to_string())
        .unwrap_or_else(|| "—".to_string());
    Ok(Frame {
        key: preview_key(p, &meta),
        path: p.to_string_lossy().to_string(),
        name,
        rel,
        size: meta.len(),
        mtime: mtime_secs(&meta),
        imagetype,
        filter,
        median: s.median,
        sigma: s.sigma,
        quality: s.sky_over_sigma(),
        width: h.width as u32,
        height: h.height as u32,
        exptime: h.exptime,
    })
}

/// Scan one directory. The per-file FITS work runs in parallel: measured on
/// the real corpus, a serial pass costs ~76 ms/file (56 s for the 740-file
/// `Pier/Jacoby1`) while the parallel pass costs ~13 ms/file cold.
pub fn scan(dir: &Path) -> Manifest {
    let files = list_images(dir);
    let signature = dir_signature(dir, &files);

    let results: Vec<Result<Frame, String>> =
        files.par_iter().map(|p| scan_frame(p, dir)).collect();

    let mut errors: Vec<String> = Vec::new();
    let mut frames: Vec<Frame> = Vec::with_capacity(results.len());
    for r in results {
        match r {
            Ok(f) => frames.push(f),
            Err(e) => errors.push(e),
        }
    }

    let scanned = frames.len();

    let mut buckets: BTreeMap<(String, String), Vec<Frame>> = BTreeMap::new();
    for f in frames {
        buckets
            .entry((f.imagetype.clone(), f.filter.clone()))
            .or_default()
            .push(f);
    }

    let mut groups: Vec<Group> = buckets
        .into_iter()
        .map(|((imagetype, filter), mut frames)| {
            // Best first by default; the client can re-sort by median/sigma.
            frames.sort_by(|a, b| {
                b.quality
                    .partial_cmp(&a.quality)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            Group { imagetype, filter, frames }
        })
        .collect();

    // Lights first, then everything else alphabetically.
    groups.sort_by(|a, b| {
        let rank = |t: &str| if t.eq_ignore_ascii_case(DEFAULT_IMAGETYPE) { 0 } else { 1 };
        rank(&a.imagetype).cmp(&rank(&b.imagetype))
            .then_with(|| a.imagetype.cmp(&b.imagetype))
            .then_with(|| a.filter.cmp(&b.filter))
    });

    errors.sort();
    errors.truncate(50);

    Manifest {
        dir: dir.to_string_lossy().to_string(),
        signature,
        generated: now_secs(),
        scanned,
        groups,
        errors,
    }
}

// --- Preview cache ----------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PreviewCache {
    root: PathBuf,
}

impl PreviewCache {
    pub fn new(root: PathBuf) -> Self {
        // The sweep writes JPEGs straight into `previews/`; make sure it exists.
        // A failure is not fatal here — a render that cannot write is recorded as
        // a failed key and retried on the next pass.
        let _ = fs::create_dir_all(root.join("previews"));
        Self { root }
    }

    pub fn jpeg_path(&self, key: u64) -> PathBuf {
        self.root.join("previews").join(format!("{key:016x}.jpg"))
    }

    /// Return a cached JPEG, rendering it if absent.
    pub fn get_or_render(&self, path: &Path, key: u64) -> Result<Vec<u8>, FitError> {
        let jp = self.jpeg_path(key);
        if let Ok(bytes) = fs::read(&jp) {
            return Ok(bytes);
        }
        fits_preview::render_preview(path, &jp, &RenderOptions::default())?;
        fs::read(&jp).map_err(FitError::Io)
    }

    pub fn drop_keys(&self, keys: &[u64]) {
        for k in keys {
            let _ = fs::remove_file(self.jpeg_path(*k));
        }
    }
}

// --- Preview sweep ----------------------------------------------------------

/// What a sweep pass found for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewState {
    /// The cache already holds a JPEG for this path + mtime + size.
    Cached,
    /// No cached JPEG; the file is stat'able and can be rendered.
    Missing,
    /// The file cannot be stat'ed (deleted mid-sweep, permission, dangling link).
    Unreadable,
}

/// Classify one file for the sweep: its cache key and whether a preview exists.
pub fn preview_state(cache: &PreviewCache, path: &Path) -> (u64, PreviewState) {
    let Ok(meta) = fs::metadata(path) else {
        return (0, PreviewState::Unreadable);
    };
    let key = preview_key(path, &meta);
    let state = if cache.jpeg_path(key).exists() {
        PreviewState::Cached
    } else {
        PreviewState::Missing
    };
    (key, state)
}

/// One pass over a staging tree.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub scanned: usize,
    pub rendered: usize,
    pub existing: usize,
    /// Files whose key is in the known-failure set: not retried every pass.
    pub skipped_failed: usize,
    /// Files that failed **this** pass (path, error).
    pub failed: Vec<(String, String)>,
}

// --- Path guards ------------------------------------------------------------

/// Reject any path that escapes `base` after resolution. Every client-supplied
/// path goes through this before touching the filesystem. Canonicalizing the
/// parent before re-attaching the leaf also catches symlink escapes and
/// `..` smuggling.
pub fn resolve_within(base: &Path, raw: &str) -> Result<PathBuf, String> {
    let base_real = fs::canonicalize(base).map_err(|e| format!("base dir: {e}"))?;
    let candidate = Path::new(raw);
    let (parent, leaf) = match candidate.parent() {
        Some(p) if !p.as_os_str().is_empty() => (p.to_path_buf(), candidate.file_name()),
        _ => (PathBuf::new(), candidate.file_name()),
    };
    let parent_real = fs::canonicalize(&parent)
        .map_err(|_| format!("cannot resolve directory of {raw}"))?;
    let leaf = leaf.ok_or_else(|| format!("not a file path: {raw}"))?;
    let full = parent_real.join(leaf);
    if !full.starts_with(&base_real) {
        return Err(format!("path escapes {}: {raw}", base_real.display()));
    }
    Ok(full)
}

/// Resolve a path that may live under **any** of the allowed roots. A path
/// inside one root is accepted; a path inside none of them is rejected.
pub fn resolve_within_any(roots: &[PathBuf], raw: &str) -> Result<PathBuf, String> {
    let mut last = None;
    for r in roots {
        match resolve_within(r, raw) {
            Ok(p) => return Ok(p),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| format!("no review roots configured: {raw}")))
}

// --- Apply ------------------------------------------------------------------

/// What to do with the marked set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Move,
    Copy,
    Symlink,
    Delete,
}

impl Op {
    pub fn needs_destination(self) -> bool {
        !matches!(self, Op::Delete)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ApplyReport {
    pub moved: usize,
    pub copied: usize,
    pub linked: usize,
    pub deleted: usize,
    pub errors: Vec<String>,
}

impl ApplyReport {
    pub fn touched(&self) -> usize {
        self.moved + self.copied + self.linked + self.deleted
    }
}

/// `dst/<basename of src>`, refusing to clobber anything already there —
/// including a symlink, which `Path::exists` reports as absent when broken.
fn target_for(src: &Path, dst_dir: &Path) -> Result<PathBuf, String> {
    let leaf = src
        .file_name()
        .ok_or_else(|| "source has no file name".to_string())?;
    let target = dst_dir.join(leaf);
    if fs::symlink_metadata(&target).is_ok() {
        return Err(format!("refusing to overwrite existing {}", target.display()));
    }
    Ok(target)
}

/// Move `src` onto `dst_dir/<basename>`, refusing to clobber.
///
/// Move is the staging case: a Syncthing transfer area only drains if the
/// source goes away. `rename` is used first; a cross-filesystem pair falls
/// back to copy + fsync + delete.
fn move_into(src: &Path, dst_dir: &Path) -> Result<(), String> {
    let target = target_for(src, dst_dir)?;
    match fs::rename(src, &target) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Cross-device, or the destination mount disallows rename.
            fs::copy(src, &target).map_err(|e| format!("copy: {e}"))?;
            File::open(&target)
                .and_then(|f| f.sync_all())
                .map_err(|e| format!("fsync: {e}"))?;
            fs::remove_file(src).map_err(|e| format!("remove source: {e}"))?;
            Ok(())
        }
    }
}

/// Copy `src` onto `dst_dir/<basename>`, leaving the source in place.
fn copy_into(src: &Path, dst_dir: &Path) -> Result<(), String> {
    let target = target_for(src, dst_dir)?;
    fs::copy(src, &target).map_err(|e| format!("copy: {e}"))?;
    File::open(&target)
        .and_then(|f| f.sync_all())
        .map_err(|e| format!("fsync: {e}"))?;
    Ok(())
}

/// Symlink `dst_dir/<basename>` to `src`, leaving the source in place.
///
/// The link target is the **canonical absolute** source path: a relative link
/// would break the moment the source moves, which is exactly the operation
/// this screen performs most often.
fn link_into(src: &Path, dst_dir: &Path) -> Result<(), String> {
    let target = target_for(src, dst_dir)?;
    let src_abs = fs::canonicalize(src).map_err(|e| format!("canonicalize source: {e}"))?;
    std::os::unix::fs::symlink(&src_abs, &target).map_err(|e| format!("symlink: {e}"))?;
    Ok(())
}

/// Act on the marked set.
///
/// Deletes are **permanent** — an accepted risk of this workflow. Destinations
/// must already exist: new objects are created through the explicit
/// "＋ new destination" action, never here.
pub fn apply(
    src_dir: &Path,
    roots: &[PathBuf],
    op: Op,
    dst_raw: Option<&str>,
    files: &[String],
) -> ApplyReport {
    let mut rep = ApplyReport::default();

    let dst_dir: Option<PathBuf> = if op.needs_destination() {
        let raw = dst_raw.unwrap_or("");
        match resolve_within_any(roots, raw) {
            Ok(p) if p.is_dir() => Some(p),
            Ok(_) => {
                rep.errors.push(format!(
                    "destination {} does not exist — create it with \"＋ new destination\" on the review page",
                    raw
                ));
                return rep;
            }
            Err(e) => {
                rep.errors.push(format!(
                    "destination {} does not exist ({e}) — create it with \"＋ new destination\" on the review page",
                    raw
                ));
                return rep;
            }
        }
    } else {
        None
    };

    for raw in files {
        let src = match resolve_within(src_dir, raw) {
            Ok(v) => v,
            Err(e) => {
                rep.errors.push(format!("{raw}: {e}"));
                continue;
            }
        };
        if !src.is_file() {
            // Re-applying after a partial failure must skip already-processed
            // files rather than report a fresh error for each.
            rep.errors.push(format!("{raw}: not a file (already processed?)"));
            continue;
        }
        let r = match op {
            Op::Delete => fs::remove_file(&src)
                .map(|()| rep.deleted += 1)
                .map_err(|e| e.to_string()),
            _ => {
                // Delete needs no destination; the other three always have one
                // by the time this loop runs.
                let dst = dst_dir.as_ref().expect("op needs a destination");
                match op {
                    Op::Move => move_into(&src, dst).map(|()| rep.moved += 1),
                    Op::Copy => copy_into(&src, dst).map(|()| rep.copied += 1),
                    Op::Symlink => link_into(&src, dst).map(|()| rep.linked += 1),
                    Op::Delete => unreachable!(),
                }
            }
        };
        if let Err(e) = r {
            rep.errors.push(format!("{} {raw}: {e}", op_label(op)));
        }
    }

    rep
}

pub fn op_label(op: Op) -> &'static str {
    match op {
        Op::Move => "move",
        Op::Copy => "copy",
        Op::Symlink => "symlink",
        Op::Delete => "delete",
    }
}

// --- Directory browsing -----------------------------------------------------

/// A directory that holds reviewable frames, offered by the generic browser.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewDir {
    /// Absolute path.
    pub path: String,
    /// `root/rel` label, so two roots never look identical.
    pub label: String,
    pub frames: usize,
    pub bytes: u64,
    /// Newest image in the directory, unix seconds. Filename timestamp, falling
    /// back to mtime. `None` when nothing is stampable.
    pub latest: Option<i64>,
}

/// Directories under `roots` that hold frames **directly**. Nested object dirs
/// are listed; a directory whose frames all live in children is not, so the
/// browser never double-counts.
pub fn list_review_dirs(roots: &[PathBuf], cap: usize) -> Vec<ReviewDir> {
    let mut out = Vec::new();
    for root in roots {
        let root_real = match fs::canonicalize(root) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let root_name = root_real
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut stack = vec![(root_real.clone(), 0usize)];
        let mut visited = 0usize;
        let mut per_root = Vec::new();
        while let Some((d, depth)) = stack.pop() {
            // Bound the walk: a root with thousands of dirs should not be
            // descended exhaustively on every page load.
            if visited > cap * 40 {
                break;
            }
            visited += 1;
            let rel = d.strip_prefix(&root_real).unwrap_or(Path::new(""));
            let label = if rel.as_os_str().is_empty() {
                root_name.clone()
            } else {
                format!("{root_name}/{}", rel.to_string_lossy())
            };
            let n = count_direct_images(&d);
            if n > 0 {
                per_root.push(ReviewDir {
                    path: d.to_string_lossy().to_string(),
                    label,
                    frames: n,
                    bytes: direct_image_bytes(&d),
                    latest: direct_latest_image(&d),
                });
            }
            if depth >= MAX_WALK_DEPTH {
                continue;
            }
            let Ok(entries) = fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if !p.is_dir() {
                    continue;
                }
                // `.stfolder`, `.git`, `.syncthing.*`, `.config` — never offered.
                if e.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                stack.push((p, depth + 1));
            }
        }
        // Cap per root: a root with hundreds of directories must not crowd the
        // other roots out of the list entirely.
        per_root.truncate(cap);
        out.append(&mut per_root);
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Every directory under `roots`, offered as an action destination. Basenames
/// are matched against the reviewed directory's basename to suggest a default.
pub fn list_destinations(roots: &[PathBuf], cap: usize) -> Vec<Destination> {
    let mut out = Vec::new();
    for root in roots {
        let root_real = match fs::canonicalize(root) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let root_name = root_real
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut stack = vec![(root_real.clone(), 0usize)];
        let mut visited = 0usize;
        let mut per_root = Vec::new();
        while let Some((d, depth)) = stack.pop() {
            if visited > cap * 40 {
                break;
            }
            visited += 1;
            let rel = d.strip_prefix(&root_real).unwrap_or(Path::new(""));
            let label = if rel.as_os_str().is_empty() {
                root_name.clone()
            } else {
                format!("{root_name}/{}", rel.to_string_lossy())
            };
            per_root.push(Destination {
                label,
                name: d.file_name().unwrap_or_default().to_string_lossy().to_string(),
                path: d.to_string_lossy().to_string(),
            });
            if depth >= MAX_WALK_DEPTH {
                continue;
            }
            let Ok(entries) = fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if !p.is_dir() {
                    continue;
                }
                if e.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                stack.push((p, depth + 1));
            }
        }
        // Cap per root, as above: destinations must stay reachable from every
        // root, not just the first one.
        per_root.truncate(cap);
        out.append(&mut per_root);
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct Destination {
    /// `root/telescope/object`, shown in the dropdown.
    pub label: String,
    /// Object basename, used for the fuzzy default match.
    pub name: String,
    pub path: String,
}

/// Basenames of the files already in `dir`, so the UI can flag collisions
/// before anything is written.
pub fn dest_files(dir: &Path, cap: usize) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    out.truncate(cap);
    out
}

// --- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};

    /// Builds a root list without `clone`-in-slice noise.
    fn roots(paths: &[&std::path::Path]) -> Vec<PathBuf> {
        paths.iter().map(|p| p.to_path_buf()).collect()
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("astro_review_test_{}_{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// The index's `Latest image` column: the newest filename stamp wins, and a
    /// directory whose names carry no stamp falls back to file mtime.
    #[test]
    fn review_dir_reports_the_newest_image_timestamp() {
        let root = tmpdir("latest");
        let obj = root.join("Pier/Obj");
        fs::create_dir_all(&obj).unwrap();
        fs::write(obj.join("a_20260815_054412.fits"), b"x").unwrap();
        fs::write(obj.join("b_20260901_120000.fits"), b"x").unwrap();
        fs::write(obj.join("c_20260701_010101.fits"), b"x").unwrap();

        let dirs = list_review_dirs(&[root.clone()], 400);
        assert_eq!(dirs.len(), 1);
        assert_eq!(
            dirs[0].latest,
            Some(
                Local.with_ymd_and_hms(2026, 9, 1, 12, 0, 0)
                    .unwrap()
                    .timestamp()
            ),
            "the newest filename stamp must win"
        );

        let plain = root.join("Pier/Plain");
        fs::create_dir_all(&plain).unwrap();
        fs::write(plain.join("nope.fits"), b"x").unwrap();
        let dirs = list_review_dirs(&[root.clone()], 400);
        let plain_dir = dirs.iter().find(|d| d.label.ends_with("Plain")).unwrap();
        assert!(plain_dir.latest.is_some(), "mtime fallback must produce a stamp");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fnv1a_is_stable_and_sensitive() {
        assert_eq!(fnv1a(b"abc"), fnv1a(b"abc"));
        assert_ne!(fnv1a(b"abc"), fnv1a(b"abd"));
    }

    /// The sweep classifies by cache key: a JPEG for path+mtime+size is
    /// `Cached`, an absent one is `Missing`, and a file that cannot be stat'ed
    /// is `Unreadable`.
    #[test]
    fn sweep_classifies_files_by_cache_key() {
        let staging = tmpdir("sweep");
        let cache_dir = tmpdir("sweep_cache");
        let cache = PreviewCache::new(cache_dir.clone());
        let f = staging.join("obj").join("a.fits");
        fs::create_dir_all(staging.join("obj")).unwrap();
        fs::write(&f, b"pixels").unwrap();

        let (key, st) = preview_state(&cache, &f);
        assert_eq!(st, PreviewState::Missing);
        assert_ne!(key, 0);

        fs::write(cache.jpeg_path(key), b"jpeg").unwrap();
        assert_eq!(preview_state(&cache, &f).1, PreviewState::Cached);

        // A changed size is a new key, so the old JPEG no longer counts.
        fs::write(&f, b"pixels-longer").unwrap();
        let (k2, st2) = preview_state(&cache, &f);
        assert_ne!(k2, key);
        assert_eq!(st2, PreviewState::Missing);

        assert_eq!(
            preview_state(&cache, &staging.join("gone.fits")).1,
            PreviewState::Unreadable
        );
        fs::remove_dir_all(&staging).ok();
        fs::remove_dir_all(&cache_dir).ok();
    }

    /// A corrupt file fails to render and leaves **no** JPEG behind, so its key
    /// stays `Missing` and the sweep's known-failure set is what stops it being
    /// retried every pass.
    #[test]
    fn a_corrupt_fits_fails_render_and_leaves_no_cached_jpeg() {
        let staging = tmpdir("sweep_bad");
        let cache_dir = tmpdir("sweep_bad_cache");
        let cache = PreviewCache::new(cache_dir.clone());
        let f = staging.join("bad.fits");
        fs::write(&f, b"not a fits file").unwrap();
        let (key, st) = preview_state(&cache, &f);
        assert_eq!(st, PreviewState::Missing);
        assert!(cache.get_or_render(&f, key).is_err());
        assert!(
            !cache.jpeg_path(key).exists(),
            "a failed render must leave no JPEG"
        );
        fs::remove_dir_all(&staging).ok();
        fs::remove_dir_all(&cache_dir).ok();
    }

    #[test]
    fn resolve_within_blocks_traversal() {
        let root = tmpdir("trav");
        fs::write(root.join("ok.fits"), b"x").unwrap();
        let ok = resolve_within(&root, root.join("ok.fits").to_str().unwrap());
        assert!(ok.is_ok());
        let bad = resolve_within(&root, root.join("..").join("etc").join("passwd").to_str().unwrap());
        assert!(bad.is_err(), "traversal must be rejected");
        let deep = resolve_within(
            &root,
            root.join("a").join("..").join("..").join("..").join("etc").join("passwd")
                .to_str()
                .unwrap(),
        );
        assert!(deep.is_err(), "dot-dot smuggling must be rejected");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_within_any_accepts_any_root_and_rejects_outside() {
        let a = tmpdir("any_a");
        let b = tmpdir("any_b");
        fs::write(a.join("x.fits"), b"x").unwrap();
        fs::write(b.join("y.fits"), b"y").unwrap();
        let roots = [a.clone(), b.clone()];
        assert!(resolve_within_any(&roots, a.join("x.fits").to_str().unwrap()).is_ok());
        assert!(resolve_within_any(&roots, b.join("y.fits").to_str().unwrap()).is_ok());
        let outside = a.join("../escape.fits");
        fs::write(&outside, b"z").unwrap();
        assert!(resolve_within_any(&roots, outside.to_str().unwrap()).is_err());
        fs::remove_file(&outside).ok();
        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }

    #[test]
    fn move_is_a_move_and_refuses_to_clobber() {
        let src_dir = tmpdir("mv_src");
        let root = tmpdir("mv_root");
        let dest = root.join("81GT/M42");
        fs::create_dir_all(&dest).unwrap();
        let f = src_dir.join("a.fits");
        fs::write(&f, b"payload").unwrap();

        let rep = apply(
            &src_dir,
            &roots(&[&root]),
            Op::Move,
            Some(dest.to_str().unwrap()),
            &[f.to_string_lossy().into_owned()],
        );
        assert_eq!(rep.moved, 1, "{:?}", rep.errors);
        assert_eq!(fs::read(dest.join("a.fits")).unwrap(), b"payload");
        assert!(!f.exists(), "move must remove the source");

        // Re-applying the same request must not create a duplicate or clobber.
        let rep2 = apply(
            &src_dir,
            &roots(&[&root]),
            Op::Move,
            Some(dest.to_str().unwrap()),
            &[f.to_string_lossy().into_owned()],
        );
        assert_eq!(rep2.moved, 0);
        assert_eq!(fs::read(dest.join("a.fits")).unwrap(), b"payload", "must not overwrite");
    }

    #[test]
    fn copy_leaves_the_source_and_refuses_to_clobber() {
        let src_dir = tmpdir("cp_src");
        let root = tmpdir("cp_root");
        let dest = root.join("8RC/M57");
        fs::create_dir_all(&dest).unwrap();
        fs::write(src_dir.join("new.fits"), b"new").unwrap();
        fs::write(src_dir.join("dup.fits"), b"new").unwrap();
        fs::write(dest.join("dup.fits"), b"old").unwrap();

        let rep = apply(
            &src_dir,
            &roots(&[&root]),
            Op::Copy,
            Some(dest.to_str().unwrap()),
            &[src_dir.join("new.fits").to_string_lossy().into_owned(),
              src_dir.join("dup.fits").to_string_lossy().into_owned()],
        );
        assert_eq!(rep.copied, 1, "{:?}", rep.errors);
        assert!(src_dir.join("new.fits").exists(), "copy must keep the source");
        assert_eq!(fs::read(dest.join("new.fits")).unwrap(), b"new");
        assert_eq!(fs::read(dest.join("dup.fits")).unwrap(), b"old", "must not overwrite");
        assert!(rep.errors.iter().any(|e| e.contains("overwrite")), "{:?}", rep.errors);
    }

    #[test]
    fn symlink_points_at_the_absolute_source_and_keeps_it() {
        let src_dir = tmpdir("sl_src");
        let root = tmpdir("sl_root");
        let dest = root.join("81GT/M42");
        fs::create_dir_all(&dest).unwrap();
        let f = src_dir.join("a.fits");
        fs::write(&f, b"payload").unwrap();

        let rep = apply(
            &src_dir,
            &roots(&[&root]),
            Op::Symlink,
            Some(dest.to_str().unwrap()),
            &[f.to_string_lossy().into_owned()],
        );
        assert_eq!(rep.linked, 1, "{:?}", rep.errors);
        assert!(f.exists(), "symlink must leave the source in place");

        let link = dest.join("a.fits");
        let tgt = fs::read_link(&link).unwrap();
        assert_eq!(tgt, fs::canonicalize(&f).unwrap(), "link must be absolute");
        assert_eq!(fs::read(&link).unwrap(), b"payload");

        // A link onto an existing name is refused, and the existing file is untouched.
        fs::write(dest.join("b.fits"), b"old").unwrap();
        fs::write(src_dir.join("b.fits"), b"new").unwrap();
        let rep2 = apply(
            &src_dir,
            &roots(&[&root]),
            Op::Symlink,
            Some(dest.to_str().unwrap()),
            &[src_dir.join("b.fits").to_string_lossy().into_owned()],
        );
        assert_eq!(rep2.linked, 0);
        assert_eq!(fs::read(dest.join("b.fits")).unwrap(), b"old");
    }

    #[test]
    fn delete_is_permanent_and_needs_no_destination() {
        let src_dir = tmpdir("del_src");
        let root = tmpdir("del_root");
        let f = src_dir.join("gone.fits");
        fs::write(&f, b"x").unwrap();
        let rep = apply(&src_dir, &roots(&[&root]), Op::Delete, None, &[f.to_string_lossy().into_owned()]);
        assert_eq!(rep.deleted, 1, "{:?}", rep.errors);
        assert!(!f.exists());
    }

    #[test]
    fn apply_never_creates_a_missing_destination() {
        let src_dir = tmpdir("src2");
        let root = tmpdir("root2");
        fs::create_dir_all(&root).unwrap();
        fs::write(src_dir.join("a.fits"), b"x").unwrap();
        let missing = root.join("81GT/NewObject");
        let rep = apply(
            &src_dir,
            &roots(&[&root]),
            Op::Move,
            Some(missing.to_str().unwrap()),
            &[src_dir.join("a.fits").to_string_lossy().into_owned()],
        );
        assert_eq!(rep.moved, 0);
        assert!(!missing.exists(), "apply must not auto-create destinations");
        assert!(src_dir.join("a.fits").exists(), "source must survive a rejected batch");
        assert!(
            rep.errors.iter().any(|e| e.contains("new destination")),
            "error should point at the create form: {:?}",
            rep.errors
        );
    }

    #[test]
    fn apply_rejects_paths_outside_the_reviewed_dir() {
        let src_dir = tmpdir("src3");
        let root = tmpdir("root3");
        fs::create_dir_all(root.join("81GT/x")).unwrap();
        let outside = src_dir.join("../outside.fits");
        fs::write(&outside, b"x").unwrap();
        let rep = apply(
            &src_dir,
            &roots(&[&root]),
            Op::Move,
            Some(root.join("81GT/x").to_str().unwrap()),
            &[outside.to_string_lossy().into_owned()],
        );
        assert_eq!(rep.moved, 0);
        assert!(rep.errors.iter().any(|e| e.contains("escapes")), "{:?}", rep.errors);
        assert!(outside.exists());
        fs::remove_file(&outside).ok();
    }

    #[test]
    fn destination_is_validated_against_the_allowed_roots() {
        let src_dir = tmpdir("src4");
        let root = tmpdir("root4");
        fs::create_dir_all(root.join("81GT/x")).unwrap();
        fs::write(src_dir.join("a.fits"), b"x").unwrap();
        let escape = root.join("../escape4");
        fs::create_dir_all(&escape).unwrap();
        let rep = apply(
            &src_dir,
            &roots(&[&root]),
            Op::Move,
            Some(escape.to_str().unwrap()),
            &[src_dir.join("a.fits").to_string_lossy().into_owned()],
        );
        assert_eq!(rep.moved, 0, "destination outside the allowed roots must be refused");
        assert!(!escape.join("a.fits").exists());
        fs::remove_dir_all(&escape).ok();
    }

    #[test]
    fn scan_groups_by_imagetype_and_filter_and_ranks_by_quality() {
        // Reuse the synthetic FITS builder from fits_preview via a minimal
        // hand-written file: two Lights (H, R) and one Dark.
        let dir = tmpdir("scan");
        fs::create_dir_all(&dir).unwrap();
        write_fake_fits(&dir.join("L_H_1.fits"), 310.0, 13.0);
        write_fake_fits(&dir.join("L_H_2.fits"), 310.0, 26.0); // worse quality
        write_fake_fits(&dir.join("L_R_1.fits"), 310.0, 13.0);
        write_fake_fits(&dir.join("Dark_1.fits"), 310.0, 13.0);

        let m = scan(&dir);
        assert_eq!(m.scanned, 4, "{:?}", m.errors);
        assert!(m.errors.is_empty(), "{:?}", m.errors);

        let lights: Vec<&Group> = m.groups.iter().filter(|g| g.imagetype == "Light").collect();
        assert_eq!(lights.len(), 2, "expected an H and an R Light group");
        // Lights sort before other types.
        assert_eq!(m.groups[0].imagetype, "Light");

        let gh = m.groups.iter().find(|g| g.filter == "Ha").unwrap();
        assert_eq!(gh.frames.len(), 2);
        assert!(
            gh.frames[0].quality > gh.frames[1].quality,
            "best frame must sort first: {} vs {}",
            gh.frames[0].quality,
            gh.frames[1].quality
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_includes_calibration_directories() {
        let dir = tmpdir("calib");
        fs::create_dir_all(dir.join("Darks")).unwrap();
        write_fake_fits(&dir.join("Darks/d1.fits"), 310.0, 13.0);
        let m = scan(&dir);
        assert_eq!(m.scanned, 1, "calibration frames are reviewable too");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dot_directories_are_never_offered() {
        let root = tmpdir("dotdirs");
        fs::create_dir_all(root.join(".config/ccdciel")).unwrap();
        fs::create_dir_all(root.join(".stfolder")).unwrap();
        fs::create_dir_all(root.join("Pier/.syncthing.tmp")).unwrap();
        fs::create_dir_all(root.join("Pier/Real")).unwrap();
        fs::write(root.join(".config/ccdciel/a.fits"), b"x").unwrap();
        fs::write(root.join("Pier/.syncthing.tmp/b.fits"), b"x").unwrap();
        fs::write(root.join("Pier/Real/c.fits"), b"x").unwrap();
        let dirs = list_review_dirs(&roots(&[&root]), 100);
        assert_eq!(dirs.len(), 1, "only Pier/Real must survive: {dirs:?}");
        assert_eq!(
            dirs[0].label,
            format!("{}/Pier/Real", root.file_name().unwrap().to_string_lossy()),
            "label must be root-relative"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn browser_rows_do_not_double_count_nested_dirs() {
        let root = tmpdir("twolevel");
        fs::create_dir_all(root.join("Pier/Obj1")).unwrap();
        fs::create_dir_all(root.join("Pier/Obj2")).unwrap();
        fs::create_dir_all(root.join("Empty")).unwrap();
        fs::write(root.join("Pier/Obj1/a.fits"), b"x").unwrap();
        fs::write(root.join("Pier/Obj2/b.fits"), b"yy").unwrap();
        let dirs = list_review_dirs(&roots(&[&root]), 100);
        assert_eq!(dirs.len(), 2, "only dirs holding frames: {dirs:?}");
        assert!(dirs.iter().all(|d| d.frames == 1), "{dirs:?}");
        let rn = root.file_name().unwrap().to_string_lossy().to_string();
        assert!(dirs.iter().all(|d| d.label.starts_with(&format!("{rn}/Pier/"))), "{dirs:?}");
        fs::remove_dir_all(&root).ok();
    }

    /// A cap must be per root: a CCD tree with hundreds of directories must
    /// not push the staging directories out of the list entirely.
    #[test]
    fn every_root_stays_visible_when_one_root_is_huge() {
        let big = tmpdir("bigroot");
        let small = tmpdir("smallroot");
        for i in 0..5 {
            fs::create_dir_all(big.join(format!("81GT/Obj{i}"))).unwrap();
            fs::write(big.join(format!("81GT/Obj{i}/a.fits")), b"x").unwrap();
        }
        fs::create_dir_all(small.join("Pier/Target")).unwrap();
        fs::write(small.join("Pier/Target/a.fits"), b"x").unwrap();

        let dirs = list_review_dirs(&roots(&[&big, &small]), 2);
        let labels: Vec<String> = dirs.iter().map(|d| d.label.clone()).collect();
        let bn = big.file_name().unwrap().to_string_lossy().to_string();
        let sn = small.file_name().unwrap().to_string_lossy().to_string();
        assert!(labels.iter().any(|l| l.starts_with(&format!("{bn}/"))), "{labels:?}");
        assert!(
            labels.iter().any(|l| l.starts_with(&format!("{sn}/"))),
            "second root crowded out: {labels:?}"
        );
        fs::remove_dir_all(&big).ok();
        fs::remove_dir_all(&small).ok();
    }

    #[test]
    fn destinations_cover_every_root_and_label_by_root() {
        let a = tmpdir("dest_a");
        let b = tmpdir("dest_b");
        fs::create_dir_all(a.join("81GT/M42")).unwrap();
        fs::create_dir_all(b.join("Pier/Jacoby1")).unwrap();
        let ds = list_destinations(&roots(&[&a, &b]), 100);
        let labels: Vec<&str> = ds.iter().map(|d| d.label.as_str()).collect();
        let an = a.file_name().unwrap().to_string_lossy().to_string();
        let bn = b.file_name().unwrap().to_string_lossy().to_string();
        assert!(labels.contains(&format!("{an}/81GT/M42").as_str()), "{labels:?}");
        assert!(labels.contains(&format!("{bn}/Pier/Jacoby1").as_str()), "{labels:?}");
        assert!(labels.contains(&an.as_str()), "the root itself is a destination");
        // Labels sort, so the dropdown is stable.
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted, "destinations must be sorted by label");
        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }

    #[test]
    fn signature_changes_when_files_change() {
        let dir = tmpdir("sig");
        let f = dir.join("a.fits");
        fs::write(&f, b"one").unwrap();
        let s1 = dir_signature(&dir, &list_images(&dir));
        fs::write(&f, b"two-longer").unwrap();
        let s2 = dir_signature(&dir, &list_images(&dir));
        assert_ne!(s1, s2);
        fs::remove_file(&f).unwrap();
        let s3 = dir_signature(&dir, &list_images(&dir));
        assert_ne!(s2, s3);
        fs::remove_dir_all(&dir).ok();
    }

    /// Minimal single-HDU BITPIX=16 FITS with a flat sky, enough for the header
    /// parser and the strided stats pass.
    fn write_fake_fits(path: &Path, sky: f32, sigma: f32) {
        let (w, h) = (64usize, 64usize);
        let mut hdr = String::new();
        let mut card = |k: &str, v: &str| {
            let mut s = format!("{k:<8}= {v:>20}");
            s.truncate(80);
            s.push_str(&" ".repeat(80 - s.len()));
            hdr.push_str(&s);
        };
        card("SIMPLE", "T");
        card("BITPIX", "16");
        card("NAXIS", "2");
        card("NAXIS1", &w.to_string());
        card("NAXIS2", &h.to_string());
        card("BZERO", "32768");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if let Some(f) = parse_filter(&name) {
            let mut s = format!("{:<8}= '{f}'", "FILTER");
            s.truncate(80);
            s.push_str(&" ".repeat(80 - s.len()));
            hdr.push_str(&s);
        }
        let it = if name.to_lowercase().starts_with("dark") { "Dark" } else { "Light" };
        let mut s = format!("{:<8}= '{it}'", "IMAGETYP");
        s.truncate(80);
        s.push_str(&" ".repeat(80 - s.len()));
        hdr.push_str(&s);
        hdr.push_str(&format!("{:<80}", "END"));
        while !hdr.len().is_multiple_of(2880) {
            hdr.push_str(&format!("{:<80}", ""));
        }
        let mut bytes = hdr.into_bytes();
        let mut state = 12345u64;
        for _y in 0..h {
            for _x in 0..w {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let n = (((state >> 33) as f32) / ((1u64 << 31) as f32) - 0.5) * 2.0 * sigma;
                let v = ((sky + n - 32768.0).round() as i32).clamp(-32768, 32767) as i16;
                bytes.extend_from_slice(&v.to_be_bytes());
            }
        }
        while !bytes.len().is_multiple_of(2880) {
            bytes.extend_from_slice(&[0u8; 8]);
        }
        fs::write(path, &bytes).unwrap();
    }
}
