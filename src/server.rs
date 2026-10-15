use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
use axum::{extract::Form, Router};
use chrono::Local;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use crate::plan::expand_constellation;
use crate::render::{
    col_by_key, page_size_control, render_edit_page, render_header_cells, render_index_page,
    render_pager, render_review, render_review_index, sort_records,
};
use crate::traverse::{build_row, traverse, ObjectRecord};
use crate::review::{self, Manifest, PreviewCache, PreviewState};
use astro_inventory::compendium;

/// Shared application state.
pub struct AppState {
    pub root: PathBuf,
    pub page_size_default: usize,
    pub longitude: f64,
    pub latitude: f64,
    pub compendium_dir: PathBuf,
    cache: Mutex<ScanCache>,
    /// `None` disables the staging launcher page.
    pub staging_root: Option<PathBuf>,
    /// Directories the generic review screen may open. Every client path is
    /// validated against this set before touching the filesystem.
    pub review_roots: Vec<PathBuf>,
    /// Autorepeat throttle for the review screen's arrow keys, in ms.
    pub nav_repeat_ms: u64,
    /// Staging telescope name -> inventory telescope name (config
    /// `staging_tel_map`); unmapped names fall back to name equality.
    pub tel_map: HashMap<String, String>,
    previews: Option<PreviewCache>,
    /// Parsed manifests keyed by canonical reviewed directory.
    manifests: Mutex<HashMap<String, Manifest>>,
    /// Walked destination list. Rebuilt after anything that can add a directory
    /// (apply, newdest) — a full walk of ~1,900 dirs measured 1.05 s.
    dests: Mutex<Option<Vec<review::Destination>>>,
    /// Preview keys that failed to render. A corrupt FITS is not retried every
    /// sweep pass; a changed mtime/size produces a new key and is retried.
    failed_keys: Mutex<HashSet<u64>>,
    /// In-flight apply job: (total files, per-file counter). The browser polls
    /// `/review/progress` while the apply POST is still open. Single-user tool,
    /// so one global slot is enough — a second apply simply replaces it.
    pub apply_progress: Mutex<Option<(usize, Arc<AtomicUsize>)>>,
    /// Serializes full FITS decodes: each one saturates rayon for ~250 ms.
    render_permits: Semaphore,
}

struct ScanCache {
    records: Option<Vec<ObjectRecord>>,
    generated: Option<chrono::DateTime<Local>>,
    root_mtime: f64,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root: PathBuf,
        page_size: usize,
        longitude: f64,
        latitude: f64,
        compendium_dir: PathBuf,
        staging_root: Option<PathBuf>,
        cache_dir: PathBuf,
        review_roots: Vec<PathBuf>,
        nav_repeat_ms: u64,
        tel_map: HashMap<String, String>,
    ) -> Arc<Self> {
        let previews = Some(PreviewCache::new(cache_dir));
        Arc::new(Self {
            root,
            page_size_default: page_size,
            longitude,
            latitude,
            compendium_dir,
            cache: Mutex::new(ScanCache {
                records: None,
                generated: None,
                root_mtime: 0.0,
            }),
            staging_root,
            review_roots,
            nav_repeat_ms,
            tel_map,
            previews,
            manifests: Mutex::new(HashMap::new()),
            dests: Mutex::new(None),
            failed_keys: Mutex::new(HashSet::new()),
            apply_progress: Mutex::new(None),
            render_permits: Semaphore::new(1),
        })
    }

    /// Resolve a client-supplied path, refusing anything outside the allowed
    /// review roots. Every review route goes through this first.
    fn review_path(&self, raw: &str) -> Result<std::path::PathBuf, (StatusCode, String)> {
        review::resolve_within_any(&self.review_roots, raw).map_err(|e| (StatusCode::BAD_REQUEST, e))
    }

    /// Resolve a **destination** path. Frames may be reviewed anywhere, but
    /// move/copy/symlink destinations live only inside the inventory (CCD)
    /// root — staging is a transit area, never a destination.
    fn dest_path(&self, raw: &str) -> Result<std::path::PathBuf, (StatusCode, String)> {
        review::resolve_within_any(std::slice::from_ref(&self.root), raw)
            .map_err(|e| (StatusCode::BAD_REQUEST, e))
    }

    /// Move-flow hints for a reviewed directory inside staging:
    /// (telescope names usable as destinations, telescope of the reviewed
    /// dir). Telescopes are the child dirs of the staging root — the
    /// Syncthing per-telescope folders — intersected with directories that
    /// already exist under the CCD root, so the dropdown can never offer a
    /// non-telescope directory that happens to sit at the top of CCD. The
    /// reviewed dir's telescope is its first component under staging. `None`
    /// for any directory outside staging.
    fn staging_move_hint(&self, dir: &std::path::Path) -> Option<(Vec<String>, Option<String>)> {
        let staging = self.staging_root.as_ref()?;
        let staging = std::fs::canonicalize(staging).ok()?;
        let rel = dir.strip_prefix(&staging).ok()?;
        if rel.as_os_str().is_empty() {
            return None;
        }
        let tel = rel.components().next()?.as_os_str().to_string_lossy().to_string();
        // Staging folders are named after the mount, inventory ones after the
        // instrument; the config map translates between the two. Unmapped
        // names pass through unchanged, so name-equal trees behave as before.
        let mapped = |n: &str| self.tel_map.get(n).cloned().unwrap_or_else(|| n.to_string());
        let mut tels: Vec<String> = std::fs::read_dir(&staging)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .map(|n| mapped(&n))
            .filter(|m| self.root.join(m).is_dir())
            .collect();
        tels.sort();
        tels.dedup();
        let want = mapped(&tel);
        let suggest = if tels.iter().any(|t| *t == want) {
            Some(want)
        } else {
            None
        };
        Some((tels, suggest))
    }

    fn cached_dests(&self) -> Option<Vec<review::Destination>> {
        self.dests.lock().unwrap().clone()
    }

    fn set_dests(&self, dirs: Vec<review::Destination>) {
        *self.dests.lock().unwrap() = Some(dirs);
    }

    /// A destination list is only stale once a directory can appear; a file
    /// move does not change it.
    fn invalidate_dests(&self) {
        *self.dests.lock().unwrap() = None;
    }

    /// Return a manifest for `dir`, rescanning only when the file set changed.
    fn manifest_for(&self, dir: &std::path::Path) -> Manifest {
        let key = dir.to_string_lossy().to_string();
        let sig = review::dir_signature(dir, &review::list_images(dir));
        {
            let cache = self.manifests.lock().unwrap();
            if let Some(m) = cache.get(&key) {
                if m.signature == sig {
                    return m.clone();
                }
            }
        }
        let m = review::scan(dir);
        self.manifests.lock().unwrap().insert(key, m.clone());
        m
    }

    /// Drop the manifest entries and cached JPEGs of files that were pushed or
    /// deleted, so the next view cannot show a vanished frame.
    /// Collect the preview cache keys for `processed` paths **before** anything
    /// is mutated. A key folds in `mtime + size`, and once a file has been
    /// moved its stat is gone, so this must run first.
    fn snapshot_keys(&self, dir: &str, processed: &[String]) -> Vec<u64> {
        let cache = self.manifests.lock().unwrap();
        let Some(m) = cache.get(dir) else { return Vec::new() };
        m.groups
            .iter()
            .flat_map(|g| g.frames.iter())
            .filter(|f| processed.iter().any(|p| p == &f.path))
            .map(|f| f.key)
            .collect()
    }

    /// Drop processed frames from the cached manifest so a re-open reflects the
    /// directory as it now is, without paying for a full rescan. When the
    /// directory is drained the whole entry goes away.
    fn drop_frames_from_manifest(&self, dir: &str, processed: &[String]) {
        {
            let mut cache = self.manifests.lock().unwrap();
            if let Some(m) = cache.get_mut(dir) {
                for g in m.groups.iter_mut() {
                    g.frames.retain(|f| !processed.iter().any(|p| p == &f.path));
                }
                m.groups.retain(|g| !g.frames.is_empty());
                m.scanned = m.groups.iter().map(|g| g.frames.len()).sum();
                if m.scanned == 0 {
                    cache.remove(dir);
                }
            }
        }
    }

    fn root_mtime(&self) -> f64 {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(_) => return 0.0,
        };
        let mut mtimes = Vec::new();
        for entry in entries.flatten() {
            if let Ok(meta) = std::fs::metadata(entry.path()) {
                if let Ok(modified) = meta.modified() {
                    if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                        mtimes.push(duration.as_secs_f64());
                    }
                }
            }
        }
        mtimes.iter().fold(0.0f64, |a, b| a.max(*b))
    }

    fn get_records(&self, force: bool) -> (Vec<ObjectRecord>, chrono::DateTime<Local>) {
        let mut cache = self.cache.lock().unwrap();
        let stale = cache
            .records
            .as_ref()
            .map_or(true, |_| self.root_mtime() != cache.root_mtime);

        if force || cache.records.is_none() || stale {
            let t0 = std::time::Instant::now();
            eprintln!("[profile] get_records: refresh (force={force}, stale={stale})");
            let records = traverse(&self.root, self.longitude);
            eprintln!("[profile] get_records: traverse took {:?}", t0.elapsed());
            let generated = Local::now();
            cache.records = Some(records);
            cache.generated = Some(generated);
            cache.root_mtime = self.root_mtime();
        }

        let records = cache.records.clone().unwrap_or_default();
        let generated = cache.generated.clone().unwrap_or_else(Local::now);
        (records, generated)
    }
}

/// Build the axum router.
pub fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/img/{*path}", get(image))
        .route("/add", post(add_object))
        .route("/edit/{telescope}/{name}", get(edit_plan_get).post(edit_plan_post))
        .route("/delete/{telescope}/{name}", post(delete_object))
        .route("/astrobin/{slug}", get(astrobin_lookup))
        .route("/compendium", get(compendium_page))
        .route("/compendium/data", get(compendium_data))
        .route("/compendium/thumb/{id}", get(compendium_thumb))
        .route("/staging", get(staging_index))
        .route("/review", get(review))
        .route("/review/manifest", get(review_manifest))
        .route("/review/preview", get(review_preview))
        .route("/review/destinations", get(review_destinations))
        .route("/review/subdirs", get(review_subdirs))
        .route("/review/destinfo", get(review_destinfo))
        .route("/review/newdest", post(review_newdest))
        .route("/review/apply", post(review_apply))
        .route("/review/progress", get(review_progress))
        .with_state(state)
}

// --- Query params ---

#[derive(Deserialize)]
struct IndexQuery {
    sort: Option<String>,
    dir: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
    refresh: Option<String>,
}

// --- Flash message via cookie ---

fn flash_cookie(cat: &str, msg: &str) -> String {
    format!("flash={}|{}; HttpOnly; Path=/; Max-Age=600", cat, msg)
}

// --- Routes ---

async fn index(
    State(state): State<Arc<AppState>>,
    Query(params): Query<IndexQuery>,
) -> Response {
    let force = params.refresh.as_deref() == Some("1");
    let (mut records, generated) = state.get_records(force);

    let telescopes: Vec<String> = {
        let set: std::collections::HashSet<&str> =
            records.iter().map(|r| r.telescope.as_str()).collect();
        let mut v: Vec<String> = set.into_iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    };

    // Sort
    let sort_key = params.sort.as_deref().unwrap_or("latest");
    let sort_key = if col_by_key(sort_key).map_or(true, |c| !c.sortable) {
        "latest"
    } else {
        sort_key
    };
    let default_dir = col_by_key(sort_key).map(|c| c.default_dir).unwrap_or("asc");
    let direction = params
        .dir
        .as_deref()
        .filter(|d| *d == "asc" || *d == "desc")
        .unwrap_or(default_dir);

    sort_records(&mut records, sort_key, direction);

    // Pagination
    let page_size = params
        .page_size
        .unwrap_or(state.page_size_default)
        .max(0);
    let total = records.len();

    let (page, pages, page_records): (usize, usize, Vec<ObjectRecord>) = if page_size == 0 {
        (1, 1, records)
    } else {
        let pages = total.div_ceil(page_size);
        let page = params.page.unwrap_or(1).clamp(1, pages.max(1));
        let start = (page - 1) * page_size;
        let page_records = records[start..(start + page_size).min(total)].to_vec();
        (page, pages, page_records)
    };

    // Build rows
    let root = state.root.clone();
    let rows: Vec<String> = page_records
        .iter()
        .map(|rec| {
            let root_ref = &root;
            let image_url_fn = |path: &std::path::Path| -> String {
                let rel = path.strip_prefix(root_ref).unwrap_or(path);
                let rel_str = rel.to_string_lossy();
                let encoded = percent_encode(rel_str.as_bytes(), &NON_ALPHANUMERIC).to_string();
                format!("/img/{encoded}")
            };
            let actions_url_fn = |tel: &str, name: &str| -> (String, String) {
                (
                    format!("/edit/{}/{}", tel, name),
                    format!("/delete/{}/{}", tel, name),
                )
            };
            // Every object directory is reviewable: the screen is generic, so
            // a CCD dir opens with the same marks and actions as a staging dir.
            let review_url_fn = |path: &std::path::Path| -> String {
                let s = path.to_string_lossy();
                // DIR_QUERY keeps `/` readable: a plain `dir=/a/b` is easier to
                // read, bookmark, and debug than `dir=%2Fa%2F%2Fb`.
                format!("/review?dir={}", percent_encode(s.as_bytes(), crate::render::DIR_QUERY))
            };
            build_row(rec, &image_url_fn, &actions_url_fn, &review_url_fn)
        })
        .collect();

    let telescope_options: String = telescopes
        .iter()
        .map(|t| format!("<option value=\"{}\">{}</option>", crate::plan::html_escape(t), crate::plan::html_escape(t)))
        .collect::<Vec<_>>()
        .join("\n");

    let refresh_url = format!(
        "?sort={sort_key}&dir={direction}&page=1&page_size={page_size}&refresh=1"
    );

    let header_cells = render_header_cells(sort_key, direction, page_size);

    let pager = page_size_control(page_size, sort_key, direction)
        + " "
        + &render_pager(page, pages, total, sort_key, direction, page_size);

    let generated_str = generated.format("%Y-%m-%d %H:%M:%S").to_string();

    // No flash on index (cookie-based flash would need to be read from request)
    let flashes = String::new();

    let html = render_index_page(
        &state.root.to_string_lossy(),
        &format!("{:.2}, {:.2}", state.latitude, state.longitude),
        total,
        &generated_str,
        &telescope_options,
        &flashes,
        &refresh_url,
        &header_cells,
        &rows.join("\n"),
        &pager,
    );

    Html(html).into_response()
}

async fn image(
    State(state): State<Arc<AppState>>,
    req: Request,
) -> Result<Response, (StatusCode, String)> {
    // Reconstruct the relative path from the raw request URI, since axum's
    // path parameters mangle multi-segment catch-all paths.
    let raw = req.uri().path().to_string();
    let decoded = percent_encoding::percent_decode_str(&raw)
        .decode_utf8()
        .map_err(|_| (StatusCode::NOT_FOUND, "Not found".to_string()))?
        .into_owned();
    let relpath = decoded.strip_prefix("/img/").unwrap_or(&decoded).to_string();

    let root_real = std::fs::canonicalize(&state.root)
        .map_err(|_| (StatusCode::NOT_FOUND, "Not found".to_string()))?;

    let full = std::fs::canonicalize(state.root.join(&relpath))
        .map_err(|_| (StatusCode::NOT_FOUND, "Not found".to_string()))?;

    if !full.starts_with(&root_real) {
        return Err((StatusCode::NOT_FOUND, "Not found".to_string()));
    }
    if !full.is_file() {
        return Err((StatusCode::NOT_FOUND, "Not found".to_string()));
    }

    let content = std::fs::read(&full)
        .map_err(|_| (StatusCode::NOT_FOUND, "Not found".to_string()))?;

    let content_type = if relpath.to_lowercase().ends_with(".jpg")
        || relpath.to_lowercase().ends_with(".jpeg")
    {
        "image/jpeg"
    } else if relpath.to_lowercase().ends_with(".png") {
        "image/png"
    } else if relpath.to_lowercase().ends_with(".gif") {
        "image/gif"
    } else {
        "application/octet-stream"
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(content))
        .unwrap())
}

#[derive(Deserialize)]
struct AddForm {
    telescope: Option<String>,
    object: Option<String>,
    constellation: Option<String>,
    ra: Option<String>,
    dec: Option<String>,
    rotation: Option<String>,
    sample: Option<String>,
}

/// Optional `plan.md` front matter, in the order the template emits it.
struct PlanMeta<'a> {
    constellation: &'a str,
    ra: &'a str,
    dec: &'a str,
    rotation: &'a str,
    sample: &'a str,
}

const EMPTY_META: PlanMeta<'static> = PlanMeta {
    constellation: "",
    ra: "",
    dec: "",
    rotation: "",
    sample: "",
};

/// A path component that could escape its parent or name a dot-file.
fn bad_component(s: &str) -> bool {
    s.is_empty() || s == "." || s == ".." || s.contains('/') || s.contains('\\') || s.contains('\0')
}

/// Fold a Latin accented, diacritic, or ligature character to its ASCII
/// spelling, so `Céphée` becomes `Cephee` rather than `C_ph_e`.
///
/// Characters with no ASCII spelling return `None` and fall through to the `_`
/// rule, so non-Latin scripts (Cyrillic, Greek, CJK) are unaffected here.
/// Combining marks (`e` + U+0301) map to `""`: the base letter is already
/// ASCII, so the mark is simply dropped.
fn fold_accented(ch: char) -> Option<&'static str> {
    Some(match ch {
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' => "A",
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'Æ' => "AE",
        'æ' => "ae",
        'Ç' | 'Ć' | 'Ĉ' | 'Ċ' => "C",
        'ç' | 'ć' | 'ĉ' | 'ċ' => "c",
        'È' | 'É' | 'Ê' | 'Ë' | 'Ĕ' | 'Ě' => "E",
        'è' | 'é' | 'ê' | 'ë' | 'ĕ' | 'ě' | 'ē' | 'Ė' | 'ę' => "e",
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => "G",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'Ĥ' => "H",
        'ĥ' => "h",
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ĩ' | 'Ī' | 'Ĭ' | 'İ' => "I",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'ı' => "i",
        'Ķ' => "K",
        'ķ' => "k",
        'Ľ' | 'Ĺ' | 'Ļ' | 'Ł' => "L",
        'ľ' | 'ĺ' | 'ļ' | 'ł' => "l",
        'Ñ' | 'Ņ' | 'Ň' => "N",
        'ñ' | 'ņ' | 'ň' | 'ń' => "n",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' | 'Ő' => "O",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'Ŕ' | 'Ŗ' => "R",
        'ŕ' | 'ŗ' => "r",
        'Š' | 'Ŝ' | 'Ş' => "S",
        'š' | 'ŝ' | 'ş' => "s",
        'Ť' | 'Ţ' => "T",
        'ť' | 'ţ' => "t",
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ũ' | 'Ū' | 'Ű' | 'Ų' => "U",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ű' | 'ų' => "u",
        'Ý' | 'Ŷ' => "Y",
        'ý' | 'ŷ' | 'ÿ' => "y",
        'Ź' | 'Ż' | 'Ž' => "Z",
        'ź' | 'ż' | 'ž' => "z",
        'Ð' => "D",
        'ð' => "d",
        'Đ' => "D",
        'đ' => "d",
        'Ď' => "D",
        'ď' => "d",
        'Ř' => "R",
        'ř' => "r",
        'Þ' => "TH",
        'þ' => "th",
        'ß' => "ss",
        'ẞ' => "SS",
        'Ĳ' => "IJ",
        'ĳ' => "ij",
        'Œ' => "OE",
        'œ' => "oe",
        '\u{0300}'..'\u{036f}' => "",
        _ => return None,
    })
}

/// Normalise a new object name to ASCII alphanumerics only: accented Latin
/// letters are folded to their ASCII spelling first (`Céphée` → `Cephee`), then
/// every remaining character that is not `[A-Za-z0-9]` becomes `_`, runs of `_`
/// collapse to one, and leading and trailing `_` are trimmed per segment.
///
/// `/` is kept as the nesting separator, so the review page's drill-down path
/// (`M42/deep/sub`) still nests, and `NGC 7331 (Marat)` becomes `NGC_7331_Marat`
/// — the same underscore convention as the Compendium thumbnail stems
/// (`0001.Abell_01.jpg`).
///
/// Unlike `compendium::sanitize_name`, which maps characters 1:1 for thumbnail
/// stems (`M13 (Rosette)!` → `M13__Rosette__`), this collapses runs and trims
/// edges so a typed name lands as a tidy directory; the two are deliberately
/// not shared.
///
/// Idempotent: a clean name is a no-op, so the handlers can normalise the name
/// for their flash messages and labels before `create_object` normalises again
/// as the authoritative choke point.
fn sanitize_object_name(s: &str) -> String {
    s.split('/')
        .map(|seg| {
            let mut out = String::with_capacity(seg.len());
            for ch in seg.chars() {
                if ch.is_ascii_alphanumeric() {
                    out.push(ch);
                } else if let Some(folded) = fold_accented(ch) {
                    // Every fold is ASCII alphanumerics (or empty for a
                    // combining mark), so it never introduces a `_`.
                    out.push_str(folded);
                } else if !out.ends_with('_') {
                    out.push('_');
                }
            }
            out.trim_matches('_').to_string()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Create `CCD/<telescope>/<name>`, optionally together with its `plan.md`.
///
/// `name` may contain `/` to nest deeper (`M42/sub/deep`); every segment is
/// sanitised individually and all intermediate directories are created.
///
/// The index form passes `with_plan: true` so the object is immediately
/// visible to `traverse.rs` (RA/Dec + latest-image fallback). The review
/// page's move flow passes `with_plan: false`: a destination created on the
/// way to a move is just a landing folder, and an empty `plan.md` there only
/// creates clutter the user has to edit or delete later.
///
/// Errors are human-readable because both callers surface them verbatim.
fn create_object(
    root: &std::path::Path,
    telescope: &str,
    name: &str,
    meta: &PlanMeta<'_>,
    with_plan: bool,
) -> Result<PathBuf, String> {
    if telescope.is_empty() || name.is_empty() {
        return Err("Please choose a telescope and enter an object name.".to_string());
    }
    // The index form only sanitised `name` because its telescope came from a
    // closed select; this is also reachable from JSON, so both are checked.
    if bad_component(telescope) {
        return Err(format!("Invalid telescope name: {telescope}"));
    }
    // Backslashes and NUL bytes are never part of a legitimate name, and
    // sanitising them away would hide an attempted escape, so they are refused
    // outright before any rewriting happens.
    if name.contains('\\') || name.contains('\0') {
        return Err(format!("Invalid object name: {name}"));
    }
    // Slashes are allowed so the review page's drill-down dropdowns can nest
    // at any depth; every other character is normalised to `_`, so the created
    // directory and its `plan.md` heading are alphanumerics-only.
    let name = sanitize_object_name(name);
    // `/` survives sanitising, so a segment that sanitises to nothing (`..`,
    // `//`, `!!!/x`) is still a rejected name; `.` and `..` can no longer
    // survive, so the escape checks above stay meaningful.
    if name.is_empty() || name.split('/').any(bad_component) {
        return Err(format!("Invalid object name: {name}"));
    }
    if !root.join(telescope).is_dir() {
        return Err(format!("Telescope directory not found: {telescope}"));
    }
    // Belt and braces: prove the joined path stays under the CCD root even
    // though the components are already sanitised. The check is **lexical**
    // (guard_new_dir): for a nested new name the intermediate directories do
    // not exist yet, so the path cannot be canonicalised here.
    let root_real = std::fs::canonicalize(root).map_err(|e| format!("CCD root: {e}"))?;
    let obj_dir =
        review::guard_new_dir(&[root_real.clone()], &root_real.join(telescope).join(&name).to_string_lossy())?;
    if obj_dir.exists() {
        return Err(format!(
            "Object directory already exists: {telescope}/{name}"
        ));
    }
    (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&obj_dir)?;
        let mut fm = Vec::new();
        if !meta.constellation.is_empty() {
            fm.push(format!(
                "constellation: {}",
                expand_constellation(meta.constellation)
            ));
        }
        if !meta.ra.is_empty() {
            fm.push(format!("ra: {}", meta.ra));
        }
        if !meta.dec.is_empty() {
            fm.push(format!("dec: {}", meta.dec));
        }
        if !meta.rotation.is_empty() {
            fm.push(format!("rotation: {}", meta.rotation));
        }
        if !meta.sample.is_empty() {
            fm.push(format!("link: {}", meta.sample));
        }
        let mut content = String::new();
        if !fm.is_empty() {
            content.push_str("---\n");
            content.push_str(&fm.join("\n"));
            content.push('\n');
            content.push_str("---\n\n");
        }
        content.push_str(&format!("# {name}\n\n## Plan\n"));
        if with_plan {
            std::fs::write(obj_dir.join("plan.md"), content)?;
        }
        Ok(())
    })()
    .map_err(|e| format!("Failed to create {}: {e}", obj_dir.display()))?;
    // Re-check after creation: a symlinked component along a nested path
    // could otherwise land the new directory outside the root.
    let real = std::fs::canonicalize(&obj_dir).map_err(|e| format!("resolve {}: {e}", obj_dir.display()))?;
    if !real.starts_with(&root_real) {
        return Err(format!("created path escapes the CCD root: {name}"));
    }
    Ok(obj_dir)
}

async fn add_object(
    State(state): State<Arc<AppState>>,
    Form(form): Form<AddForm>,
) -> Response {
    let telescope = form.telescope.unwrap_or_default().trim().to_string();
    // Normalise here too so the flash message reports the name that is created.
    let name = sanitize_object_name(form.object.unwrap_or_default().trim());
    let constellation = form.constellation.unwrap_or_default().trim().to_string();
    let ra = form.ra.unwrap_or_default().trim().to_string();
    let dec = form.dec.unwrap_or_default().trim().to_string();
    let rotation = form.rotation.unwrap_or_default().trim().to_string();
    let sample = form.sample.unwrap_or_default().trim().to_string();

    let meta = PlanMeta {
        constellation: &constellation,
        ra: &ra,
        dec: &dec,
        rotation: &rotation,
        sample: &sample,
    };
    match create_object(&state.root, &telescope, &name, &meta, true) {
        Ok(_) => {
            // Force cache refresh
            state.cache.lock().unwrap().records = None;
            redirect_with_flash("ok", &format!("Created {telescope}/{name}/plan.md"))
        }
        Err(e) => redirect_with_flash("error", &e),
    }
}

#[derive(Deserialize)]
struct NewDestRequest {
    telescope: String,
    name: String,
}

/// Create a new `CCD/<telescope>/<object>` destination from the review page.
///
/// `apply` deliberately still never creates directories: creating one is an
/// explicit, visible action, so a typo in a batch cannot quietly scatter files
/// into a fresh directory.
async fn review_newdest(
    State(state): State<Arc<AppState>>,
    Json(req): Json<NewDestRequest>,
) -> Result<Json<review::Destination>, (StatusCode, String)> {
    let telescope = req.telescope.trim().to_string();
    // Normalise here too so the returned label matches the directory created.
    let name = sanitize_object_name(req.name.trim());
    let st = state.clone();
    let (tel, nm) = (telescope.clone(), name.clone());
    let created = tokio::task::spawn_blocking(move || {
        create_object(&st.root, &tel, &nm, &EMPTY_META, false)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // A new object dir under the CCD root is invisible to root_mtime().
    state.cache.lock().unwrap().records = None;
    state.invalidate_dests();

    // Label exactly as `list_destinations` does (`root/telescope/object`), so
    // the JS can select the new option by path without a label mismatch.
    let root_name = state
        .root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    Ok(Json(review::Destination {
        label: format!("{root_name}/{telescope}/{name}"),
        name,
        path: created.to_string_lossy().into_owned(),
    }))
}

async fn edit_plan_get(
    State(state): State<Arc<AppState>>,
    Path((telescope, name)): Path<(String, String)>,
) -> Response {
    let (obj_dir, err) = resolve_object(&state, &telescope, &name);
    if let Some(err) = err {
        return redirect_with_flash("error", &err);
    }

    let plan_path = obj_dir.join("plan.md");
    let content = std::fs::read_to_string(&plan_path).unwrap_or_default();

    let flashes = String::new();
    let html = render_edit_page(
        &telescope,
        &name,
        &plan_path.to_string_lossy(),
        &content,
        &flashes,
    );
    Html(html).into_response()
}

#[derive(Deserialize)]
struct EditForm {
    plan: Option<String>,
}

async fn edit_plan_post(
    State(state): State<Arc<AppState>>,
    Path((telescope, name)): Path<(String, String)>,
    Form(form): Form<EditForm>,
) -> Response {
    let (obj_dir, err) = resolve_object(&state, &telescope, &name);
    if let Some(err) = err {
        return redirect_with_flash("error", &err);
    }

    let plan_path = obj_dir.join("plan.md");
    let content = form.plan.unwrap_or_default();

    if let Err(e) = std::fs::write(&plan_path, content) {
        return redirect_with_flash("error", &format!("Failed to save plan: {e}"));
    }

    // Force cache refresh
    {
        let mut cache = state.cache.lock().unwrap();
        cache.records = None;
    }

    redirect_with_flash(
        "ok",
        &format!("Saved {telescope}/{name}/plan.md"),
    )
}

#[derive(Deserialize)]
struct DeleteForm {
    confirm: Option<String>,
}

async fn delete_object(
    State(state): State<Arc<AppState>>,
    Path((telescope, name)): Path<(String, String)>,
    Form(form): Form<DeleteForm>,
) -> Response {
    if form.confirm.as_deref() != Some("yes") {
        return redirect_with_flash("error", "Confirmation required.");
    }

    let (obj_dir, err) = resolve_object(&state, &telescope, &name);
    if let Some(err) = err {
        return redirect_with_flash("error", &err);
    }

    if let Err(e) = std::fs::remove_dir_all(&obj_dir) {
        return redirect_with_flash(
            "error",
            &format!("Failed to delete {}: {e}", obj_dir.display()),
        );
    }

    // Force cache refresh
    {
        let mut cache = state.cache.lock().unwrap();
        cache.records = None;
    }

    redirect_with_flash("ok", &format!("Deleted {telescope}/{name}"))
}

async fn astrobin_lookup(
    State(_state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Response {
    let encoded_slug: String = slug
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u8)
            }
        })
        .collect();
    let url = format!(
        "https://www.astrobin.com/api/v2/images/image/?hash={encoded_slug}"
    );

    let client = reqwest::Client::new();
    let resp_result = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (astro-inventory)")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;

    let resp = match resp_result {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("AstroBin API request failed: {e}")
                }))).into_response();
        }
    };

    let data_result: Result<serde_json::Value, _> = resp.json().await;
    let data = match data_result {
        Ok(d) => d,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("AstroBin API request failed: {e}")
                }))).into_response();
        }
    };

    let results = data.get("results").and_then(|r| r.as_array());
    let results = match results {
        Some(r) if !r.is_empty() => r,
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("No AstroBin image found for slug '{slug}'")
                })),
            )
                .into_response();
        }
    };

    let img = &results[0];
    let sol = img.get("solution").unwrap_or(&serde_json::Value::Null);

    // AstroBin serialises coordinates as decimal-degree *strings* (`"ra":
    // "25.760"`), so a bare `as_f64()` finds nothing and the form fills with
    // `00h00m00s`. The `advanced*` fields are the plate-solved values and carry
    // more precision, so they win when present. No parsable value leaves the
    // field blank rather than asserting a coordinate at the equator.
    let ra_str = sol
        .get("advancedRa")
        .and_then(deg_value)
        .or_else(|| sol.get("ra").and_then(deg_value))
        .map(ra_hms);
    let dec_str = sol
        .get("advancedDec")
        .and_then(deg_value)
        .or_else(|| sol.get("dec").and_then(deg_value))
        .map(dec_dms);

    let name = img.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let constellation = img
        .get("constellation")
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let constellation = expand_constellation(constellation);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "name": name,
            "constellation": constellation,
            "ra": ra_str.unwrap_or_default(),
            "dec": dec_str.unwrap_or_default(),
        })),
    )
        .into_response()
}

/// Accept a coordinate as a JSON number **or** a numeric string.
fn deg_value(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// Decimal degrees → `HHhMMmSSs`.
fn ra_hms(ra_deg: f64) -> String {
    let total_sec = (ra_deg % 360.0) * 240.0;
    let h = (total_sec / 3600.0) as i64;
    let m = ((total_sec % 3600.0) / 60.0) as i64;
    let s = (total_sec % 60.0) as i64;
    format!("{h:02}h{m:02}m{s:02}s")
}

/// Decimal degrees → `±DD°MM′SS″`.
fn dec_dms(dec_deg: f64) -> String {
    let sign = if dec_deg >= 0.0 { "+" } else { "-" };
    let dec_abs = dec_deg.abs();
    let d = dec_abs as i64;
    let m = ((dec_abs - d as f64) * 60.0) as i64;
    let s = (((dec_abs - d as f64) * 60.0 - m as f64) * 60.0) as i64;
    format!("{sign}{d}\u{00b0}{m:02}\u{2032}{s:02}\u{2033}")
}

// --- Compendium routes (lazy: nothing loads until these are hit) ---

async fn compendium_page(State(state): State<Arc<AppState>>) -> Response {
    let available = state.compendium_dir.join("objects.json").exists();
    let observer = compendium::Observer { latitude: state.latitude, longitude: state.longitude };
    let when = chrono::Local::now().format("%Y-%m-%dT%H:%M").to_string();
    Html(compendium::render_compendium_page(available, observer, &when)).into_response()
}

async fn compendium_data(State(state): State<Arc<AppState>>) -> Response {
    match compendium::get_objects(&state.compendium_dir) {
        Ok(objs) => Json(objs.as_ref()).into_response(),
        Err(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
    }
}

async fn compendium_thumb(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, "Not found").into_response();
    let Ok(objs) = compendium::get_objects(&state.compendium_dir) else {
        return not_found();
    };
    let Some(obj) = objs.iter().find(|o| o.id == id) else {
        return not_found();
    };
    let Some(thumb) = obj.thumb.as_ref() else {
        return not_found();
    };
    // Filename comes from our own JSON, but guard anyway.
    if thumb.contains('/') || thumb.contains('\\') || thumb.contains("..") || thumb.contains('\0') {
        return not_found();
    }
    let path = state.compendium_dir.join("thumbs").join(thumb);
    let Ok(content) = std::fs::read(&path) else {
        return not_found();
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .body(Body::from(content))
        .unwrap()
}

// --- Helpers ---

fn resolve_object(
    state: &AppState,
    telescope: &str,
    name: &str,
) -> (PathBuf, Option<String>) {
    if telescope.is_empty()
        || telescope == "."
        || telescope == ".."
        || telescope.contains('/')
        || telescope.contains('\\')
        || telescope.contains('\0')
    {
        return (PathBuf::new(), Some("Invalid telescope name.".to_string()));
    }
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return (PathBuf::new(), Some("Invalid object name.".to_string()));
    }

    let tel_dir = state.root.join(telescope);
    let obj_dir = tel_dir.join(name);

    if !tel_dir.is_dir() {
        return (
            PathBuf::new(),
            Some(format!("Telescope directory not found: {telescope}")),
        );
    }
    if !obj_dir.is_dir() {
        return (
            PathBuf::new(),
            Some(format!("Object directory not found: {telescope}/{name}")),
        );
    }

    // Verify path stays under ROOT
    let root_real = std::fs::canonicalize(&state.root).unwrap_or_else(|_| state.root.clone());
    let obj_real = std::fs::canonicalize(&obj_dir).unwrap_or(obj_dir.clone());
    if !obj_real.starts_with(&root_real) {
        return (PathBuf::new(), Some("Invalid path.".to_string()));
    }

    (obj_dir, None)
}

fn redirect_with_flash(cat: &str, msg: &str) -> Response {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        flash_cookie(cat, msg).parse().unwrap(),
    );
    headers.insert(
        header::LOCATION,
        "/".parse().unwrap(),
    );
    (StatusCode::SEE_OTHER, headers).into_response()
}

// --- Review routes ---

#[derive(Deserialize)]
struct ReviewQuery {
    dir: Option<String>,
}

#[derive(Deserialize)]
struct PreviewQuery {
    path: Option<String>,
}

#[derive(Deserialize)]
struct ApplyRequest {
    dir: String,
    op: review::Op,
    /// Destination directory. Required for move/copy/symlink, ignored for delete.
    dst: Option<String>,
    files: Vec<String>,
    /// Remove the source directory once a move completes. Only an empty
    /// directory is ever removed; leftovers keep it in place.
    #[serde(default)]
    remove_src_dir: bool,
}

/// The staging launcher: directories under the staging root that hold frames.
async fn staging_index(
    State(state): State<Arc<AppState>>,
) -> Result<Html<String>, (StatusCode, String)> {
    let sr = state
        .staging_root
        .clone()
        .ok_or((StatusCode::NOT_FOUND, "staging launcher is not enabled".to_string()))?;
    let sr2 = sr.clone();
    let dirs = tokio::task::spawn_blocking(move || review::list_review_dirs(&[sr2], 400))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Html(render_review_index(&[sr], &dirs)))
}

/// The generic review screen. With `?dir=` it opens that directory; without it
/// it lists every directory under the review roots that holds frames.
async fn review(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReviewQuery>,
) -> Result<Html<String>, (StatusCode, String)> {
    let raw = q.dir.filter(|d| !d.is_empty());
    let raw = match raw {
        Some(r) => r,
        None => {
            let roots = state.review_roots.clone();
            let dirs = tokio::task::spawn_blocking(move || review::list_review_dirs(&roots, 400))
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            return Ok(Html(render_review_index(&state.review_roots, &dirs)));
        }
    };
    let dir = state.review_path(&raw)?;
    if !dir.is_dir() {
        return Err((StatusCode::NOT_FOUND, "not a directory".to_string()));
    }
    let hint = state.staging_move_hint(&dir);
    let (tel_only, suggest_tel) = match &hint {
        Some((tels, suggest)) => (Some(tels.clone()), suggest.clone()),
        None => (None, None),
    };
    Ok(Html(render_review(
        &dir.to_string_lossy(),
        state.nav_repeat_ms,
        tel_only.as_deref(),
        suggest_tel.as_deref(),
        hint.is_some(),
    )))
}

async fn review_manifest(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReviewQuery>,
) -> Result<Json<Manifest>, (StatusCode, String)> {
    let raw = q.dir.ok_or((StatusCode::BAD_REQUEST, "dir is required".to_string()))?;
    let dir = state.review_path(&raw)?;
    if !dir.is_dir() {
        return Err((StatusCode::NOT_FOUND, "not a directory".to_string()));
    }
    // A full scan is CPU- and I/O-heavy; keep it off the async worker threads.
    let st = state.clone();
    let m = tokio::task::spawn_blocking(move || st.manifest_for(&dir))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(m))
}

async fn review_preview(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PreviewQuery>,
) -> Result<Response, (StatusCode, String)> {
    let raw = q.path.ok_or((StatusCode::BAD_REQUEST, "path is required".to_string()))?;
    let path = state.review_path(&raw)?;
    if !path.is_file() {
        return Err((StatusCode::NOT_FOUND, "not a file".to_string()));
    }
    let cache = state
        .previews
        .clone()
        .ok_or((StatusCode::NOT_FOUND, "preview cache is not configured".to_string()))?;

    let meta = std::fs::metadata(&path).map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    let key = review::preview_key(&path, &meta);

    // One decode at a time: each render uses every core for a few hundred ms.
    let _permit = state
        .render_permits
        .acquire()
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "closed".to_string()))?;

    let (p, k) = (path.clone(), key);
    let bytes = tokio::task::spawn_blocking(move || cache.get_or_render(&p, k))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/jpeg")
        // Keyed on path+mtime+size, so a cached entry is immutable.
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(Body::from(bytes))
        .unwrap())
}

/// Sweep the staging tree for missing previews, `interval` apart. The first
/// pass runs immediately at startup, so the review screen finds previews
/// already rendered.
pub fn spawn_preview_sweep(state: Arc<AppState>, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let r = sweep_once(&state).await;
            println!(
                "[preview-sweep] scanned {} rendered {} cached {} skipped {} failed {}",
                r.scanned,
                r.rendered,
                r.existing,
                r.skipped_failed,
                r.failed.len()
            );
            for (p, e) in r.failed.iter().take(5) {
                println!("[preview-sweep] failed {p}: {e}");
            }
            tokio::time::sleep(interval).await;
        }
    })
}

/// One pass. The render permit is taken **per file** and released after each
/// render, so an interactive preview request interleaves with the sweep and two
/// FITS never decode at once.
async fn sweep_once(state: &Arc<AppState>) -> review::SweepReport {
    let (Some(staging), Some(cache)) = (state.staging_root.clone(), state.previews.clone()) else {
        return review::SweepReport::default();
    };
    // The walk goes to the blocking pool; each per-file stat is one syscall.
    let files = tokio::task::spawn_blocking(move || review::list_images(&staging))
        .await
        .unwrap_or_default();
    let mut report = review::SweepReport::default();
    for p in files {
        report.scanned += 1;
        let (key, st) = review::preview_state(&cache, &p);
        match st {
            PreviewState::Cached => report.existing += 1,
            PreviewState::Unreadable => {
                report.failed.push((p.to_string_lossy().into_owned(), "stat failed".to_string()));
            }
            PreviewState::Missing => {
                if state.failed_keys.lock().unwrap().contains(&key) {
                    report.skipped_failed += 1;
                    continue;
                }
                let Ok(permit) = state.render_permits.acquire().await else {
                    break;
                };
                let (p2, k, c) = (p.clone(), key, cache.clone());
                let out = tokio::task::spawn_blocking(move || c.get_or_render(&p2, k)).await;
                // Released only after the render completes, so the next waiter
                // (an interactive request) gets the permit, not the sweep.
                drop(permit);
                match out {
                    Ok(Ok(_)) => report.rendered += 1,
                    Ok(Err(e)) => {
                        state.failed_keys.lock().unwrap().insert(key);
                        report.failed.push((p.to_string_lossy().into_owned(), e.to_string()));
                    }
                    Err(_) => {
                        report.failed.push((p.to_string_lossy().into_owned(), "render task panicked".to_string()));
                    }
                }
            }
        }
    }
    report
}

async fn review_destinations(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<review::Destination>>, (StatusCode, String)> {
    if let Some(cached) = state.cached_dests() {
        return Ok(Json(cached));
    }
    // The dropdown offers children of the **inventory root** only, down to
    // telescope / object level (`root/tel`, `root/tel/obj`): the root itself
    // and deeper subdirs (`master`, `*.pxiproject`, …) are not offered, and
    // neither is staging — frames move *out of* staging, never into it. Any
    // other directory inside the root can still arrive via `/review/destinfo`.
    // A bounded read_dir walk, deliberately NOT get_records(): that path
    // takes a std Mutex across a traverse measured at ~48 s, which would
    // stall every other request.
    let roots = vec![state.root.clone()];
    let dirs = tokio::task::spawn_blocking(move || review::list_destinations(&roots, 2_000, 2))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state.set_dests(dirs.clone());
    Ok(Json(dirs))
}

/// Child **directory** names of `dir`, which must exist inside the inventory
/// root. Powers the review page's drill-down dropdowns: one call per level,
/// so the hierarchy can be walked without typing a path. Paths outside the
/// root or that do not exist are rejected.
async fn review_subdirs(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReviewQuery>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let raw = q.dir.ok_or((StatusCode::BAD_REQUEST, "dir is required".to_string()))?;
    let roots = vec![state.root.clone()];
    let subs = tokio::task::spawn_blocking(move || review::list_subdirs(&roots, &raw, 500))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(Json(subs))
}

#[derive(Serialize)]
struct DestInfo {
    path: String,
    exists: bool,
    files: usize,
    names: Vec<String>,
}

/// Facts about a **candidate** destination directory: whether it exists, how
/// many files it holds, and their names. Unlike the other review routes this
/// one also accepts a path that does not exist yet (inside a review root),
/// returning `exists: false` so the UI can offer to create it. Anything
/// outside every review root is rejected.
async fn review_destinfo(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReviewQuery>,
) -> Result<Json<DestInfo>, (StatusCode, String)> {
    let raw = q.dir.ok_or((StatusCode::BAD_REQUEST, "dir is required".to_string()))?;
    let cand = review::guard_new_dir(std::slice::from_ref(&state.root), &raw)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let exists = cand.is_dir();
    let names = if exists {
        review::dest_files(&cand, 20_000)
    } else {
        Vec::new()
    };
    let files = names.len();
    Ok(Json(DestInfo {
        path: cand.to_string_lossy().into_owned(),
        exists,
        files,
        names,
    }))
}

/// Optional tidy-up after a move or a delete: plain `remove_dir`, so a
/// directory that still holds unmarked files fails safely and the reason is
/// reported. Review roots themselves are never removable.
fn tidy_src_dir(dir: &std::path::Path, review_roots: &[PathBuf], report: &mut review::ApplyReport) {
    if review_roots.iter().any(|r| r == dir) {
        report.errors.push(format!(
            "remove source dir {}: is a review root — not removed",
            dir.display()
        ));
    } else if let Err(e) = std::fs::remove_dir(dir) {
        report.errors.push(format!(
            "remove source dir {}: {e}",
            dir.display()
        ));
    }
}

/// The in-flight apply job, for the page's progress bar: `{total, done}`
/// while an apply is running, `null` when idle.
async fn review_progress(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let guard = state.apply_progress.lock().unwrap();
    match guard.as_ref() {
        Some((total, done)) => Json(serde_json::json!({
            "total": total,
            "done": done.load(std::sync::atomic::Ordering::Relaxed),
        })),
        None => Json(serde_json::Value::Null),
    }
}

async fn review_apply(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ApplyRequest>,
) -> Result<Json<review::ApplyReport>, (StatusCode, String)> {
    let dir = state.review_path(&req.dir)?;
    if !dir.is_dir() {
        return Err((StatusCode::BAD_REQUEST, "dir is not a directory".to_string()));
    }
    if req.files.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "nothing to do".to_string()));
    }
    let op = req.op;
    let dst = if op.needs_destination() {
        let raw = req
            .dst
            .as_deref()
            .filter(|d| !d.is_empty())
            .ok_or((
                StatusCode::BAD_REQUEST,
                "a destination is required for this operation".to_string(),
            ))?;
        state.dest_path(raw)?;
        Some(raw.to_string())
    } else {
        None
    };

    let processed: Vec<String> = req.files.clone();
    let dir_key = dir.to_string_lossy().to_string();

    // Snapshot the cache keys first: after a move the source's stat is gone,
    // so the key can never be recomputed later. Load the manifest if the UI
    // has not already fetched it.
    state.manifest_for(&dir);
    let keys = state.snapshot_keys(&dir_key, &processed);

    let roots = vec![state.root.clone()];
    let files = processed.clone();
    let remove_src = req.remove_src_dir;
    let dir_for_apply = dir.clone();
    // Publish progress for the duration of the job so the page can draw a bar.
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    *state.apply_progress.lock().unwrap() = Some((processed.len(), counter.clone()));
    let counter_for_apply = counter.clone();
    let mut report = tokio::task::spawn_blocking(move || {
        review::apply(&dir_for_apply, &roots, op, dst.as_deref(), &files, Some(&counter_for_apply))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.apply_progress.lock().unwrap() = None;

    // Optional tidy-up after a move or a delete.
    if matches!(op, review::Op::Move | review::Op::Delete) && remove_src && report.touched() > 0 {
        tidy_src_dir(&dir, &state.review_roots, &mut report);
    }
    state.drop_frames_from_manifest(&dir_key, &processed);
    if let Some(p) = &state.previews {
        p.drop_keys(&keys);
    }

    // Files landing in (or leaving) `CCD/<tel>/<obj>` bump the *object* dir's
    // mtime only — root_mtime() stats entries of the root, so the inventory
    // cache cannot notice on its own. Every other mutator nulls it explicitly.
    if report.touched() > 0 {
        state.cache.lock().unwrap().records = None;
        // A move into a directory created by `＋ new destination` must show up
        // in the dropdown on the next page load.
        state.invalidate_dests();
    }

    Ok(Json(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AstroBin encodes coordinates as decimal-degree *strings* (`"ra":
    /// "25.760"`); the reader must accept both strings and JSON numbers.
    #[test]
    fn astrobin_degrees_accept_string_encoding() {
        let ra = serde_json::json!("25.760");
        let dec = serde_json::json!("13.648");
        assert_eq!(ra_hms(deg_value(&ra).unwrap()), "01h43m02s");
        assert_eq!(dec_dms(deg_value(&dec).unwrap()), "+13\u{00b0}38\u{2032}52\u{2033}");
        assert_eq!(deg_value(&serde_json::json!(25.760)), Some(25.760));
        // The plate-solved fields are more precise and must not lose seconds.
        assert_eq!(
            dec_dms(deg_value(&serde_json::json!("13.647493760")).unwrap()),
            "+13\u{00b0}38\u{2032}50\u{2033}"
        );
    }

    #[test]
    fn southern_declination_keeps_its_sign() {
        assert_eq!(
            dec_dms(deg_value(&serde_json::json!("-29.5598")).unwrap()),
            "-29\u{00b0}33\u{2032}35\u{2033}"
        );
    }

    #[test]
    fn unparseable_coordinate_stays_blank() {
        for v in [serde_json::json!(""), serde_json::json!("n/a"), serde_json::Value::Null] {
            assert_eq!(deg_value(&v), None, "{v} must not yield a coordinate");
        }
        // RA wraps at 360°, not 24h.
        assert_eq!(
            ra_hms(deg_value(&serde_json::json!("359.99")).unwrap()),
            "23h59m57s"
        );
    }

    /// The config telescope map translates staging folder names (mount-based)
    /// into inventory ones (instrument-based): both the dropdown scope and the
    /// preselect follow the mapping, unmapped names fall back to name equality,
    /// and staging folders with no inventory twin drop out of the scope.
    #[test]
    fn staging_move_hint_follows_the_config_telescope_map() {
        let root = scratch("hintmap");
        for t in ["10RC", "8RC"] {
            std::fs::create_dir_all(root.join(t)).unwrap();
        }
        let staging = root.join("staging");
        for t in ["Pier", "RC", "GT81"] {
            std::fs::create_dir_all(staging.join(t).join("Darks")).unwrap();
        }
        let mut tel_map = HashMap::new();
        tel_map.insert("Pier".into(), "10RC".into());
        tel_map.insert("RC".into(), "8RC".into());
        let state = AppState::new(
            root.clone(),
            20,
            0.0,
            0.0,
            root.join("compendium"),
            Some(staging.clone()),
            root.join("cache"),
            vec![root.clone(), staging.clone()],
            500,
            tel_map,
        );
        let (tels, suggest) = state.staging_move_hint(&staging.join("Pier/Darks")).unwrap();
        assert_eq!(
            tels,
            vec!["10RC".to_string(), "8RC".to_string()],
            "mapped telescopes form the scope; GT81 has no inventory twin and drops out"
        );
        assert_eq!(suggest.as_deref(), Some("10RC"), "Pier preselects 10RC");
        // Unmapped staging names keep the old name-equality behaviour.
        let (_, suggest) = state.staging_move_hint(&staging.join("GT81/Darks")).unwrap();
        assert_eq!(suggest, None);
        // Not under staging at all -> no hint.
        assert!(state.staging_move_hint(&root.join("10RC")).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The post-apply tidy-up: empty directories go, non-empty ones fail
    /// safely with a reported reason, and a review root is never removed.
    #[test]
    fn tidy_src_dir_removes_only_empty_dirs_and_never_a_review_root() {
        let root = scratch("tidy");
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let full = root.join("full");
        std::fs::create_dir_all(&full).unwrap();
        std::fs::write(full.join("keep.fits"), b"x").unwrap();
        let mut rep = review::ApplyReport::default();
        tidy_src_dir(&empty, &[root.clone()], &mut rep);
        assert!(!empty.exists(), "an emptied directory is removed");
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        tidy_src_dir(&full, &[root.clone()], &mut rep);
        assert!(full.exists(), "a directory that still holds files is kept");
        assert!(rep.errors.iter().any(|e| e.contains("remove source dir")), "{:?}", rep.errors);
        tidy_src_dir(&root, &[root.clone()], &mut rep);
        assert!(root.exists(), "a review root is never removed");
        assert!(rep.errors.iter().any(|e| e.contains("review root")), "{:?}", rep.errors);
        std::fs::remove_dir_all(&root).ok();
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "astro_newdest_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("81GT")).unwrap();
        d
    }

    #[test]
    fn create_object_makes_the_dir_and_plan_md() {
        let root = scratch("basic");
        let meta = EMPTY_META;
        let created = create_object(&root, "81GT", "Jacoby1", &meta, true).unwrap();
        assert_eq!(created, root.join("81GT").join("Jacoby1"));
        assert!(created.is_dir());
        let plan = std::fs::read_to_string(created.join("plan.md")).unwrap();
        assert_eq!(plan, "# Jacoby1\n\n## Plan\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn create_object_writes_front_matter_when_given() {
        let root = scratch("meta");
        let meta = PlanMeta {
            constellation: "Cyg",
            ra: "20h",
            dec: "",
            rotation: "",
            sample: "",
        };
        let created = create_object(&root, "81GT", "M39", &meta, true).unwrap();
        let plan = std::fs::read_to_string(created.join("plan.md")).unwrap();
        assert!(plan.starts_with("---\nconstellation: "), "{plan}");
        assert!(plan.contains("ra: 20h"), "{plan}");
        assert!(!plan.contains("dec:"), "{plan}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn create_object_refuses_names_that_escape() {
        let root = scratch("escape");
        let meta = EMPTY_META;
        for bad in ["..", "../81GT", "a/../b", "a\\b", ".", "\0", "a//b"] {
            assert!(
                create_object(&root, "81GT", bad, &meta, true).is_err(),
                "accepted {bad:?}"
            );
        }
        // Nothing was created outside the telescope dir.
        assert_eq!(
            std::fs::read_dir(root.join("81GT")).unwrap().count(),
            0,
            "a rejected name still created something"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sanitize_object_name_keeps_only_alphanumerics() {
        assert_eq!(sanitize_object_name("NGC 7331 (Marat)"), "NGC_7331_Marat");
        assert_eq!(sanitize_object_name("M31"), "M31");
        assert_eq!(sanitize_object_name("M31--And."), "M31_And");
        assert_eq!(sanitize_object_name("  Abell  01  "), "Abell_01");
        // Accented Latin letters fold to their ASCII spelling, not to `_`.
        assert_eq!(sanitize_object_name("Andromède"), "Andromede");
        assert_eq!(sanitize_object_name("Céphée"), "Cephee");
        assert_eq!(sanitize_object_name("Müller"), "Muller");
        assert_eq!(sanitize_object_name("Å"), "A");
        assert_eq!(sanitize_object_name("Æon"), "AEon");
        assert_eq!(sanitize_object_name("straße"), "strasse");
        assert_eq!(sanitize_object_name("Østfold"), "Ostfold");
        assert_eq!(sanitize_object_name("Łódź"), "Lodz");
        assert_eq!(sanitize_object_name("Dvořák"), "Dvorak");
        // Non-Latin scripts have no ASCII spelling and become `_`; a leading `_`
        // is then trimmed, so `М31` lands as `31`.
        assert_eq!(sanitize_object_name("М31"), "31");
        assert_eq!(sanitize_object_name("М31 М32"), "31_32");
        // `/` is the nesting separator, not a character to convert.
        assert_eq!(sanitize_object_name("M42/deep/sub"), "M42/deep/sub");
        assert_eq!(sanitize_object_name("M42 /deep/ sub "), "M42/deep/sub");
        assert_eq!(sanitize_object_name("M42 Céphée deep"), "M42_Cephee_deep");
        assert_eq!(sanitize_object_name("!!!"), "");
        // Idempotent, including on accented input: handlers normalise before
        // create_object normalises again.
        for s in ["NGC 7331 (Marat)", "Andromède", "M42 Céphée deep"] {
            let once = sanitize_object_name(s);
            assert_eq!(sanitize_object_name(&once), once, "{s}");
        }
    }

    #[test]
    fn create_object_folds_accents_into_the_directory_and_the_heading() {
        let root = scratch("accent");
        let meta = EMPTY_META;
        let created = create_object(&root, "81GT", "Céphée (Flamsteed)", &meta, true).unwrap();
        assert_eq!(created, root.join("81GT").join("Cephee_Flamsteed"));
        let plan = std::fs::read_to_string(created.join("plan.md")).unwrap();
        assert_eq!(plan, "# Cephee_Flamsteed\n\n## Plan\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn create_object_sanitises_the_directory_and_the_plan_heading() {
        let root = scratch("sanitise");
        let meta = EMPTY_META;
        let created = create_object(&root, "81GT", "NGC 7331 (Marat)", &meta, true).unwrap();
        assert_eq!(created, root.join("81GT").join("NGC_7331_Marat"));
        // The heading uses the name that was created, not the typed one.
        let plan = std::fs::read_to_string(created.join("plan.md")).unwrap();
        assert_eq!(plan, "# NGC_7331_Marat\n\n## Plan\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn create_object_sanitises_each_nested_segment() {
        let root = scratch("nested_sanitise");
        let meta = EMPTY_META;
        let created = create_object(&root, "81GT", "M42 deep/sub dir", &meta, true).unwrap();
        assert_eq!(created, root.join("81GT").join("M42_deep/sub_dir"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn create_object_refuses_a_name_that_sanitises_to_nothing() {
        let root = scratch("empty_name");
        let meta = EMPTY_META;
        for bad in ["!!!", "(( ))", "!!!/x", "///"] {
            assert!(
                create_object(&root, "81GT", bad, &meta, true).is_err(),
                "accepted {bad:?}"
            );
        }
        assert_eq!(
            std::fs::read_dir(root.join("81GT")).unwrap().count(),
            0,
            "a rejected name still created something"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn create_object_nests_deeper_names_with_plan_md_at_the_leaf() {
        let root = scratch("nested");
        let meta = EMPTY_META;
        let created = create_object(&root, "81GT", "M42/deep/sub", &meta, true).unwrap();
        assert!(created.is_dir(), "every intermediate level is created");
        assert!(
            created.join("plan.md").is_file(),
            "plan.md belongs at the leaf, as for a top-level object"
        );
        // An escaping segment inside a nested name is refused, and the
        // escape target is never created.
        assert!(create_object(&root, "81GT", "M42/../../x", &meta, true).is_err());
        assert!(!root.join("x").exists());
        // A nested name that already exists is refused like any other.
        assert!(create_object(&root, "81GT", "M42/deep/sub", &meta, true).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The move screen creates bare landing folders: no `plan.md`, unlike the
    /// index form.
    #[test]
    fn create_object_without_plan_leaves_no_plan_md() {
        let root = scratch("noplan");
        let created = create_object(&root, "81GT", "Landing", &EMPTY_META, false).unwrap();
        assert!(created.is_dir());
        assert!(!created.join("plan.md").exists(), "move destinations must not grow a plan.md");
        assert!(root.join("81GT").join("Landing").is_dir());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn create_object_refuses_a_telescope_that_is_not_a_directory() {
        let root = scratch("tel");
        let meta = EMPTY_META;
        assert!(create_object(&root, "Pier", "X", &meta, true).is_err());
        assert!(create_object(&root, "../RC", "X", &meta, true).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn create_object_never_adopts_an_existing_directory() {
        let root = scratch("exists");
        let meta = EMPTY_META;
        assert!(create_object(&root, "81GT", "Dup", &meta, true).is_ok());
        let err = create_object(&root, "81GT", "Dup", &meta, true).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }
}
