use crate::plan::html_escape;
use crate::traverse::ObjectRecord;
use percent_encoding::{percent_encode, AsciiSet, NON_ALPHANUMERIC};

/// Encodes a filesystem path for use as a query-string value: everything
/// non-alphanumeric except `/`. Without this a directory name containing `&`
/// or `#` silently truncates the `dir=` parameter.
const DIR_QUERY: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'/')
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// CSS for the dark theme (copied verbatim from astro_inventory_server.py).
pub const CSS: &str = r#"
  :root {
    --bg: #0b1020;
    --panel: #131a2e;
    --panel-2: #1a2340;
    --border: #26314f;
    --text: #e6ebf5;
    --muted: #8b96b3;
    --accent: #7c8cff;
    --accent-2: #5a6cf0;
    --ok-bg: #10331f; --ok-fg: #6ee7a0;
    --err-bg: #3a1520; --err-fg: #ff9db1;
    --warn-bg: #3a2f10; --warn-fg: #ffd97a;
    --red-bg: rgba(255, 99, 132, 0.12);
    --yellow-bg: rgba(255, 209, 102, 0.10);
  }
  * { box-sizing: border-box; }
  body {
    font-family: Inter, -apple-system, 'Segoe UI', Roboto, sans-serif;
    margin: 0; padding: 2em;
    background: radial-gradient(1200px 600px at 80% -10%, #1b2547 0%, var(--bg) 55%) fixed, var(--bg);
    color: var(--text);
  }
  h1 { font-size: 1.6em; font-weight: 700; letter-spacing: 0.3px; margin: 0 0 0.5em; }
  a { color: var(--accent); }
  .meta { color: var(--muted); margin-bottom: 1.2em; font-size: 14px; }
  .meta a { text-decoration: none; }
  .meta a:hover { text-decoration: underline; }
  table { border-collapse: separate; border-spacing: 0; width: 100%; font-size: 14px; background: var(--panel); border: 1px solid var(--border); border-radius: 10px; overflow: hidden; }
  th, td { padding: 10px 14px; text-align: left; border-bottom: 1px solid var(--border); }
  th {
    background: var(--panel-2); color: var(--text);
    position: sticky; top: 0; user-select: none; font-size: 12px; text-transform: uppercase; letter-spacing: 0.6px; font-weight: 600;
  }
  th a { color: var(--text); text-decoration: none; }
  th a:hover { color: var(--accent); }
  th.sortable { cursor: pointer; }
  th.sortable:hover { color: var(--accent); }
  /* Only the staging table uses .num; right-aligning it makes the numeric
     columns read as numbers and makes the sort direction obvious. */
  td.num, th.num { text-align: right; }
  th .arrow { display: inline-block; font-size: 15px; font-weight: bold; margin-left: 6px; vertical-align: middle; color: var(--accent); }
  tbody tr { background: transparent; transition: background 0.12s; }
  tbody tr:hover { background: rgba(124, 140, 255, 0.07); }
  tr.red { background: var(--red-bg) !important; }
  tr.yellow { background: var(--yellow-bg) !important; }
  .thumbs img { max-height: 60px; max-width: 120px; margin: 2px; border: 1px solid var(--border); border-radius: 4px; vertical-align: middle; }
  .plan { font-size: 12px; }
  .plan-top { margin-bottom: 4px; }
  .badge { display: inline-block; background: var(--accent-2); color: #fff; border-radius: 10px; padding: 2px 9px; font-size: 11px; font-weight: 600; margin-right: 6px; }
  .plan-link { display: inline-block; padding: 2px 10px; background: var(--accent-2); color: #fff; border-radius: 10px; text-decoration: none; font-size: 11px; font-weight: 600; }
  .plan-link:hover { background: var(--accent); }
  .plan-body { margin: 2px 0; line-height: 1.4; }
  .plan-meta { color: var(--muted); font-size: 11px; margin-top: 3px; }
  .btn { padding: 6px 14px; font-size: 13px; border: none; border-radius: 8px; cursor: pointer; font-weight: 600; }
  .btn-edit { background: var(--accent-2); color: #fff; }
  .btn-edit:hover { background: var(--accent); }
  .btn-del { background: #e05268; color: #fff; }
  .btn-del:hover { background: #f06a7d; }
  .addbox { border: 1px solid var(--border); border-radius: 12px; padding: 1.2em 1.4em; margin-bottom: 1.5em; background: var(--panel); }
  .addbox h2 { margin: 0 0 0.9em; font-size: 1.05em; font-weight: 600; }
  .addbox form { display: flex; align-items: center; gap: 0.6em; flex-wrap: wrap; }
  .addbox label { color: var(--muted); font-size: 13px; }
  .addbox select, .addbox input[type=text], .addbox input[type=number] {
    padding: 8px 10px; font-size: 14px; color: var(--text);
    background: var(--panel-2); border: 1px solid var(--border); border-radius: 8px;
  }
  .addbox input[type=text] { width: 240px; }
  .addbox input:focus, .addbox select:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px rgba(124, 140, 255, 0.2); }
  .addbox button { padding: 8px 18px; font-size: 14px; cursor: pointer; border: none; border-radius: 8px; font-weight: 600; }
  .addbox button[type=button] { background: var(--panel-2); color: var(--text); border: 1px solid var(--border); }
  .addbox button[type=button]:hover { border-color: var(--accent); color: var(--accent); }
  .addbox button[type=submit] { background: var(--accent-2); color: #fff; }
  .addbox button[type=submit]:hover { background: var(--accent); }
  .flash { margin: 0.6em 0 0; padding: 0.6em 1em; border-radius: 8px; font-size: 14px; }
  .flash.ok { background: var(--ok-bg); color: var(--ok-fg); border: 1px solid #1d5c39; }
  .flash.error { background: var(--err-bg); color: var(--err-fg); border: 1px solid #6b2737; }
  .pager { margin: 1em 0; font-size: 14px; }
  .pager a, .pager span.cur {
    padding: 4px 11px; margin: 0 2px; border: 1px solid var(--border); border-radius: 8px;
    text-decoration: none; color: var(--text); display: inline-block;
  }
  .pager a:hover { border-color: var(--accent); color: var(--accent); }
  .pager span.cur { background: var(--accent-2); color: #fff; border-color: var(--accent-2); }
  .pager span.disabled { color: var(--muted); border-color: var(--border); }
  .sep { color: var(--muted); }
  a.del { color: #ff8fa3; }
  textarea.plan { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; width: 100%; box-sizing: border-box; background: var(--panel-2); color: var(--text); border: 1px solid var(--border); border-radius: 8px; padding: 10px; }
  a.cancel { text-decoration: none; color: var(--accent); }
  button.danger { background: #e05268; color: #fff; border: none; padding: 8px 18px; border-radius: 8px; cursor: pointer; font-weight: 600; }
"#;

/// Column definition.
pub struct ColumnDef {
    pub key: &'static str,
    pub name: &'static str,
    pub sortable: bool,
    pub default_dir: &'static str,
}

/// All columns in display order.
pub const COLUMNS: &[ColumnDef] = &[
    ColumnDef { key: "project",   name: "Project",    sortable: true,  default_dir: "asc" },
    ColumnDef { key: "final",     name: "Final image", sortable: true,  default_dir: "asc" },
    ColumnDef { key: "latest",    name: "Latest image", sortable: true, default_dir: "desc" },
    ColumnDef { key: "telescope", name: "Telescope",  sortable: true,  default_dir: "asc" },
    ColumnDef { key: "object",    name: "Object",     sortable: true,  default_dir: "asc" },
    ColumnDef { key: "transit",   name: "Transit",    sortable: true,  default_dir: "asc" },
    ColumnDef { key: "total",     name: "Total exposure", sortable: true, default_dir: "desc" },
    ColumnDef { key: "size",      name: "Size (MB)",  sortable: true,  default_dir: "desc" },
    ColumnDef { key: "filters",   name: "Filters (count / duration / total)", sortable: false, default_dir: "" },
    ColumnDef { key: "masters",   name: "Master images", sortable: false, default_dir: "" },
    ColumnDef { key: "plan",      name: "Plan",       sortable: false, default_dir: "" },
    ColumnDef { key: "actions",   name: "Actions",    sortable: false, default_dir: "" },
];

/// Get column by key.
pub fn col_by_key(key: &str) -> Option<&'static ColumnDef> {
    COLUMNS.iter().find(|c| c.key == key)
}

/// Sort records by the given key and direction.
pub fn sort_records(records: &mut Vec<ObjectRecord>, key: &str, direction: &str) {
    let key = if col_by_key(key).map_or(true, |c| !c.sortable) {
        "latest"
    } else {
        key
    };

    let reverse = direction == "desc";

    records.sort_by(|a, b| {
        let cmp = match key {
            "project" => {
                let av = if a.has_project { 1 } else { 0 };
                let bv = if b.has_project { 1 } else { 0 };
                av.cmp(&bv)
            }
            "final" => {
                let av = if a.has_final { 1 } else { 0 };
                let bv = if b.has_final { 1 } else { 0 };
                av.cmp(&bv)
            }
            "latest" => {
                let av = a.latest.map(|d| d.timestamp()).unwrap_or(0);
                let bv = b.latest.map(|d| d.timestamp()).unwrap_or(0);
                av.cmp(&bv)
            }
            "telescope" => a.telescope.cmp(&b.telescope),
            "object" => a.object.cmp(&b.object),
            "transit" => {
                let av = if rec_transit_sort(a) == 0.0 { 9999999999.0 } else { rec_transit_sort(a) };
                let bv = if rec_transit_sort(b) == 0.0 { 9999999999.0 } else { rec_transit_sort(b) };
                let av = av as i64;
                let bv = bv as i64;
                av.cmp(&bv)
            }
            "total" => {
                let av = crate::traverse::total_seconds(a);
                let bv = crate::traverse::total_seconds(b);
                av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal)
            }
            "size" => {
                let av = a.size_bytes as f64;
                let bv = b.size_bytes as f64;
                av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal)
            }
            _ => std::cmp::Ordering::Equal,
        };
        if reverse { cmp.reverse() } else { cmp }
    });
}

fn rec_transit_sort(rec: &ObjectRecord) -> f64 {
    rec.transit_sort
}

/// Build the pager HTML.
pub fn render_pager(
    page: usize,
    pages: usize,
    total: usize,
    sort_key: &str,
    direction: &str,
    page_size: usize,
) -> String {
    if page_size == 0 {
        return format!("<span>{total} objects (paging disabled)</span>");
    }

    let pager_url = |page: usize| -> String {
        format!(
            "?sort={sort_key}&dir={direction}&page={page}&page_size={page_size}"
        )
    };

    let mut parts = Vec::new();

    if page > 1 {
        parts.push(format!(
            "<a href=\"{}\">&laquo; Prev</a>",
            pager_url(page - 1)
        ));
    } else {
        parts.push("<span class=\"disabled\">&laquo; Prev</span>".to_string());
    }

    // Page numbers: 1, current-1, current, current+1, last
    let mut nums: Vec<usize> = vec![1, pages, page.saturating_sub(1), page, page + 1];
    nums.retain(|n| *n >= 1 && *n <= pages);
    nums.sort();
    nums.dedup();

    let mut prev = 0;
    for n in nums {
        if n - prev > 1 {
            parts.push("…".to_string());
        }
        if n == page {
            parts.push(format!("<span class=\"cur\">{n}</span>"));
        } else {
            parts.push(format!("<a href=\"{}\">{n}</a>", pager_url(n)));
        }
        prev = n;
    }

    if page < pages {
        parts.push(format!(
            "<a href=\"{}\">Next &raquo;</a>",
            pager_url(page + 1)
        ));
    } else {
        parts.push("<span class=\"disabled\">Next &raquo;</span>".to_string());
    }

    parts.push(format!(
        "<span>Page {page} of {pages} &mdash; {total} objects</span>"
    ));

    parts.join(" ")
}

/// Build the page-size select control.
pub fn page_size_control(
    page_size: usize,
    sort_key: &str,
    direction: &str,
) -> String {
    let options = [(10usize, "10"), (20, "20"), (50, "50"), (100, "100"), (0, "All (no paging)")];
    let opts: Vec<String> = options
        .iter()
        .map(|(v, label)| {
            let selected = if *v == page_size { " selected" } else { "" };
            format!("<option value=\"{v}\"{selected}>{label}</option>")
        })
        .collect();

    let base = format!(
        "?sort={sort_key}&dir={direction}&page=1"
    );

    format!(
        "<span class=\"page-size\">Page size: \
         <select onchange=\"window.location.href='{}&page_size='+this.value\">{}</select></span>",
        base,
        opts.join("")
    )
}

/// Build sortable header cells.
pub fn render_header_cells(
    sort_key: &str,
    direction: &str,
    page_size: usize,
) -> String {
    let mut cells = Vec::new();

    for col in COLUMNS {
        if col.sortable {
            let arrow = if col.key == sort_key {
                if direction == "asc" {
                    "&#8593;"
                } else {
                    "&#8595;"
                }
            } else {
                "&#8597;"
            };

            let other_dir = if direction == "asc" { "desc" } else { "asc" };
            let url = if col.key == sort_key {
                format!(
                    "?sort={}&dir={}&page=1&page_size={}",
                    col.key, other_dir, page_size
                )
            } else {
                format!(
                    "?sort={}&dir={}&page=1&page_size={}",
                    col.key, col.default_dir, page_size
                )
            };

            cells.push(format!(
                "<th><a href=\"{}\">{}<span class=\"arrow\">{arrow}</span></a></th>",
                url,
                html_escape(col.name)
            ));
        } else {
            cells.push(format!(
                "<th>{}</th>",
                html_escape(col.name)
            ));
        }
    }

    cells.join("\n")
}

/// Build the full index page HTML.
pub fn render_index_page(
    root: &str,
    location: &str,
    total: usize,
    generated: &str,
    telescope_options: &str,
    flashes: &str,
    refresh_url: &str,
    header_cells: &str,
    rows: &str,
    pager: &str,
    // Pre-rendered nav fragment; empty when staging review is disabled.
    staging_nav: &str,
) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Astro Imaging Tracker</title>
<style>{css}</style>
</head>
<body>
<h1>Astro Imaging Tracker</h1>
<p class="meta">Root: {root} &middot; Location: {location} &middot; Objects: {n} &middot; Generated: {gen}<br>
Click a column header to sort. <a href="{refresh_url}">Refresh scan</a> &middot; <a href="/compendium">Compendium</a>{staging_nav}</p>

<div class="addbox">
  <h2>Add a new object:</h2>
  <form method="post" action="/add">
    <label for="telescope">Telescope:</label>
    <select name="telescope" id="telescope" required>
      <option value="" disabled selected>— choose —</option>
      {telescope_options}
    </select>
    <label for="object">Object Name:</label>
    <input type="text" name="object" id="object" placeholder="e.g. M31" required>
    <label for="constellation">Constellation:</label>
    <input type="text" name="constellation" id="constellation" placeholder="e.g. Andromeda">
    <label for="ra">RA:</label>
    <input type="text" name="ra" id="ra" placeholder="e.g. 00h42m44s">
    <label for="dec">DEC:</label>
    <input type="text" name="dec" id="dec" placeholder="e.g. +41°16′09″">
    <label for="rotation">Rotation:</label>
    <input type="number" name="rotation" id="rotation" placeholder="e.g. 45" step="any">
    <label for="sample">Link:</label>
    <input type="text" name="sample" id="sample" placeholder="e.g. https://app.astrobin.com/...">
    <button type="button" id="populate-btn">Populate from Astrobin</button>
    <button type="submit">Add</button>
  </form>
  <script>
  document.getElementById('populate-btn').addEventListener('click', function() {{
    var link = document.getElementById('sample').value.trim();
    if (!link) {{ alert('Please enter an AstroBin link first.'); return; }}
    var slug = link.split('/').filter(Boolean).pop();
    if (!slug) {{ alert('Could not extract slug from link.'); return; }}
    this.disabled = true; this.textContent = 'Loading...';
    fetch('/astrobin/' + encodeURIComponent(slug))
      .then(function(r) {{ return r.json().then(function(d) {{ return {{ok: r.ok, data: d}}; }}); }})
      .then(function(res) {{
        if (!res.ok) throw new Error(res.data.error || 'Lookup failed');
        var d = res.data;
        if (d.name) document.getElementById('object').value = d.name;
        if (d.constellation) document.getElementById('constellation').value = d.constellation;
        if (d.ra) document.getElementById('ra').value = d.ra;
        if (d.dec) document.getElementById('dec').value = d.dec;
      }})
      .catch(function(e) {{ alert('AstroBin lookup failed: ' + e.message); }})
      .finally(function() {{ document.getElementById('populate-btn').disabled = false; document.getElementById('populate-btn').textContent = 'Populate from Astrobin'; }});
  }});
  </script>
  {flashes}
</div>

<p class="pager">{pager}</p>

<table id="report">
<thead>
<tr>
{header_cells}
</tr>
</thead>
<tbody>
{rows}
</tbody>
</table>

<p class="pager">{pager}</p>
</body>
</html>
"#,
        css = CSS,
        root = html_escape(root),
        n = total,
        gen = html_escape(generated),
        refresh_url = html_escape(refresh_url),
        telescope_options = telescope_options,
        flashes = flashes,
        pager = pager,
        header_cells = header_cells,
        rows = rows,
        staging_nav = staging_nav,
    )
}

/// Build the edit page HTML.
pub fn render_edit_page(
    telescope: &str,
    name: &str,
    plan_path: &str,
    content: &str,
    flashes: &str,
) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Astro Imaging Tracker</title>
<style>{css}</style>
</head>
<body>
{flashes}
<h1>Edit plan: {telescope}/{name}</h1>
<p class="meta">{plan_path}</p>
<form method="post" action="/edit/{telescope}/{name}">
  <textarea name="plan" class="plan" rows="24" cols="100" spellcheck="false">{content}</textarea>
  <p>
    <button type="submit">Save</button>
    <a href="/" class="cancel">Cancel</a>
  </p>
</form>
</body>
</html>
"#,
        css = CSS,
        telescope = html_escape(telescope),
        name = html_escape(name),
        plan_path = html_escape(plan_path),
        content = html_escape(content),
        flashes = flashes,
    )
}

// --- Staging review ---------------------------------------------------------

/// Landing page listing every staging directory that holds frames.
pub fn render_staging_index(staging_root: &str, dirs: &[crate::staging::StagingDir]) -> String {
    let mut rows = String::new();
    if dirs.is_empty() {
        rows = r#"<tr><td colspan="4" class="meta">No directories containing FITS frames.</td></tr>"#.to_string();
    }
    for d in dirs {
        // Percent-encode for the query string (see DIR_QUERY).
        let q = percent_encode(d.path.as_bytes(), DIR_QUERY).to_string();
        // Raw numbers ride along on the row: sorting on the display string
        // would order "15.7 GB" below "632 MB".
        let bytes = scan_dir_bytes(&d.path);
        rows.push_str(&format!(
            "<tr data-obj=\"{objq}\" data-tel=\"{telq}\" data-frames=\"{n}\" data-bytes=\"{bytes}\"><td><a href=\"/staging/review?dir={q}\">{obj}</a></td><td>{tel}</td><td class=\"num\">{n}</td><td class=\"num\">{sz}</td></tr>\n",
            q = crate::plan::html_escape(&q),
            obj = crate::plan::html_escape(&d.name),
            tel = crate::plan::html_escape(&d.telescope),
            objq = crate::plan::html_escape(&d.name),
            telq = crate::plan::html_escape(&d.telescope),
            n = d.frames,
            bytes = bytes,
            sz = human_bytes(bytes),
        ));
    }
    let html = r####"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Staging review</title>
<style>__CSS__</style>
</head>
<body>
<h1>Staging review</h1>
<p class="meta">Staging root: __ROOT__ &middot; <a href="/">Inventory</a> &middot; <a href="/compendium">Compendium</a></p>
<p class="meta">Pick <strong>one</strong> object directory. The review never touches the whole staging area.</p>
<table id="dirs">
<thead><tr>
<th class="sortable" data-key="obj">Object<span class="arrow"></span></th>
<th class="sortable" data-key="tel">Telescope<span class="arrow"></span></th>
<th class="sortable num" data-key="frames">Frames<span class="arrow"></span></th>
<th class="sortable num" data-key="size">Size<span class="arrow"></span></th>
</tr></thead>
<tbody>
__ROWS__
</tbody>
</table>
<script>
"use strict";
// Client-side sort: the table is a dozen rows and the byte totals are already
// computed server-side, so a round trip per click would re-walk every staging
// directory for no gain. Default order is Object A-Z, matching the server order.
(function () {
  const tb = document.querySelector("#dirs tbody");
  const ths = Array.from(document.querySelectorAll("th.sortable"));
  let key = "obj", dir = 1;
  const NUM = { frames: "frames", size: "bytes" };
  function apply() {
    const rows = Array.from(tb.querySelectorAll("tr"));
    rows.sort((a, b) => {
      const col = NUM[key];
      if (col) return (+a.dataset[col] - +b.dataset[col]) * dir;
      return (a.dataset[key] || "").localeCompare(b.dataset[key] || "") * dir;
    });
    for (const r of rows) tb.appendChild(r);
    for (const th of ths)
      th.querySelector(".arrow").textContent =
        th.dataset.key === key ? (dir === 1 ? "\u25b2" : "\u25bc") : "";
  }
  for (const th of ths) {
    th.addEventListener("click", () => {
      const k = th.dataset.key;
      if (k === key) dir = -dir;
      else { key = k; dir = NUM[k] ? -1 : 1; } // counts and sizes read best high-first
      apply();
    });
  }
  apply();
})();
</script>
</body>
</html>"####;
    html.replace("__CSS__", CSS)
        .replace("__ROOT__", &crate::plan::html_escape(staging_root))
        .replace("__ROWS__", &rows)
}

fn scan_dir_bytes(path: &str) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![std::path::PathBuf::from(path)];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(m) = std::fs::metadata(&p) {
                total += m.len();
            }
        }
    }
    total
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// The keyboard-driven culling page. All data arrives from `/staging/manifest`.
///
/// Marks live in `localStorage`, keyed by staging directory: deletes are
/// permanent, so a reload, a discarded tab, or a crash must not lose them.
pub fn render_staging_review(dir: &str) -> String {
    let html = r####"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Staging review</title>
<style>
__CSS__
body { margin: 0; background: #111; color: #ddd; }
#bar { position: sticky; top: 0; background: #1b1b1b; border-bottom: 1px solid #333;
       padding: 8px 12px; display: flex; gap: 12px; align-items: center;
       flex-wrap: wrap; z-index: 5; font-size: 13px; }
#bar a { color: #8cf; text-decoration: none; }
#dir { font-weight: 600; word-break: break-all; }
.badge { padding: 2px 8px; border-radius: 3px; font-size: 12px; font-weight: 700; }
.b-push { background: #2c6e49; color: #fff; }
.b-del  { background: #c0392b; color: #fff; }
.b-none { background: #333; color: #888; }
#stage { text-align: center; padding: 10px 12px 24px; }
#img { max-width: 100%; max-height: 76vh; background: #000; border: 1px solid #333; }
#meta { font-size: 12px; color: #999; margin-top: 8px; word-break: break-all; }
#meta .warn { color: #fc6; }
#grp { color: #8cf; font-weight: 600; }
#overlay { position: fixed; inset: 0; background: #111; display: none;
           overflow: auto; padding: 20px 24px; z-index: 10; }
#overlay.show { display: block; }
table { border-collapse: collapse; width: 100%; font-size: 12px; margin: 8px 0 16px; }
td, th { padding: 4px 8px; border-bottom: 1px solid #2a2a2a; text-align: left; }
td.num { text-align: right; }
.col { color: #f88; }
button { padding: 6px 14px; background: #2c6e49; border: 0; color: #fff;
         cursor: pointer; border-radius: 3px; font-size: 13px; }
button:disabled { background: #333; color: #777; cursor: default; }
button.plain { background: #333; }
button.danger { background: #c0392b; }
select { background: #222; color: #ddd; border: 1px solid #444; padding: 4px; max-width: 320px; }
#newname { background: #222; color: #ddd; border: 1px solid #444; padding: 4px; }
#hint { color: #777; }
#load { color: #9ab; padding: 30px; }
</style>
</head>
<body data-dir="__DIR__">
<div id="bar">
  <a href="/staging">&larr; staging</a>
  <span id="dir">__DIR__</span>
  <span id="counter"></span>
  <span id="badge" class="badge b-push">PUSH</span>
  <span id="grp"></span>
  <label>destination <select id="dest"></select></label>
  <span id="newrow" style="display:none">
    <select id="newtel"></select>
    <input id="newname" type="text" size="18" placeholder="object name">
    <button id="btnNew">Create</button>
    <button id="btnNewCancel" class="plain">Cancel</button>
    <span id="newmsg" class="col" style="display:none"></span>
  </span>
  <label>sort <select id="sort">
    <option value="sig">&sigma; (lower = cleaner)</option>
    <option value="q">sky/&sigma;</option>
    <option value="med">median</option>
  </select></label>
  <button id="btnSummary">Summary (G)</button>
  <span id="hint">everything pushes by default &middot; z delete + next &middot; Space skip &middot; &larr;/&rarr; move (hold to scroll) &middot; U reset &middot; G summary</span>
</div>
<div id="stage">
  <div id="load">Scanning&hellip;</div>
  <img id="img" alt="" style="display:none">
  <div id="meta"></div>
</div>
<div id="overlay">
  <h2>Summary</h2>
  <div id="sum"></div>
  <button id="btnApply" disabled>Apply</button>
  <button id="btnClose" class="plain">Close (Esc)</button>
  <div id="result"></div>
</div>
<script>
"use strict";
const DIR = document.body.dataset.dir;
const $ = (id) => document.getElementById(id);

// Marks are persisted: deletes are permanent, so losing them to a reload is
// worse than a stale mark. Keyed by staging dir so two tabs never cross over.
//
// Push is the DEFAULT: a frame is only an exception if it is marked "del"
// (delete) or "off" (skip). Storing just the exceptions means a 740-frame
// directory needs zero marks, and the culling work becomes "flag the duds".
const LKEY = "stagemarks2:" + DIR;
let marks = {};
try {
  const old = JSON.parse(localStorage.getItem(LKEY) || "{}");
  for (const k in old) if (old[k] === "del" || old[k] === "off") marks[k] = old[k];
} catch (e) { marks = {}; }
const saveMarks = () => localStorage.setItem(LKEY, JSON.stringify(marks));
const kind = (f) => marks[f.path] || "push";
const isPush = (f) => kind(f) === "push";

let manifest = { groups: [] };
let frames = [];
let i = 0;
let lastNav = 0;   // throttle for held arrow keys
let dest = "";
let destFiles = new Set();
const NEW = "__new__";   // sentinel for the "new destination" option, never a real path

const collides = (f) => destFiles.has(f.name);

function flat() {
  const mode = $("sort").value;
  frames = [];
  for (const g of manifest.groups) {
    const fs = g.frames.slice();
    if (mode === "med") fs.sort((a, b) => b.median - a.median);
    else if (mode === "sig") fs.sort((a, b) => a.sigma - b.sigma);
    else fs.sort((a, b) => b.quality - a.quality);
    const label = (g.imagetype || "?") + " \u00b7 " + (g.filter || "?") + " (" + fs.length + ")";
    for (const f of fs) { f.grp = label; frames.push(f); }
  }
  if (i >= frames.length) i = 0;
}

const src = (f) => "/staging/preview?path=" + encodeURIComponent(f.path);

function setBadge(m) {
  const b = $("badge");
  b.className = "badge " + (m === "del" ? "b-del" : m === "off" ? "b-none" : "b-push");
  b.textContent = m === "del" ? "DELETE"
              : m === "off" ? "SKIPPED \u2014 will not push"
              : "PUSH \u2192 " + dest.split("/").slice(-2).join("/");
}

function show() {
  if (!frames.length) {
    $("img").style.display = "none";
    $("load").style.display = "";
    $("load").textContent = "No images here.";
    $("counter").textContent = ""; $("grp").textContent = ""; $("meta").textContent = "";
    return;
  }
  const f = frames[i];
  $("load").style.display = "none";
  $("img").style.display = "";
  $("img").src = src(f);
  $("counter").textContent = (i + 1) + " / " + frames.length;
  $("grp").textContent = f.grp;
  setBadge(kind(f));
  const warn = isPush(f) && collides(f)
    ? ' <span class="warn">\u26a0 name already exists in the destination</span>' : "";
  $("meta").innerHTML = f.name + " &middot; " + f.exptime + "s &middot; "
    + f.width + "\u00d7" + f.height + " &middot; med " + f.median.toFixed(1)
    + " &middot; \u03c3 " + f.sigma.toFixed(2) + " &middot; sky/\u03c3 " + f.quality.toFixed(1) + warn;
  // Warm the browser cache for the next frame so the slideshow stays instant.
  if (frames[i + 1]) new Image().src = src(frames[i + 1]);
}

// Space now EXCLUDES a frame and advances. With push as the default the
// culling gesture is "skip the duds": one key, no look-back.
function toggleSkip() {
  const f = frames[i]; if (!f) return;
  if (marks[f.path] === "off") delete marks[f.path]; else marks[f.path] = "off";
  saveMarks(); setBadge(kind(f));
  if (i < frames.length - 1) { i++; show(); }
}
function toggleDel() {
  const f = frames[i]; if (!f) return;
  if (marks[f.path] === "del") delete marks[f.path]; else marks[f.path] = "del";
  saveMarks(); setBadge(kind(f));
  // Delete advances too, so flagging a dud and moving on is one gesture, like
  // Space. The toggle still works: navigate left and press z again.
  if (i < frames.length - 1) { i++; show(); }
}
function clearMark() {
  const f = frames[i]; if (!f) return;
  delete marks[f.path]; saveMarks(); setBadge("push");
}

function marked() {
  const push = [], del = [], skip = [];
  for (const f of frames) {
    if (marks[f.path] === "del") del.push(f);
    else if (marks[f.path] === "off") skip.push(f);
    else push.push(f);
  }
  return { push, del, skip };
}

function renderSummary() {
  const { push, del, skip } = marked();
  const clash = push.filter(collides);
  let h = "";
  h += "<h3>Move <b>" + push.length + "</b> of " + frames.length + " file"
     + (frames.length === 1 ? "" : "s") + " \u2192 "
     + (dest || "<span class=col>no destination chosen</span>") + "</h3>";
  if (clash.length)
    h += "<p class=col>\u26a0 " + clash.length + " file(s) would overwrite an existing "
       + "name in the destination. Those will be reported as failures, never overwritten.</p>";
  // Only the exceptions are listed: with push as the default a whole directory
  // would otherwise render a 740-row table nobody reads.
  if (skip.length) {
    h += "<h3>Skip " + skip.length + " file" + (skip.length === 1 ? "" : "s") + " (left in staging)</h3>";
    h += "<table><tr><th>file</th><th>sky/\u03c3</th></tr>";
    for (const f of skip)
      h += "<tr><td>" + f.name + "</td><td class=num>" + f.quality.toFixed(1) + "</td></tr>";
    h += "</table>";
  }
  if (del.length) {
    h += "<h3>Delete " + del.length + " file" + (del.length === 1 ? "" : "s")
       + " <span class=col>(permanently \u2014 staging is Syncthing-shared, this propagates)</span></h3>";
    h += "<table><tr><th>file</th><th>sky/\u03c3</th></tr>";
    for (const f of del)
      h += "<tr><td>" + f.name + "</td><td class=num>" + f.quality.toFixed(1) + "</td></tr>";
    h += "</table>";
  }
  if (!skip.length && !del.length) h += "<p>No exceptions: every frame in this directory will be moved.</p>";
  $("sum").innerHTML = h;
  $("result").textContent = "";
  const btn = $("btnApply");
  btn.disabled = !push.length && !del.length;
  btn.className = del.length ? "danger" : "";
  btn.textContent = "Apply";
  btn.dataset.armed = "";
}

async function applyAll() {
  const btn = $("btnApply");
  // Two-step confirm: deletes are permanent and propagate over Syncthing.
  if (btn.dataset.armed !== "1") {
    btn.dataset.armed = "1";
    btn.textContent = "Confirm \u2014 this cannot be undone";
    return;
  }
  const { push, del } = marked();
  if (push.length && !dest) { $("result").textContent = "Choose a destination first."; return; }
  btn.disabled = true;
  const body = {
    dir: DIR,
    pushes: push.map((f) => ({ src: f.path, dst: dest })),
    deletes: del.map((f) => f.path),
  };
  let rep;
  try {
    const r = await fetch("/staging/apply", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    rep = await r.json();
    if (typeof rep !== "object") rep = { pushed: 0, deleted: 0, errors: [String(rep)] };
  } catch (e) {
    $("result").textContent = "Apply failed: " + e;
    btn.disabled = false;
    return;
  }
  for (const f of push.concat(del)) delete marks[f.path];
  saveMarks();
  let h = "<p>Pushed " + rep.pushed + ", deleted " + rep.deleted + ".</p>";
  if (rep.errors && rep.errors.length) {
    h += "<p class=col>" + rep.errors.length + " problem(s), not rolled back:</p><ul>";
    for (const e of rep.errors) h += "<li class=col>" + e + "</li>";
    h += "</ul>";
  }
  $("result").innerHTML = h;
  // Re-fetch the now-drained directory rather than trusting local state.
  const m = await (await fetch("/staging/manifest?dir=" + encodeURIComponent(DIR))).json();
  manifest = m; flat(); show();
  await onDest();
  btn.disabled = false;
}

async function onDest() {
  const v = $("dest").value;
  // The sentinel is a UI affordance, not a destination: never persist it and
  // never let it become the push target.
  if (v === NEW) { setBadge(frames[i] ? kind(frames[i]) : "push"); return; }
  dest = v;
  localStorage.setItem("stagedest:" + DIR, dest);
  destFiles = new Set();
  if (dest) {
    try {
      const r = await fetch("/staging/destfiles?dir=" + encodeURIComponent(dest));
      if (r.ok) for (const n of await r.json()) destFiles.add(n);
    } catch (e) { /* collision pre-check is advisory only */ }
  }
  setBadge(frames[i] ? kind(frames[i]) : "push");
  show();
}

async function loadDests() {
  const r = await fetch("/staging/destinations");
  if (!r.ok) return;
  const ds = await r.json();
  const sel = $("dest");
  const keep = sel.value;
  sel.innerHTML = "";
  for (const d of ds) { const o = document.createElement("option"); o.value = d.path; o.textContent = d.label; sel.appendChild(o); }
  const o = document.createElement("option");
  o.value = NEW; o.textContent = "\uFF0B new destination\u2026";
  sel.appendChild(o);
  // Telescope choices are the CCD prefixes already in the list: creating a
  // whole new telescope is a separate decision and stays on the index form.
  const ts = $("newtel");
  ts.innerHTML = "";
  for (const t of [...new Set(ds.map((d) => d.label.split("/")[0]))].sort()) {
    const x = document.createElement("option"); x.value = t; x.textContent = t; ts.appendChild(x);
  }
  // Fuzzy default: the staging object basename against CCD object basenames.
  const base = DIR.split("/").filter(Boolean).pop().toLowerCase();
  let guess = ds.find((d) => d.name.toLowerCase() === base)
           || ds.find((d) => d.name.toLowerCase().includes(base) || base.includes(d.name.toLowerCase()));
  const saved = localStorage.getItem("stagedest:" + DIR);
  const pick = ds.find((d) => d.path === saved) || ds.find((d) => d.path === keep) || guess;
  if (pick) sel.value = pick.path;
  await onDest();
}

function showNewRow() {
  if (!$("newname").dataset.touched) $("newname").value = DIR.split("/").filter(Boolean).pop();
  $("newmsg").style.display = "none";
  $("newrow").style.display = "";
  $("newname").focus();
  $("newname").select();
}

async function createDest() {
  const tel = $("newtel").value, name = $("newname").value.trim();
  const msg = $("newmsg");
  if (!tel || !name) { msg.textContent = "Choose a telescope and enter a name."; msg.style.display = ""; return; }
  const btn = $("btnNew");
  btn.disabled = true;
  try {
    const r = await fetch("/staging/newdest", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ telescope: tel, name }),
    });
    if (!r.ok) {
      // The handler's error body is plain text, not JSON.
      msg.textContent = await r.text();
      msg.style.display = "";
      return;
    }
    const j = await r.json();
    $("newrow").style.display = "none";
    $("newname").dataset.touched = "";
    await loadDests();
    const sel = $("dest");
    if (Array.from(sel.options).some((x) => x.value === j.path)) sel.value = j.path;
    await onDest();
    sel.blur();
  } catch (e) {
    msg.textContent = "create failed: " + e;
    msg.style.display = "";
  } finally {
    btn.disabled = false;
  }
}

document.addEventListener("keydown", (e) => {
  const t = e.target;
  if (t && (t.tagName === "INPUT" || t.tagName === "SELECT" || t.tagName === "TEXTAREA")) {
    if (e.key === "Escape") t.blur();
    return;
  }
  // Marks must never machine-gun, but browsing should: arrows keep autorepeat
  // so holding one scrolls the set, throttled so the OS repeat rate cannot
  // outrun image loading (a cold preview is ~270 ms and renders are serialised).
  const NAV = ["ArrowRight", "l", "L", "ArrowLeft", "h", "H"];
  if (!NAV.includes(e.key) && e.repeat) return;
  if (e.repeat && performance.now() - lastNav < 120) return;
  switch (e.key) {
    case " ": e.preventDefault(); toggleSkip(); break;
    case "z": case "Z": toggleDel(); break;
    case "ArrowRight": case "l": case "L": e.preventDefault(); lastNav = performance.now(); if (i < frames.length - 1) { i++; show(); } break;
    case "ArrowLeft": case "h": case "H": e.preventDefault(); lastNav = performance.now(); if (i > 0) { i--; show(); } break;
    case "u": case "U": clearMark(); break;
    case "g": case "G": renderSummary(); $("overlay").classList.add("show"); break;
    case "Escape": $("overlay").classList.remove("show"); break;
  }
});

$("sort").addEventListener("change", () => { flat(); show(); });
$("dest").addEventListener("change", (e) => {
  if (e.target.value === NEW) { e.target.value = dest; showNewRow(); return; }
  onDest();
  e.target.blur();
});
$("btnNew").addEventListener("click", createDest);
$("btnNewCancel").addEventListener("click", () => {
  $("newrow").style.display = "none";
  $("dest").blur();
});
$("newname").addEventListener("input", () => { $("newname").dataset.touched = "1"; });
$("newname").addEventListener("keydown", (e) => {
  if (e.key === "Enter") { e.preventDefault(); createDest(); }
});
$("btnSummary").addEventListener("click", () => { renderSummary(); $("overlay").classList.add("show"); });
$("btnClose").addEventListener("click", () => $("overlay").classList.remove("show"));
$("btnApply").addEventListener("click", applyAll);

(async () => {
  const t0 = Date.now();
  $("load").textContent = "Scanning " + DIR + "\u2026";
  const tick = setInterval(() => {
    $("load").textContent = "Scanning " + DIR + "\u2026 " + ((Date.now() - t0) / 1000).toFixed(1) + "s";
  }, 250);
  try {
    const r = await fetch("/staging/manifest?dir=" + encodeURIComponent(DIR));
    if (!r.ok) { $("load").textContent = "Manifest failed: " + r.status; return; }
    manifest = await r.json();
  } finally {
    clearInterval(tick);
  }
  if (manifest.errors && manifest.errors.length) console.warn("scan errors", manifest.errors);
  flat();
  show();
  loadDests();
})();
</script>
</body>
</html>"####;
    html.replace("__CSS__", CSS).replace("__DIR__", &crate::plan::html_escape(dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::staging::StagingDir;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("astro_render_test_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Every column must be sortable, and sorting has to key off raw numbers:
    /// ordering by the display string puts "4.1 kB" below "64 B".
    #[test]
    fn staging_index_exposes_numeric_sort_keys_for_every_column() {
        let root = tmpdir("idx");
        let big = root.join("big");
        let small = root.join("small");
        std::fs::create_dir_all(&big).unwrap();
        std::fs::create_dir_all(&small).unwrap();
        std::fs::write(big.join("a.fits"), vec![b'x'; 4096]).unwrap();
        std::fs::write(small.join("b.fits"), vec![b'y'; 64]).unwrap();

        let dirs = vec![
            StagingDir {
                name: "big".into(),
                telescope: "Tel A".into(),
                path: big.to_string_lossy().into_owned(),
                frames: 1,
                bytes: 0,
            },
            StagingDir {
                name: "small".into(),
                telescope: "Tel B".into(),
                path: small.to_string_lossy().into_owned(),
                frames: 9,
                bytes: 0,
            },
        ];
        let html = render_staging_index(root.to_str().unwrap(), &dirs);

        for k in ["obj", "tel", "frames", "size"] {
            assert!(
                html.contains(&format!("data-key=\"{k}\"")),
                "column {k} must be sortable"
            );
        }
        assert!(html.contains("data-bytes=\"4096\""), "raw total missing");
        assert!(html.contains("data-bytes=\"64\""), "raw total missing");
        assert!(html.contains("data-frames=\"9\""), "frame count key missing");
        assert!(
            html.contains("data-obj=\"big\"") && html.contains("data-tel=\"Tel A\""),
            "string sort keys missing"
        );
        // Display text stays human-readable.
        assert!(html.contains("4.1 kB") && html.contains("64 B"), "display size wrong");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn staging_index_names_the_empty_case_without_breaking_the_table() {
        let html = render_staging_index("/nowhere", &[]);
        assert!(html.contains("No directories containing FITS frames"));
        assert!(html.contains("data-key=\"size\""), "headers still sortable");
    }
}
