#!/usr/bin/env python3
"""
Astronomy image inventory — Flask server.

Serves the same report as astro_inventory.py, plus an "Add a new object"
section that creates /data/Astro/CCD/<telescope>/<object>/plan.md.

Run:
    .venv/bin/python astro_inventory_server.py
    -> http://localhost:5000
"""

import os
import re
import html
import threading
from datetime import datetime
from urllib.parse import quote

from flask import Flask, request, redirect, url_for, flash

from astro_inventory import ROOT, traverse, build_rows, HTML_TEMPLATE, fmt_total  # noqa: F401

app = Flask(__name__)
app.secret_key = os.urandom(24)

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


# --- routes ------------------------------------------------------------------

@app.route("/")
def index():
    force = request.args.get("refresh") == "1"
    records, generated = get_records(force=force)
    telescopes = sorted({r["telescope"] for r in records})
    return render_page(records, generated, telescopes)


@app.route("/img/<path:relpath>")
def image(relpath):
    # only serve files that live under ROOT
    full = os.path.realpath(os.path.join(ROOT, relpath))
    if not full.startswith(os.path.realpath(ROOT) + os.sep):
        return "Not found", 404
    if not os.path.isfile(full):
        return "Not found", 404
    from flask import send_file
    return send_file(full)


@app.route("/add", methods=["POST"])
def add_object():
    telescope = (request.form.get("telescope") or "").strip()
    name = (request.form.get("object") or "").strip()

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
            f.write(f"# {name}\n\n")
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
  th {{ background: #2b3a55; color: #fff; cursor: pointer; position: sticky; top: 0; user-select: none; }}
  th:hover {{ background: #3d5177; }}
  th .arrow {{ display: inline-block; width: 14px; font-size: 11px; opacity: 0.6; }}
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
</style>
</head>
<body>
<h1>Astronomy Capture Report</h1>
<p class="meta">Root: {root} &middot; Objects: {n} &middot; Generated: {gen}<br>
Click a column header to sort (click again to reverse). Default: Latest image, descending.
<a href="?refresh=1">Refresh scan</a></p>

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
    <button type="submit">Add</button>
  </form>
  {flashes}
</div>

<table id="report">
<thead>
<tr>
  <th data-type="ts">Latest image<span class="arrow"></span></th>
  <th data-type="str">Telescope<span class="arrow"></span></th>
  <th data-type="str">Object<span class="arrow"></span></th>
  <th data-type="num">Total exposure<span class="arrow"></span></th>
  <th data-type="num">Size (MB)<span class="arrow"></span></th>
  <th data-type="str">Filters (count / duration / total)<span class="arrow"></span></th>
  <th data-type="bool">Project<span class="arrow"></span></th>
  <th data-type="bool">Final image<span class="arrow"></span></th>
  <th data-type="str">Master images<span class="arrow"></span></th>
  <th data-type="str">Plan<span class="arrow"></span></th>
</tr>
</thead>
<tbody>
{rows}
</tbody>
</table>
<script>
const table = document.getElementById('report');
const tbody = table.tBodies[0];
const headers = table.tHead.rows[0].cells;

function sortTable(colIdx, dir) {{
  const type = headers[colIdx].dataset.type;
  const rows = Array.from(tbody.rows);
  rows.sort((a, b) => {{
    let va = a.cells[colIdx].dataset.sort, vb = b.cells[colIdx].dataset.sort;
    if (type === 'num' || type === 'ts') {{
      va = parseFloat(va) || 0; vb = parseFloat(vb) || 0;
      return dir * (va - vb);
    }}
    if (type === 'bool') {{
      va = va === '1' ? 1 : 0; vb = vb === '1' ? 1 : 0;
      return dir * (va - vb);
    }}
    return dir * va.localeCompare(vb);
  }});
  rows.forEach(r => tbody.appendChild(r));
  for (let i = 0; i < headers.length; i++) {{
    headers[i].removeAttribute('data-sorted');
    headers[i].querySelector('.arrow').textContent = '\\u2195';
  }}
  headers[colIdx].dataset.sorted = dir > 0 ? 'asc' : 'desc';
  headers[colIdx].querySelector('.arrow').textContent = dir > 0 ? '\\u2191' : '\\u2193';
}}

for (let i = 0; i < headers.length; i++) {{
  headers[i].addEventListener('click', () => {{
    const cur = headers[i].dataset.sorted;
    const dir = (i === 0 && !cur) ? -1 : (cur === 'asc' ? -1 : 1);
    sortTable(i, dir);
  }});
}}
sortTable(0, -1);
</script>
</body>
</html>
"""


def render_page(records, generated, telescopes):
    from flask import get_flashed_messages

    options = "\n".join(
        f'<option value="{html.escape(t)}">{html.escape(t)}</option>' for t in telescopes
    )
    flashes = "".join(
        f'<div class="flash {html.escape(cat)}">{html.escape(msg)}</div>'
        for cat, msg in get_flashed_messages(with_categories=True)
    )
    def image_url(path):
        # path is under ROOT: /data/Astro/CCD/<telescope>/<object>/master/<file>
        # relpath = <telescope>/<object>/master/<file>
        return f"/img/{quote(os.path.relpath(path, ROOT))}"

    return PAGE_TEMPLATE.format(
        root=html.escape(ROOT),
        n=len(records),
        gen=generated.strftime("%Y-%m-%d %H:%M:%S"),
        telescope_options=options,
        flashes=flashes,
        rows=build_rows(records, image_url=image_url),
    )


if __name__ == "__main__":
    app.run(host="127.0.0.1", port=5000, debug=False)
