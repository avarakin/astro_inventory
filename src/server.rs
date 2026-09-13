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
use crate::plan::expand_constellation;
use crate::render::{
    col_by_key, page_size_control, render_edit_page, render_header_cells, render_index_page,
    render_pager, sort_records,
};
use crate::traverse::{build_row, traverse, ObjectRecord};

/// Shared application state.
pub struct AppState {
    pub root: PathBuf,
    pub page_size_default: usize,
    pub longitude: f64,
    cache: Mutex<ScanCache>,
}

struct ScanCache {
    records: Option<Vec<ObjectRecord>>,
    generated: Option<chrono::DateTime<Local>>,
    root_mtime: f64,
}

impl AppState {
    pub fn new(root: PathBuf, page_size: usize, longitude: f64) -> Arc<Self> {
        Arc::new(Self {
            root,
            page_size_default: page_size,
            longitude,
            cache: Mutex::new(ScanCache {
                records: None,
                generated: None,
                root_mtime: 0.0,
            }),
        })
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
            let records = traverse(&self.root, self.longitude);
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
            build_row(rec, &image_url_fn, &actions_url_fn)
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

    if telescope.is_empty() || name.is_empty() {
        return redirect_with_flash(
            "error",
            "Please choose a telescope and enter an object name.",
        );
    }

    let tel_dir = state.root.join(&telescope);
    if !tel_dir.is_dir() {
        return redirect_with_flash(
            "error",
            &format!("Telescope directory not found: {telescope}"),
        );
    }

    if name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name == "."
        || name == ".."
    {
        return redirect_with_flash("error", "Invalid object name.");
    }

    let obj_dir = state.root.join(&telescope).join(&name);
    if obj_dir.exists() {
        return redirect_with_flash(
            "error",
            &format!("Object directory already exists: {telescope}/{name}"),
        );
    }

    let plan_path = obj_dir.join("plan.md");
    if let Err(e) = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&obj_dir)?;
        let mut fm = Vec::new();
        if !constellation.is_empty() {
            fm.push(format!(
                "constellation: {}",
                expand_constellation(&constellation)
            ));
        }
        if !ra.is_empty() {
            fm.push(format!("ra: {ra}"));
        }
        if !dec.is_empty() {
            fm.push(format!("dec: {dec}"));
        }
        if !rotation.is_empty() {
            fm.push(format!("rotation: {rotation}"));
        }
        if !sample.is_empty() {
            fm.push(format!("link: {sample}"));
        }
        let mut content = String::new();
        if !fm.is_empty() {
            content.push_str("---\n");
            content.push_str(&fm.join("\n"));
            content.push('\n');
            content.push_str("---\n\n");
        }
        content.push_str(&format!("# {name}\n\n## Plan\n"));
        std::fs::write(&plan_path, content)?;
        Ok(())
    })() {
        return redirect_with_flash(
            "error",
            &format!("Failed to create {}: {e}", obj_dir.display()),
        );
    }

    // Force cache refresh
    {
        let mut cache = state.cache.lock().unwrap();
        cache.records = None;
    }

    redirect_with_flash(
        "ok",
        &format!("Created {telescope}/{name}/plan.md"),
    )
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
