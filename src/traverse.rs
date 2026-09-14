use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use chrono::{DateTime, Local, TimeZone, Utc};

fn mtime_to_local(mtime: std::time::SystemTime) -> Option<DateTime<Local>> {
    let ts = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?.as_secs_f64();
    let utc = Utc.timestamp_opt(ts as i64, 0).single()?;
    Some(Local.from_utc_datetime(&utc.naive_utc()))
}

use crate::plan::html_escape;
use crate::transit::transit_date;

/// Filter display names.
pub const FILTERS: &[(&str, &str)] = &[
    ("L", "Luminance"),
    ("R", "Red"),
    ("B", "Blue"),
    ("G", "Green"),
    ("S", "SII"),
    ("O", "OIII"),
    ("H", "Ha"),
];

pub fn filter_label(letter: &str) -> &str {
    for (k, v) in FILTERS {
        if *k == letter {
            return v;
        }
    }
    letter
}

/// Stats for a single filter.
#[derive(Debug, Clone)]
pub struct FilterStats {
    pub count: u64,
    pub seconds: f64,
    pub durations: BTreeMap<i64, u64>,
}

impl FilterStats {
    fn new() -> Self {
        Self {
            count: 0,
            seconds: 0.0,
            durations: BTreeMap::new(),
        }
    }
}

/// A single object record.
#[derive(Debug, Clone)]
pub struct ObjectRecord {
    pub telescope: String,
    pub object: String,
    pub path: PathBuf,
    pub filters: BTreeMap<String, FilterStats>,
    pub cr2cr3_count: u64,
    pub latest: Option<DateTime<Local>>,
    pub has_project: bool,
    pub has_final: bool,
    pub size_bytes: u64,
    pub final_images: Vec<String>,
    pub plan: Option<String>,
    pub plan_mtime: Option<DateTime<Local>>,
    pub transit: Option<String>,
    pub transit_sort: f64,
}

// --- Filename parsing --------------------------------------------------------

/// Parse exposure duration from filename. Returns seconds or None.
static RE_DUR_S: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"_(\d+(?:\.\d+)?)s_").unwrap());
static RE_DUR_SECS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new("(?i)_(\\d+(?:\\.\\d+)?)_secs_").unwrap());
static RE_DUR_EXPOSURE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new("(?i)EXPOSURE-(\\d+(?:\\.\\d+)?)s").unwrap());

pub fn parse_duration(name: &str) -> Option<f64> {

    if let Some(m) = RE_DUR_S.captures(name) {
        if let Some(g) = m.get(1) {
            return g.as_str().parse::<f64>().ok();
        }
    }
    if let Some(m) = RE_DUR_SECS.captures(name) {
        if let Some(g) = m.get(1) {
            return g.as_str().parse::<f64>().ok();
        }
    }
    if let Some(m) = RE_DUR_EXPOSURE.captures(name) {
        if let Some(g) = m.get(1) {
            return g.as_str().parse::<f64>().ok();
        }
    }
    None
}

/// Parse filter letter from filename. Returns the letter or None (DSLR).
static RE_FILTER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"_([LRBGSOH])_").unwrap());

pub fn parse_filter(name: &str) -> Option<String> {
    RE_FILTER.captures(name)
        .and_then(|m| m.get(1))
        .map(|m| m.as_str().to_string())
}

/// Parse timestamp from filename. Returns local naive datetime or None.
static RE_TS_ISO: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(\d{4}-\d{2}-\d{2})[T_ ](\d{2})[._-](\d{2})[._-](\d{2})").unwrap()
});
static RE_TS_COMPACT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(\d{4})(\d{2})(\d{2})_(\d{2})(\d{2})(\d{2})").unwrap()
});

pub fn parse_timestamp(name: &str) -> Option<DateTime<Local>> {
    if let Some(m) = RE_TS_ISO.captures(name) {
        let date_str = format!(
            "{} {}:{}:{}",
            m.get(1).unwrap().as_str(),
            m.get(2).unwrap().as_str(),
            m.get(3).unwrap().as_str(),
            m.get(4).unwrap().as_str()
        );
        if let Ok(dt) =
            chrono::NaiveDateTime::parse_from_str(&date_str, "%Y-%m-%d %H:%M:%S")
        {
            if let Some(local) = Local.from_local_datetime(&dt).single() {
                return Some(local);
            }
        }
    }

    if let Some(m) = RE_TS_COMPACT.captures(name) {
        let date_str: String = (1..=6)
            .map(|i| m.get(i).unwrap().as_str().to_string())
            .collect();
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&date_str, "%Y%m%d%H%M%S") {
            if let Some(local) = Local.from_local_datetime(&dt).single() {
                return Some(local);
            }
        }
    }

    None
}

/// Parse RA from plan.md text. Returns hours (f64) or None.
static RE_RA_YAML: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?m)^ra:\s*(\d+)h(\d+)m(\d+)s").unwrap());
static RE_RA_OLD: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\*\*RA:\*\*\s*(\d+)h(\d+)m(\d+)s").unwrap());

pub fn parse_ra_hours(text: &str) -> Option<f64> {
    if text.is_empty() {
        return None;
    }
    // YAML frontmatter: ra: 23h20m29s
    let m = RE_RA_YAML
        .captures(text)
        .or_else(|| RE_RA_OLD.captures(text))?;
    let h = m.get(1).unwrap().as_str().parse::<f64>().ok()?;
    let mi = m.get(2).unwrap().as_str().parse::<f64>().ok()?;
    let s = m.get(3).unwrap().as_str().parse::<f64>().ok()?;
    Some(h + mi / 60.0 + s / 3600.0)
}

// --- Formatting helpers ------------------------------------------------------

/// Format a duration value: None -> "?", integer -> "300", else shortest repr.
pub fn fmt_duration(s: Option<f64>) -> String {
    match s {
        None => "?".to_string(),
        Some(v) => {
            if (v - v.round()).abs() < 1e-9 {
                (v.round() as i64).to_string()
            } else {
                format!("{}", v)
            }
        }
    }
}

/// Format total seconds as e.g. '2h 30m 15s'.
pub fn fmt_total(seconds: Option<f64>) -> String {
    let seconds = match seconds {
        Some(s) => s,
        None => return "?".to_string(),
    };
    let seconds = seconds as i64;
    let h = seconds / 3600;
    let rem = seconds % 3600;
    let m = rem / 60;
    let s = rem % 60;

    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{h}h"));
    }
    if m > 0 {
        parts.push(format!("{m}m"));
    }
    if s > 0 || parts.is_empty() {
        parts.push(format!("{s}s"));
    }
    parts.join(" ")
}

/// Total seconds for a record.
pub fn total_seconds(rec: &ObjectRecord) -> f64 {
    let mut total = 0.0;
    for stats in rec.filters.values() {
        total += stats.seconds;
    }
    total += rec.cr2cr3_count as f64 * 30.0;
    total
}

// --- Traversal ---------------------------------------------------------------

/// Traverse the root directory and build object records.
pub fn traverse(root: &Path, longitude: f64) -> Vec<ObjectRecord> {
    let _t0 = std::time::Instant::now();
    eprintln!("[profile] traverse: start");
    let mut records = Vec::new();

    let root_entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return records,
    };

    for tel_entry in root_entries.flatten() {
        let tel_path = tel_entry.path();
        if !tel_path.is_dir() {
            continue;
        }
        let tel_name = tel_entry.file_name().to_string_lossy().to_string();

        let obj_entries = match std::fs::read_dir(&tel_path) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for obj_entry in obj_entries.flatten() {
            let obj_path = obj_entry.path();
            if !obj_path.is_dir() {
                continue;
            }
            let obj_name = obj_entry.file_name().to_string_lossy().to_string();

            // Skip calibration directories
            let lower = obj_name.to_lowercase();
            if lower == "darks" || lower == "cal" || lower == "calibration" || lower == "flats" {
                continue;
            }

            let mut rec = ObjectRecord {
                telescope: tel_name.clone(),
                object: obj_name.clone(),
                path: obj_path.clone(),
                filters: BTreeMap::new(),
                cr2cr3_count: 0,
                latest: None,
                has_project: false,
                has_final: false,
                size_bytes: 0,
                final_images: Vec::new(),
                plan: None,
                plan_mtime: None,
                transit: None,
                transit_sort: 0.0,
            };

            // Recursive walk
            let master_path = obj_path.join("master");
            walk_object_tree(&obj_path, &master_path, &mut rec);

            // plan.md handling
            let plan_path = obj_path.join("plan.md");
            let _tp = std::time::Instant::now();
            if plan_path.is_file() {
                let plan_mtime = std::fs::metadata(&plan_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(mtime_to_local);

                rec.plan_mtime = plan_mtime;

                match std::fs::read_to_string(&plan_path) {
                    Ok(text) => {
                        rec.plan = Some(text.clone());
                    }
                    Err(_) => {
                        rec.plan = Some(String::new());
                    }
                }

                // If no images found, use plan.md mtime as latest
                if rec.latest.is_none() {
                    rec.latest = rec.plan_mtime;
                }

                // Compute transit date from RA in plan
                if let Some(plan_text) = &rec.plan {
                    if let Some(ra_h) = parse_ra_hours(plan_text) {
                        if let Some(td) = transit_date(ra_h, longitude) {
                            rec.transit = Some(td.format("%Y-%m-%d").to_string());
                            rec.transit_sort = td.timestamp() as f64;
                        }
                    }
                }
            } else {
                rec.plan = None;
            }
            if _tp.elapsed().as_millis() > 50 {
                eprintln!("[profile] plan.md block for {}/{obj_name}: {:?}", tel_name, _tp.elapsed());
            }

            records.push(rec);
        }
    }

    eprintln!("[profile] traverse: done in {:?} ({} records)", _t0.elapsed(), records.len());
    records
}

/// Recursively walk an object directory tree.
fn walk_object_tree(obj_path: &Path, master_path: &Path, rec: &mut ObjectRecord) {
    let mut stack = vec![obj_path.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();

            if path.is_dir() {
                // Check for .pxiproject directories
                if dir == obj_path || &dir == master_path {
                    if file_name.to_lowercase().ends_with(".pxiproject") {
                        rec.has_project = true;
                    }
                }
                // Recurse into subdirectories
                stack.push(path);
                continue;
            }

            // Accumulate size
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.is_file() {
                    rec.size_bytes += meta.len();
                }
            }

            let lower = file_name.to_lowercase();

            // Check master/ for final images
            if dir == master_path {
                if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
                    rec.has_final = true;
                    rec.final_images.push(file_name.clone());
                }
            }

            // Image files at any depth
            if lower.ends_with(".fits")
                || lower.ends_with(".fit")
                || lower.ends_with(".cr2")
                || lower.ends_with(".cr3")
            {
                let mtime = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(mtime_to_local);

                if lower.ends_with(".fits") || lower.ends_with(".fit") {
                    let dur = parse_duration(&file_name);
                    let flt = parse_filter(&file_name).unwrap_or_else(|| "DSLR".to_string());

                    let stats = rec.filters.entry(flt).or_insert_with(FilterStats::new);
                    stats.count += 1;
                    if let Some(d) = dur {
                        stats.seconds += d;
                        *stats.durations.entry(d as i64).or_insert(0) += 1;
                    }

                    let ts = parse_timestamp(&file_name).or(mtime);
                    if let Some(ts) = ts {
                        rec.latest = Some(match rec.latest {
                            Some(existing) => existing.max(ts),
                            None => ts,
                        });
                    }
                } else if lower.ends_with(".cr2") || lower.ends_with(".cr3") {
                    rec.cr2cr3_count += 1;
                    let ts = parse_timestamp(&file_name).or(mtime);
                    if let Some(ts) = ts {
                        rec.latest = Some(match rec.latest {
                            Some(existing) => existing.max(ts),
                            None => ts,
                        });
                    }
                }
            }
        }
    }
}

// --- Row rendering helpers ---------------------------------------------------

/// Render the filters column for a record.
pub fn render_filters(rec: &ObjectRecord) -> String {
    let mut filter_parts = Vec::new();

    // Sort: DSLR last, others alphabetical
    let mut keys: Vec<&String> = rec.filters.keys().collect();
    keys.sort_by(|a, b| {
        let a_dslr = a.as_str() == "DSLR";
        let b_dslr = b.as_str() == "DSLR";
        if a_dslr != b_dslr {
            if a_dslr {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            }
        } else {
            a.cmp(b)
        }
    });

    for flt in keys {
        let stats = &rec.filters[flt];
        let durs: Vec<String> = stats
            .durations
            .keys()
            .map(|k| format!("{}s", fmt_duration(Some(*k as f64))))
            .collect();
        let label = filter_label(flt);
        filter_parts.push(format!(
            "{label}: {}x ({}) = {}",
            stats.count,
            durs.join(", "),
            fmt_total(Some(stats.seconds))
        ));
    }

    if rec.cr2cr3_count > 0 {
        filter_parts.push(format!(
            "DSLR (CR2/CR3): {}x (30s) = {}",
            rec.cr2cr3_count,
            fmt_total(Some(rec.cr2cr3_count as f64 * 30.0))
        ));
    }

    if filter_parts.is_empty() {
        "—".to_string()
    } else {
        filter_parts.join("<br>")
    }
}

/// Render a single row as HTML.
pub fn build_row(
    rec: &ObjectRecord,
    image_url_fn: &dyn Fn(&Path) -> String,
    actions_url_fn: &dyn Fn(&str, &str) -> (String, String),
) -> String {
    let total_sec = total_seconds(rec);

    let filters_html = render_filters(rec);

    let latest = rec.latest;
    let ts_sort = latest.map(|d| d.timestamp() as f64).unwrap_or(0.0);
    let ts_disp = latest
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".to_string());

    let cls = if !rec.has_project && !rec.has_final {
        "red"
    } else if rec.has_project && !rec.has_final {
        "yellow"
    } else {
        ""
    };

    let yn = |v: bool| -> (String, String) {
        if v {
            (
                "1".to_string(),
                "<span style=\"color:#4caf50;font-weight:bold\">✔</span>".to_string(),
            )
        } else {
            (
                "0".to_string(),
                "<span style=\"color:#f44336;font-weight:bold\">✘</span>".to_string(),
            )
        }
    };

    let (p_sort, p_disp) = yn(rec.has_project);
    let (f_sort, f_disp) = yn(rec.has_final);

    let plan_html = rec
        .plan
        .as_deref()
        .map(|p| crate::plan::render_plan(p))
        .unwrap_or_else(|| "—".to_string());

    let size_mb = rec.size_bytes as f64 / (1024.0 * 1024.0);

    // Thumbnails
    let mut thumbs = Vec::new();
    for fn_name in &rec.final_images {
        let img_path = rec.path.join("master").join(fn_name);
        let url = image_url_fn(&img_path);
        thumbs.push(format!(
            "<a href=\"{}\" target=\"_blank\"><img src=\"{}\" alt=\"{}\"></a>",
            html_escape(&url),
            html_escape(&url),
            html_escape(fn_name)
        ));
    }
    let thumbs_html = if thumbs.is_empty() {
        "—".to_string()
    } else {
        format!("<span class=\"thumbs\">{}</span>", thumbs.join(" "))
    };

    // Actions
    let (edit_url, delete_url) = actions_url_fn(&rec.telescope, &rec.object);
    let obj_label = html_escape(&format!("{}/{}", rec.telescope, rec.object));
    let actions_html = format!(
        "<button class=\"btn btn-edit\" onclick=\"window.open('{}', '_blank')\">Edit plan</button> \
         <button class=\"btn btn-del\" \
         onclick=\"if(confirm('Delete {} and all its files? This cannot be undone.'))document.getElementById('del_0').submit();return false\">Delete directory</button> \
         <form id=\"del_0\" method=\"post\" action=\"{}\" style=\"display:none\"><input type=\"hidden\" name=\"confirm\" value=\"yes\"></form>",
        html_escape(&edit_url),
        obj_label,
        html_escape(&delete_url)
    );

    let transit_disp = rec.transit.as_deref().unwrap_or("—");
    let transit_sort = rec.transit_sort;

    format!(
        "<tr class=\"{cls}\">\
         <td data-sort=\"{p_sort}\">{p_disp}</td>\
         <td data-sort=\"{f_sort}\">{f_disp}</td>\
         <td data-sort=\"{ts_sort}\">{}</td>\
         <td data-sort=\"{}\">{}</td>\
         <td data-sort=\"{}\">{}</td>\
         <td data-sort=\"{transit_sort}\">{}</td>\
         <td data-sort=\"{total_sec}\">{}</td>\
         <td data-sort=\"{size_mb:.6}\">{size_mb:.1}</td>\
         <td data-sort=\"\">{filters_html}</td>\
         <td data-sort=\"\">{thumbs_html}</td>\
         <td data-sort=\"\">{plan_html}</td>\
         <td data-sort=\"\">{actions_html}</td>\
         </tr>",
        html_escape(&ts_disp),
        html_escape(&rec.telescope),
        html_escape(&rec.telescope),
        html_escape(&rec.object),
        html_escape(&rec.object),
        html_escape(transit_disp),
        fmt_total(Some(total_sec)),
    )
}
