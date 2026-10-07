//! Staging review: scan a staging object directory, rank its frames, and
//! batch-push/delete them.
//!
//! The unit of work is always **one staging object directory** — the whole
//! staging area is never processed.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::traverse::{filter_label, parse_filter};
use astro_inventory::fits_preview::{self, FitError, RenderOptions};

/// Extensions treated as reviewable frames.
pub const IMAGE_EXTS: &[&str] = &["fits", "fit"];

/// Fallback used when a header carries no IMAGETYP.
const DEFAULT_IMAGETYPE: &str = "Light";

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
/// deliberately **included** — they are reviewable and pushable like any other.
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
/// decide whether a telescope directory is itself reviewable, so it does not
/// double-count the object directories nested inside it.
fn count_direct_images(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_image(p))
        .count()
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

/// Scan a staging object directory. The per-file FITS work runs in parallel:
/// measured on the real corpus, a serial pass costs ~76 ms/file (56 s for the
/// 740-file `Pier/Jacoby1`) while the parallel pass costs ~13 ms/file cold.
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

// --- Apply ------------------------------------------------------------------

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushItem {
    pub src: String,
    /// Destination **directory**, validated against the CCD root.
    pub dst: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ApplyReport {
    pub pushed: usize,
    pub deleted: usize,
    pub errors: Vec<String>,
}

/// Move `src` onto `dst_dir/<basename>`, refusing to clobber.
///
/// Push is a **move**: staging is a Syncthing transfer area and it only drains
/// if the source goes away. `rename` is used first; a cross-filesystem pair
/// (staging on one mount, CCD on another) falls back to copy + fsync + delete.
fn move_into(src: &Path, dst_dir: &Path) -> Result<(), String> {
    let leaf = src
        .file_name()
        .ok_or_else(|| "source has no file name".to_string())?;
    let target = dst_dir.join(leaf);
    if target.exists() {
        return Err(format!("refusing to overwrite existing {}", target.display()));
    }
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

/// Push frames to their chosen destinations and delete the rest of the marked
/// set.
///
/// Deletes are **permanent** — an accepted risk of this workflow. Destinations
/// must already exist: new objects are created through the existing
/// "Add a new object" form, never here.
pub fn apply(
    staging_dir: &Path,
    ccd_root: &Path,
    pushes: &[PushItem],
    deletes: &[String],
) -> ApplyReport {
    let mut rep = ApplyReport::default();

    for p in pushes {
        let src = match resolve_within(staging_dir, &p.src) {
            Ok(v) => v,
            Err(e) => {
                rep.errors.push(format!("{}: {e}", p.src));
                continue;
            }
        };
        if !src.is_file() {
            // Re-applying after a partial failure must skip already-moved files
            // rather than report a fresh error for each.
            rep.errors.push(format!("{}: not a file (already moved?)", p.src));
            continue;
        }
        let dst_dir = match resolve_within(ccd_root, &p.dst) {
            Ok(v) if v.is_dir() => v,
            Ok(_) => {
                rep.errors.push(format!("{}: destination is not a directory: {}", p.src, p.dst));
                continue;
            }
            Err(e) => {
                rep.errors.push(format!(
                    "{}: destination {} does not exist ({e}) — create it with \"Add a new object\" on the index page",
                    p.src, p.dst
                ));
                continue;
            }
        };
        match move_into(&src, &dst_dir) {
            Ok(()) => rep.pushed += 1,
            Err(e) => rep.errors.push(format!("push {}: {e}", p.src)),
        }
    }

    for raw in deletes {
        let full = match resolve_within(staging_dir, raw) {
            Ok(v) => v,
            Err(e) => {
                rep.errors.push(format!("{raw}: {e}"));
                continue;
            }
        };
        if !full.is_file() {
            rep.errors.push(format!("{raw}: not a file (already deleted?)"));
            continue;
        }
        match fs::remove_file(&full) {
            Ok(()) => rep.deleted += 1,
            Err(e) => rep.errors.push(format!("delete {raw}: {e}")),
        }
    }

    rep
}

// --- Directory browsing -----------------------------------------------------

#[derive(Debug, Serialize)]
pub struct StagingDir {
    pub path: String,
    pub name: String,
    pub telescope: String,
    pub frames: usize,
    pub bytes: u64,
}

/// Two-level listing of the staging area: `<staging>/<telescope>/<object>`.
/// Directories holding images directly under a telescope are listed too.
pub fn list_staging_dirs(staging: &Path) -> Vec<StagingDir> {
    let mut out = Vec::new();
    let Ok(tels) = fs::read_dir(staging) else { return out };
    for tel in tels.flatten() {
        let tp = tel.path();
        if !tp.is_dir() {
            continue;
        }
        let tel_name = tel.file_name().to_string_lossy().to_string();
        // `.stfolder`, `.git`, `.syncthing.*`, `.config` — none are telescopes.
        if tel_name.starts_with('.') {
            continue;
        }

        let own = count_direct_images(&tp);
        if own > 0 {
            out.push(StagingDir {
                name: tel_name.clone(),
                telescope: tel_name.clone(),
                path: tp.to_string_lossy().to_string(),
                frames: own,
                bytes: 0,
            });
        }

        let Ok(objs) = fs::read_dir(&tp) else { continue };
        for obj in objs.flatten() {
            let op = obj.path();
            if !op.is_dir() {
                continue;
            }
            if op.file_name().map_or(true, |n| n.to_string_lossy().starts_with('.')) {
                continue;
            }
            let n = list_images(&op).len();
            if n == 0 {
                continue;
            }
            out.push(StagingDir {
                name: obj.file_name().to_string_lossy().to_string(),
                telescope: tel_name.clone(),
                path: op.to_string_lossy().to_string(),
                frames: n,
                bytes: 0,
            });
        }
    }
    out.sort_by(|a, b| a.telescope.cmp(&b.telescope).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Immediate sub-directories of the CCD root, offered as push destinations.
/// Basenames of `CCD/<telescope>/<object>` are matched against the staging
/// object name to suggest a default.
pub fn list_destinations(root: &Path) -> Vec<Destination> {
    let Ok(entries) = fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<Destination> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .flat_map(|tel| {
            let tel_name = tel.file_name().unwrap_or_default().to_string_lossy().to_string();
            let Ok(objs) = fs::read_dir(&tel) else { return Vec::new() };
            objs.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .map(|obj| {
                    let name = obj.file_name().unwrap_or_default().to_string_lossy().to_string();
                    Destination {
                        label: format!("{tel_name}/{name}"),
                        name,
                        path: obj.to_string_lossy().to_string(),
                    }
                })
                .collect()
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct Destination {
    /// `telescope/object`, shown in the dropdown.
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

    fn tmpdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("astro_staging_test_{}_{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn fnv1a_is_stable_and_sensitive() {
        assert_eq!(fnv1a(b"abc"), fnv1a(b"abc"));
        assert_ne!(fnv1a(b"abc"), fnv1a(b"abd"));
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
    fn push_moves_the_file_and_refuses_to_clobber() {
        let stage = tmpdir("stage");
        let root = tmpdir("ccd");
        let dest = root.join("81GT/M42");
        fs::create_dir_all(&dest).unwrap();
        let f = stage.join("a.fits");
        fs::write(&f, b"payload").unwrap();

        let rep = apply(
            &stage,
            &root,
            &[PushItem { src: f.to_string_lossy().into_owned(), dst: dest.to_string_lossy().into_owned() }],
            &[],
        );
        assert_eq!(rep.pushed, 1, "{:?}", rep.errors);
        assert_eq!(fs::read(dest.join("a.fits")).unwrap(), b"payload");
        assert!(!f.exists(), "push is a MOVE: the staging source must be gone");

        // Re-applying the same request must not create a duplicate or clobber.
        let rep2 = apply(
            &stage,
            &root,
            &[PushItem { src: f.to_string_lossy().into_owned(), dst: dest.to_string_lossy().into_owned() }],
            &[],
        );
        assert_eq!(rep2.pushed, 0);
        assert_eq!(fs::read(dest.join("a.fits")).unwrap(), b"payload", "must not overwrite");
    }

    #[test]
    fn push_into_an_existing_name_is_refused() {
        let stage = tmpdir("stage_c");
        let root = tmpdir("ccd_c");
        let dest = root.join("8RC/M57");
        fs::create_dir_all(&dest).unwrap();
        fs::write(stage.join("dup.fits"), b"new").unwrap();
        fs::write(dest.join("dup.fits"), b"old").unwrap();
        let rep = apply(
            &stage,
            &root,
            &[PushItem { src: stage.join("dup.fits").to_string_lossy().into_owned(), dst: dest.to_string_lossy().into_owned() }],
            &[],
        );
        assert_eq!(rep.pushed, 0);
        assert!(rep.errors.iter().any(|e| e.contains("overwrite")), "{:?}", rep.errors);
        assert_eq!(fs::read(dest.join("dup.fits")).unwrap(), b"old");
        assert!(stage.join("dup.fits").exists(), "failed push must leave the source alone");
    }

    #[test]
    fn delete_is_permanent() {
        let stage = tmpdir("stage_d");
        let root = tmpdir("ccd_d");
        fs::create_dir_all(root.join("81GT/x")).unwrap();
        let f = stage.join("gone.fits");
        fs::write(&f, b"x").unwrap();
        let rep = apply(&stage, &root, &[], &[f.to_string_lossy().into_owned()]);
        assert_eq!(rep.deleted, 1);
        assert!(!f.exists());
    }

    #[test]
    fn apply_never_creates_a_missing_destination() {
        let stage = tmpdir("stage2");
        let root = tmpdir("ccd2");
        fs::create_dir_all(&root).unwrap();
        fs::write(stage.join("a.fits"), b"x").unwrap();
        let missing = root.join("81GT/NewObject");
        let rep = apply(
            &stage,
            &root,
            &[PushItem { src: stage.join("a.fits").to_string_lossy().into_owned(), dst: missing.to_string_lossy().into_owned() }],
            &[],
        );
        assert_eq!(rep.pushed, 0);
        assert!(!missing.exists(), "apply must not auto-create destinations");
        assert!(stage.join("a.fits").exists(), "source must survive a rejected push");
        assert!(
            rep.errors.iter().any(|e| e.contains("Add a new object")),
            "error should point at the add-object form: {:?}",
            rep.errors
        );
    }

    #[test]
    fn apply_rejects_paths_outside_the_staging_dir() {
        let stage = tmpdir("stage3");
        let root = tmpdir("ccd3");
        fs::create_dir_all(root.join("81GT/x")).unwrap();
        let outside = stage.join("../outside.fits");
        fs::write(&outside, b"x").unwrap();
        let rep = apply(
            &stage,
            &root,
            &[PushItem { src: outside.to_string_lossy().into_owned(), dst: root.join("81GT/x").to_string_lossy().into_owned() }],
            &[],
        );
        assert_eq!(rep.pushed, 0);
        assert!(rep.errors.iter().any(|e| e.contains("escapes")), "{:?}", rep.errors);
        assert!(outside.exists());
        fs::remove_file(&outside).ok();
    }

    #[test]
    fn push_destination_is_validated_against_the_ccd_root() {
        let stage = tmpdir("stage4");
        let root = tmpdir("ccd4");
        fs::create_dir_all(root.join("81GT/x")).unwrap();
        fs::write(stage.join("a.fits"), b"x").unwrap();
        let escape = root.join("../escape");
        fs::create_dir_all(&escape).unwrap();
        let rep = apply(
            &stage,
            &root,
            &[PushItem { src: stage.join("a.fits").to_string_lossy().into_owned(), dst: escape.to_string_lossy().into_owned() }],
            &[],
        );
        assert_eq!(rep.pushed, 0, "destination outside the CCD root must be refused");
        assert!(!escape.join("a.fits").exists());
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
        let dirs = list_staging_dirs(&root);
        assert_eq!(dirs.len(), 1, "only Pier/Real must survive: {dirs:?}");
        assert_eq!(dirs[0].telescope, "Pier");
        assert_eq!(dirs[0].name, "Real");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn telescope_rows_do_not_double_count_object_dirs() {
        let root = tmpdir("twolevel");
        fs::create_dir_all(root.join("Pier/Obj1")).unwrap();
        fs::create_dir_all(root.join("Pier/Obj2")).unwrap();
        fs::create_dir_all(root.join("Empty")).unwrap();
        fs::write(root.join("Pier/Obj1/a.fits"), b"x").unwrap();
        fs::write(root.join("Pier/Obj2/b.fits"), b"yy").unwrap();
        let dirs = list_staging_dirs(&root);
        assert_eq!(dirs.len(), 2, "only object dirs, no telescope roll-up: {dirs:?}");
        assert!(dirs.iter().all(|d| d.frames == 1), "{dirs:?}");
        assert!(dirs.iter().all(|d| d.telescope == "Pier"));
        fs::remove_dir_all(&root).ok();
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
        while hdr.len() % 2880 != 0 {
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
        while bytes.len() % 2880 != 0 {
            bytes.extend_from_slice(&[0u8; 8]);
        }
        fs::write(path, &bytes).unwrap();
    }
}
