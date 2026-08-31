#!/usr/bin/env python3
"""
Astronomy image inventory — Flask server.

Serves the report with server-side sorting and pagination (20 rows/page),
plus an "Add a new object" section that creates
/data/Astro/CCD/<telescope>/<object>/plan.md.

Run:
    .venv/bin/python astro_inventory_server.py
    -> http://localhost:5000
"""

import os
import re
import html
import json
import shutil
import threading
import urllib.request
from datetime import datetime
from urllib.parse import quote, urlencode

from flask import Flask, request, redirect, url_for, flash, get_flashed_messages, send_file, render_template_string, jsonify

from astro_inventory import ROOT, traverse, build_rows  # noqa: F401

app = Flask(__name__)
app.secret_key = os.urandom(24)

@app.after_request
def add_no_cache_headers(response):
    response.headers["Cache-Control"] = "no-cache, no-store, must-revalidate"
    response.headers["Pragma"] = "no-cache"
    response.headers["Expires"] = "0"
    return response

PAGE_SIZE = 20

# --- scan cache --------------------------------------------------------------

_cache_lock = threading.Lock()
_cache = {"records": None, "generated": None}


def get_records(force=False):
    with _cache_lock:
        if force or _cache["records"] is None:
            records = traverse(ROOT)
            _cache["records"] = records
            _cache["generated"] = datetime.now()
    return _cache["records"], _cache["generated"]


# --- sorting -----------------------------------------------------------------

def _total_seconds(rec):
    total = sum(d["seconds"] for d in rec["filters"].values())
    total += rec["cr2cr3_count"] * 30
    return total


# key -> (display name, sortable, sort_value_fn, default_dir)
COLUMNS = [
    ("latest",  "Latest image",          True,  lambda r: (r["latest"].timestamp() if r["latest"] else 0), "desc"),
    ("telescope", "Telescope",           True,  lambda r: r["telescope"], "asc"),
    ("object",  "Object",                True,  lambda r: r["object"], "asc"),
    ("transit", "Transit",               True,  lambda r: r.get("transit_sort") or 9999999999, "asc"),
    ("total",   "Total exposure",        True,  _total_seconds, "desc"),
    ("size",    "Size (MB)",             True,  lambda r: rec_size_mb(r), "desc"),
    ("filters", "Filters (count / duration / total)", False, None, None),
    ("project", "Project",               True,  lambda r: 1 if r["has_project"] else 0, "asc"),
    ("final",   "Final image",           True,  lambda r: 1 if r["has_final"] else 0, "asc"),
    ("masters", "Master images",         False, None, None),
    ("plan",    "Plan",                  False, None, None),
    ("actions", "Actions",               False, None, None),
]
COL_BY_KEY = {c[0]: c for c in COLUMNS}


def rec_size_mb(rec):
    return rec["size_bytes"] / (1024 * 1024)


def sort_records(records, key, direction):
    if key not in COL_BY_KEY or not COL_BY_KEY[key][1]:
        key = "latest"
    fn = COL_BY_KEY[key][3]
    reverse = direction == "desc"
    return sorted(records, key=fn, reverse=reverse)


# --- routes ------------------------------------------------------------------

@app.route("/")
def index():
    force = request.args.get("refresh") == "1"
    records, generated = get_records(force=force)
    telescopes = sorted({r["telescope"] for r in records})

    # --- sort ---
    sort_key = request.args.get("sort", "latest")
    if sort_key not in COL_BY_KEY or not COL_BY_KEY[sort_key][1]:
        sort_key = "latest"
    direction = request.args.get("dir", COL_BY_KEY[sort_key][4])
    if direction not in ("asc", "desc"):
        direction = "asc"
    records = sort_records(records, sort_key, direction)

    # --- pagination ---
    total = len(records)
    pages = max(1, (total + PAGE_SIZE - 1) // PAGE_SIZE)
    page = request.args.get("page", type=int) or 1
    page = max(1, min(page, pages))
    start = (page - 1) * PAGE_SIZE
    page_records = records[start:start + PAGE_SIZE]

    return render_page(
        page_records, generated, telescopes,
        total=total, page=page, pages=pages,
        sort_key=sort_key, direction=direction,
    )


@app.route("/img/<path:relpath>")
def image(relpath):
    # only serve files that live under ROOT
    full = os.path.realpath(os.path.join(ROOT, relpath))
    if not full.startswith(os.path.realpath(ROOT) + os.sep):
        return "Not found", 404
    if not os.path.isfile(full):
        return "Not found", 404
    return send_file(full)


def _resolve_object(telescope, name):
    """Validate telescope/object names and return the object dir path,
    or (None, error_message)."""
    if re.search(r"[\x00/\\]", telescope) or telescope in ("", ".", ".."):
        return None, "Invalid telescope name."
    if re.search(r"[\x00/\\]", name) or name in ("", ".", ".."):
        return None, "Invalid object name."
    tel_dir = os.path.join(ROOT, telescope)
    obj_dir = os.path.join(tel_dir, name)
    if not os.path.isdir(tel_dir):
        return None, f"Telescope directory not found: {telescope}"
    if not os.path.isdir(obj_dir):
        return None, f"Object directory not found: {telescope}/{name}"
    # make sure the resolved path stays under ROOT
    if not os.path.realpath(obj_dir).startswith(os.path.realpath(ROOT) + os.sep):
        return None, "Invalid path."
    return obj_dir, None


@app.route("/edit/<telescope>/<name>", methods=["GET", "POST"])
def edit_plan(telescope, name):
    obj_dir, err = _resolve_object(telescope, name)
    if err:
        flash(err, "error")
        return redirect(url_for("index"))
    plan_path = os.path.join(obj_dir, "plan.md")

    if request.method == "POST":
        content = request.form.get("plan", "")
        try:
            with open(plan_path, "w", encoding="utf-8") as f:
                f.write(content)
        except OSError as e:
            flash(f"Failed to save plan: {e}", "error")
            return redirect(url_for("index"))
        flash(f"Saved {telescope}/{name}/plan.md", "ok")
        get_records(force=True)
        return redirect(url_for("index"))

    try:
        with open(plan_path, "r", encoding="utf-8") as f:
            content = f.read()
    except (OSError, UnicodeDecodeError):
        content = ""

    body = f"""
<h1>Edit plan: {html.escape(telescope)}/{html.escape(name)}</h1>
<p class="meta">{html.escape(plan_path)}</p>
<form method="post" action="{url_for('edit_plan', telescope=telescope, name=name)}">
  <textarea name="plan" rows="24" cols="100" spellcheck="false">{html.escape(content)}</textarea>
  <p>
    <button type="submit">Save</button>
    <a href="{url_for('index')}" class="cancel">Cancel</a>
  </p>
</form>
"""
    return PAGE_SHELL + body


@app.route("/delete/<telescope>/<name>", methods=["POST"])
def delete_object(telescope, name):
    obj_dir, err = _resolve_object(telescope, name)
    if err:
        flash(err, "error")
        return redirect(url_for("index"))
    try:
        shutil.rmtree(obj_dir)
    except OSError as e:
        flash(f"Failed to delete {obj_dir}: {e}", "error")
        return redirect(url_for("index"))
    flash(f"Deleted {telescope}/{name}", "ok")
    get_records(force=True)
    return redirect(url_for("index"))


@app.route("/astrobin/<slug>")
def astrobin_lookup(slug):
    """Look up an AstroBin image by slug (hash) and return name/constellation/RA/DEC."""
    url = f"https://www.astrobin.com/api/v2/images/image/?hash={quote(slug)}"
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 (astro-inventory)"})
        with urllib.request.urlopen(req, timeout=15) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except Exception as e:
        return jsonify({"error": f"AstroBin API request failed: {e}"}), 502

    results = data.get("results") or []
    if not results:
        return jsonify({"error": f"No AstroBin image found for slug '{slug}'"}), 404

    img = results[0]
    sol = img.get("solution") or {}

    # Convert RA from decimal degrees to HMS
    ra_str = ""
    if sol.get("ra") is not None:
        ra_deg = float(sol["ra"]) % 360
        total_sec = ra_deg * 240  # seconds of time
        h = int(total_sec // 3600)
        m = int((total_sec % 3600) // 60)
        s = int(total_sec % 60)
        ra_str = f"{h:02d}h{m:02d}m{s:02d}s"

    # Convert DEC from decimal degrees to DMS
    dec_str = ""
    if sol.get("dec") is not None:
        dec_deg = float(sol["dec"])
        sign = "+" if dec_deg >= 0 else "-"
        dec_abs = abs(dec_deg)
        d = int(dec_abs)
        m = int((dec_abs - d) * 60)
        s = int(((dec_abs - d) * 60 - m) * 60)
        dec_str = f"{sign}{d}\u00b0{m:02d}\u2032{s:02d}\u2033"

    return jsonify({
        "name": img.get("title") or "",
        "constellation": img.get("constellation") or "",
        "ra": ra_str,
        "dec": dec_str,
    })


@app.route("/add", methods=["POST"])
def add_object():
    telescope = (request.form.get("telescope") or "").strip()
    name = (request.form.get("object") or "").strip()
    constellation = (request.form.get("constellation") or "").strip()
    ra = (request.form.get("ra") or "").strip()
    dec = (request.form.get("dec") or "").strip()
    rotation = (request.form.get("rotation") or "").strip()
    sample = (request.form.get("sample") or "").strip()

    if not telescope or not name:
        flash("Please choose a telescope and enter an object name.", "error")
        return redirect(url_for("index"))

    # only allow telescope names that actually exist in ROOT
    if not os.path.isdir(os.path.join(ROOT, telescope)):
        flash(f"Telescope directory not found: {telescope}", "error")
        return redirect(url_for("index"))

    # reject anything that is not a plain directory name
    if re.search(r"[\x00/\\]", name) or name in ("", ".", ".."):
        flash("Invalid object name.", "error")
        return redirect(url_for("index"))

    obj_dir = os.path.join(ROOT, telescope, name)
    if os.path.exists(obj_dir):
        flash(f"Object directory already exists: {telescope}/{name}", "error")
        return redirect(url_for("index"))

    plan_path = os.path.join(obj_dir, "plan.md")
    try:
        os.makedirs(obj_dir)
        with open(plan_path, "w", encoding="utf-8") as f:
            fm = []
            if constellation:
                fm.append(f"constellation: {constellation}")
            if ra:
                fm.append(f"ra: {ra}")
            if dec:
                fm.append(f"dec: {dec}")
            if rotation:
                fm.append(f"rotation: {rotation}")
            if sample:
                fm.append(f"link: {sample}")
            if fm:
                f.write("---\n")
                f.write("\n".join(fm) + "\n")
                f.write("---\n\n")
            f.write(f"# {name}\n\n## Plan\n")
    except OSError as e:
        flash(f"Failed to create {obj_dir}: {e}", "error")
        return redirect(url_for("index"))

    flash(f"Created {telescope}/{name}/plan.md", "ok")
    get_records(force=True)  # pick it up immediately
    return redirect(url_for("index"))


# --- page rendering ----------------------------------------------------------

PAGE_TEMPLATE = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Astro Capture Report</title>
<style>
  body {{ font-family: -apple-system, Segoe UI, Roboto, sans-serif; margin: 2em; }}
  table {{ border-collapse: collapse; width: 100%; font-size: 14px; }}
  th, td {{ border: 1px solid #ccc; padding: 6px 10px; text-align: left; }}
  th {{ background: #2b3a55; color: #fff; position: sticky; top: 0; user-select: none; }}
  th a {{ color: #fff; text-decoration: none; }}
  th .arrow {{ display: inline-block; font-size: 18px; font-weight: bold; margin-left: 6px; vertical-align: middle; }}
  tr:nth-child(even) {{ background: #f4f6fa; }}
  tr.red {{ background: #f8d7da !important; }}
  tr.yellow {{ background: #fff3cd !important; }}
  .meta {{ color: #666; margin-bottom: 1em; }}
  .thumbs img {{ max-height: 60px; max-width: 120px; margin: 2px; border: 1px solid #999; vertical-align: middle; }}
  .addbox {{ border: 1px solid #ccc; border-radius: 6px; padding: 1em 1.2em; margin-bottom: 1.5em; background: #f7f9fc; }}
  .addbox h2 {{ margin: 0 0 0.8em; font-size: 1.1em; }}
  .addbox form {{ display: flex; align-items: center; gap: 0.6em; flex-wrap: wrap; }}
  .addbox select, .addbox input[type=text] {{ padding: 6px 8px; font-size: 14px; }}
  .addbox input[type=text] {{ width: 260px; }}
  .addbox button {{ padding: 6px 16px; font-size: 14px; cursor: pointer; }}
  .flash {{ margin: 0.6em 0 0; padding: 0.5em 0.8em; border-radius: 4px; }}
  .flash.ok {{ background: #d4edda; color: #155724; }}
  .flash.error {{ background: #f8d7da; color: #721c24; }}
  .pager {{ margin: 0.8em 0; font-size: 14px; }}
  .pager a, .pager span.cur {{ padding: 3px 9px; margin: 0 2px; border: 1px solid #ccc; border-radius: 4px; text-decoration: none; color: #2b3a55; }}
  .pager a:hover {{ background: #e8edf5; }}
  .pager span.cur {{ background: #2b3a55; color: #fff; border-color: #2b3a55; }}
  .pager span.disabled {{ color: #aaa; border-color: #eee; }}
  .sep {{ color: #999; }}
  a.del {{ color: #b00020; }}
  textarea.plan {{ font-family: monospace; width: 100%; box-sizing: border-box; }}
  a.cancel {{ text-decoration: none; color: #2b3a55; }}
  button.danger {{ background: #b00020; color: #fff; border: none; padding: 6px 16px; cursor: pointer; }}
</style>
</head>
<body>
<h1>Astronomy Capture Report</h1>
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
"""


# minimal page shell for the edit/delete pages (same styles as the report)
PAGE_SHELL = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Astro Capture Report</title>
<style>
  body { font-family: -apple-system, Segoe UI, Roboto, sans-serif; margin: 2em; }
  .flash { margin: 0.6em 0; padding: 0.5em 0.8em; border-radius: 4px; }
  .flash.ok { background: #d4edda; color: #155724; }
  .flash.error { background: #f8d7da; color: #721c24; }
  .sep { color: #999; }
  a.del { color: #b00020; }
  textarea.plan { font-family: monospace; width: 100%; box-sizing: border-box; }
  a.cancel { text-decoration: none; color: #2b3a55; }
  button.danger { background: #b00020; color: #fff; border: none; padding: 6px 16px; cursor: pointer; }
</style>
</head>
<body>
"""


def _pager_url(page, sort_key, direction):
    params = {"sort": sort_key, "dir": direction, "page": page}
    return "?" + urlencode(params)


def render_pager(page, pages, total, sort_key, direction):
    parts = []
    if page > 1:
        parts.append(f'<a href="{_pager_url(page - 1, sort_key, direction)}">&laquo; Prev</a>')
    else:
        parts.append('<span class="disabled">&laquo; Prev</span>')

    # page numbers: 1 … around current … last
    nums = {1, pages, page - 1, page, page + 1}
    nums = sorted(n for n in nums if 1 <= n <= pages)
    prev = 0
    for n in nums:
        if n - prev > 1:
            parts.append("…")
        if n == page:
            parts.append(f'<span class="cur">{n}</span>')
        else:
            parts.append(f'<a href="{_pager_url(n, sort_key, direction)}">{n}</a>')
        prev = n

    if page < pages:
        parts.append(f'<a href="{_pager_url(page + 1, sort_key, direction)}">Next &raquo;</a>')
    else:
        parts.append('<span class="disabled">Next &raquo;</span>')
    parts.append(f"<span>Page {page} of {pages} &mdash; {total} objects</span>")
    return " ".join(parts)


def render_page(page_records, generated, telescopes, total, page, pages, sort_key, direction):
    options = "\n".join(
        f'<option value="{html.escape(t)}">{html.escape(t)}</option>' for t in telescopes
    )
    flashes = "".join(
        f'<div class="flash {html.escape(cat)}">{html.escape(msg)}</div>'
        for cat, msg in get_flashed_messages(with_categories=True)
    )

    def image_url(path):
        # path is under ROOT: /data/Astro/CCD/<telescope>/<object>/master/<file>
        return f"/img/{quote(os.path.relpath(path, ROOT))}"

    def actions_url(tel, name):
        return (
            url_for("edit_plan", telescope=tel, name=name),
            url_for("delete_object", telescope=tel, name=name),
        )

    # --- sortable header cells ---
    header_cells = []
    for key, name, sortable, _fn, _def_dir in COLUMNS:
        arrow = ""
        if sortable:
            if key == sort_key:
                arrow = "&#8593;" if direction == "asc" else "&#8595;"
            else:
                arrow = "&#8597;"
            other_dir = "desc" if direction == "asc" else "asc"
            if key == sort_key:
                url = _pager_url(1, key, other_dir)
            else:
                url = _pager_url(1, key, COL_BY_KEY[key][4])
            header_cells.append(
                f'<th><a href="{url}">{html.escape(name)}<span class="arrow">{arrow}</span></a></th>'
            )
        else:
            header_cells.append(f'<th>{html.escape(name)}</th>')

    pager = render_pager(page, pages, total, sort_key, direction)

    return PAGE_TEMPLATE.format(
        root=html.escape(ROOT),
        n=total,
        gen=generated.strftime("%Y-%m-%d %H:%M:%S"),
        telescope_options=options,
        flashes=flashes,
        refresh_url=_pager_url(1, sort_key, direction) + "&refresh=1",
        header_cells="\n".join(header_cells),
        rows=build_rows(page_records, image_url=image_url, actions_url=actions_url),
        pager=pager,
    )


if __name__ == "__main__":
    app.run(host="127.0.0.1", port=5000, debug=False)
