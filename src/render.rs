use crate::plan::html_escape;
use crate::traverse::ObjectRecord;

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
    total: usize,
    generated: &str,
    telescope_options: &str,
    flashes: &str,
    refresh_url: &str,
    header_cells: &str,
    rows: &str,
    pager: &str,
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
<p class="meta">Root: {root} &middot; Objects: {n} &middot; Generated: {gen}<br>
Click a column header to sort. <a href="{refresh_url}">Refresh scan</a></p>

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
