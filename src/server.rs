use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
use axum::{extract::Form, Router};
use chrono::Local;
use serde::Deserialize;
use tokio::sync::Semaphore;
use crate::plan::expand_constellation;
use crate::render::{
    col_by_key, page_size_control, render_edit_page, render_header_cells, render_index_page,
    render_pager, render_review, render_review_index, sort_records,
};
use crate::traverse::{build_row, traverse, ObjectRecord};
use crate::review::{self, Manifest, PreviewCache};
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
    previews: Option<PreviewCache>,
    /// Parsed manifests keyed by canonical reviewed directory.
    manifests: Mutex<HashMap<String, Manifest>>,
    /// Walked destination list. Rebuilt after anything that can add a directory
    /// (apply, newdest) — a full walk of ~1,900 dirs measured 1.05 s.
    dests: Mutex<Option<Vec<review::Destination>>>,
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
            previews,
            manifests: Mutex::new(HashMap::new()),
            dests: Mutex::new(None),
            render_permits: Semaphore::new(1),
        })
    }

    /// Resolve a client-supplied path, refusing anything outside the allowed
    /// review roots. Every review route goes through this first.
    fn review_path(&self, raw: &str) -> Result<std::path::PathBuf, (StatusCode, String)> {
        review::resolve_within_any(&self.review_roots, raw).map_err(|e| (StatusCode::BAD_REQUEST, e))
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
        .route("/review/destfiles", get(review_destfiles))
        .route("/review/newdest", post(review_newdest))
        .route("/review/apply", post(review_apply))
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
        if state.staging_root.is_some() {
            " &middot; <a href=\"/review\">Review directories</a> &middot; <a href=\"/staging\">Staging</a>"
        } else {
            " &middot; <a href=\"/review\">Review directories</a>"
        },
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

/// Create `CCD/<telescope>/<name>` together with its `plan.md`.
///
/// Shared by the index form and `POST /staging/newdest` so a destination made
/// from the review page is indistinguishable from one made on the index:
/// `traverse.rs` reads `plan.md` for RA/Dec and for the latest-image fallback,
/// so a bare directory would behave differently in the inventory.
///
/// Errors are human-readable because both callers surface them verbatim.
fn create_object(
    root: &std::path::Path,
    telescope: &str,
    name: &str,
    meta: &PlanMeta<'_>,
) -> Result<PathBuf, String> {
    if telescope.is_empty() || name.is_empty() {
        return Err("Please choose a telescope and enter an object name.".to_string());
    }
    // The index form only sanitised `name` because its telescope came from a
    // closed select; this is also reachable from JSON, so both are checked.
    if bad_component(telescope) {
        return Err(format!("Invalid telescope name: {telescope}"));
    }
    if bad_component(name) {
        return Err(format!("Invalid object name: {name}"));
    }
    if !root.join(telescope).is_dir() {
        return Err(format!("Telescope directory not found: {telescope}"));
    }
    // Belt and braces: prove the joined path stays under the CCD root even
    // though the components are already sanitised. The candidate must be
    // absolute — resolve_within canonicalizes the *parent*, and a relative
    // parent would be resolved against the process CWD, not the root.
    let root_real = std::fs::canonicalize(root).map_err(|e| format!("CCD root: {e}"))?;
    let obj_dir =
        review::resolve_within(&root_real, &root_real.join(telescope).join(name).to_string_lossy())?;
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
        std::fs::write(obj_dir.join("plan.md"), content)?;
        Ok(())
    })()
    .map_err(|e| format!("Failed to create {}: {e}", obj_dir.display()))?;
    Ok(obj_dir)
}

async fn add_object(
    State(state): State<Arc<AppState>>,
    Form(form): Form<AddForm>,
) -> Response {
    let telescope = form.telescope.unwrap_or_default().trim().to_string();
    let name = form.object.unwrap_or_default().trim().to_string();
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
    match create_object(&state.root, &telescope, &name, &meta) {
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
    let name = req.name.trim().to_string();
    let st = state.clone();
    let (tel, nm) = (telescope.clone(), name.clone());
    let created = tokio::task::spawn_blocking(move || {
        create_object(&st.root, &tel, &nm, &EMPTY_META)
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

    // Convert RA from decimal degrees to HMS
    let ra_str = if sol.get("ra").is_some() {
        let ra_deg = sol["ra"].as_f64().unwrap_or(0.0) % 360.0;
        let total_sec = ra_deg * 240.0;
        let h = (total_sec / 3600.0) as i64;
        let m = ((total_sec % 3600.0) / 60.0) as i64;
        let s = (total_sec % 60.0) as i64;
        format!("{h:02}h{m:02}m{s:02}s")
    } else {
        String::new()
    };

    // Convert DEC from decimal degrees to DMS
    let dec_str = if sol.get("dec").is_some() {
        let dec_deg = sol["dec"].as_f64().unwrap_or(0.0);
        let sign = if dec_deg >= 0.0 { "+" } else { "-" };
        let dec_abs = dec_deg.abs();
        let d = dec_abs as i64;
        let m = ((dec_abs - d as f64) * 60.0) as i64;
        let s = (((dec_abs - d as f64) * 60.0 - m as f64) * 60.0) as i64;
        format!("{sign}{d}\u{00b0}{m:02}\u{2032}{s:02}\u{2033}")
    } else {
        String::new()
    };

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
            "ra": ra_str,
            "dec": dec_str,
        })),
    )
        .into_response()
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
    Ok(Html(render_review(&dir.to_string_lossy(), state.nav_repeat_ms)))
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

async fn review_destinations(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<review::Destination>>, (StatusCode, String)> {
    let roots = state.review_roots.clone();
    if let Some(cached) = state.cached_dests() {
        return Ok(Json(cached));
    }
    // A bounded read_dir walk, deliberately NOT get_records(): that path takes
    // a std Mutex across a traverse measured at ~48 s, which would stall every
    // other request.
    let dirs = tokio::task::spawn_blocking(move || review::list_destinations(&roots, 2_000))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state.set_dests(dirs.clone());
    Ok(Json(dirs))
}

/// Filenames already in a destination, so the UI can flag collisions before
/// anything is written.
async fn review_destfiles(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReviewQuery>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let raw = q.dir.ok_or((StatusCode::BAD_REQUEST, "dir is required".to_string()))?;
    let dir = state.review_path(&raw)?;
    let files = tokio::task::spawn_blocking(move || review::dest_files(&dir, 20_000))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(files))
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
        state.review_path(raw)?;
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

    let roots = state.review_roots.clone();
    let files = processed.clone();
    let report = tokio::task::spawn_blocking(move || {
        review::apply(&dir, &roots, op, dst.as_deref(), &files)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
        let created = create_object(&root, "81GT", "Jacoby1", &meta).unwrap();
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
        let created = create_object(&root, "81GT", "M39", &meta).unwrap();
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
        for bad in ["..", "../81GT", "a/b", "a\\b", ".", "\0"] {
            assert!(
                create_object(&root, "81GT", bad, &meta).is_err(),
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
    fn create_object_refuses_a_telescope_that_is_not_a_directory() {
        let root = scratch("tel");
        let meta = EMPTY_META;
        assert!(create_object(&root, "Pier", "X", &meta).is_err());
        assert!(create_object(&root, "../RC", "X", &meta).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn create_object_never_adopts_an_existing_directory() {
        let root = scratch("exists");
        let meta = EMPTY_META;
        assert!(create_object(&root, "81GT", "Dup", &meta).is_ok());
        let err = create_object(&root, "81GT", "Dup", &meta).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }
}
