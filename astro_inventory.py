#!/usr/bin/env python3
"""
Astronomy image inventory generator.

Traverses /data/Astro/CCD once, builds an in-memory list of all objects
(telescope -> object), and generates a single inventory.html with a sortable
table of all objects.

Row colors:
  red    - no project AND no final image
  yellow - project present but no final image
"""

import os
import re
import html
from datetime import datetime
from collections import defaultdict

ROOT = "/data/Astro/CCD"
OUTPUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "inventory.html")

FILTERS = {
    "L": "Luminance",
    "R": "Red",
    "B": "Blue",
    "G": "Green",
    "S": "SII",
    "O": "OIII",
    "H": "Ha",
}

# --- filename parsing -------------------------------------------------------

# exposure duration patterns: "_300s_", "_1200_Seconds_", "_1200s_1x1_",
# "_600_Seconds_2020-...", "EXPOSURE-30.00s"
RE_DUR_S = re.compile(r"_(\d+(?:\.\d+)?)s_")
RE_DUR_SECS = re.compile(r"_(\d+(?:\.\d+)?)_secs_", re.IGNORECASE)
RE_DUR_EXPOSURE = re.compile(r"EXPOSURE-(\d+(?:\.\d+)?)s", re.IGNORECASE)

# filter: single letter after an underscore, not part of a longer word
RE_FILTER = re.compile(r"_([LRBGSOH])_")

# timestamps in filenames
RE_TS_ISO = re.compile(r"(\d{4}-\d{2}-\d{2})[T_ ](\d{2})[._-](\d{2})[._-](\d{2})")
RE_TS_COMPACT = re.compile(r"(\d{4})(\d{2})(\d{2})_(\d{2})(\d{2})(\d{2})")


def parse_duration(name):
    """Return exposure duration in seconds, or None."""
    m = RE_DUR_S.search(name) or RE_DUR_SECS.search(name) or RE_DUR_EXPOSURE.search(name)
    if m:
        return float(m.group(1))
    return None


def parse_filter(name):
    """Return filter letter, or None (DSLR)."""
    m = RE_FILTER.search(name)
    if m:
        return m.group(1)
    return None


def parse_timestamp(name):
    """Extract a timestamp from the filename, or None."""
    m = RE_TS_ISO.search(name)
    if m:
        try:
            return datetime.strptime(
                f"{m.group(1)} {m.group(2)}:{m.group(3)}:{m.group(4)}",
                "%Y-%m-%d %H:%M:%S",
            )
        except ValueError:
            pass
    m = RE_TS_COMPACT.search(name)
    if m:
        try:
            return datetime.strptime(
                f"{m.group(1)}{m.group(2)}{m.group(3)}{m.group(4)}{m.group(5)}{m.group(6)}",
                "%Y%m%d%H%M%S",
            )
        except ValueError:
            pass
    return None


def fmt_duration(s):
    if s is None:
        return "?"
    if s == int(s):
        return str(int(s))
    return f"{s:g}"


def fmt_total(seconds):
    """Format total seconds as e.g. '2h 30m 15s'."""
    if seconds is None:
        return "?"
    seconds = int(seconds)
    h, rem = divmod(seconds, 3600)
    m, s = divmod(rem, 60)
    parts = []
    if h:
        parts.append(f"{h}h")
    if m:
        parts.append(f"{m}m")
    if s or not parts:
        parts.append(f"{s}s")
    return " ".join(parts)


# --- traversal ---------------------------------------------------------------

def traverse(root):
    """
    Scan: root/telescope/object, with recursive fits/raw detection.

    For each object directory we look at:
      - files at any depth (fits / cr2 / cr3) via os.walk
      - subdirectories directly in the object dir (for *.pxiproject)
      - the master/ subdir directly in the object dir (for final .jpg/.jpeg)

    Returns a list of object records.
    """
    records = []

    with os.scandir(root) as teles:
        for tel in teles:
            if not tel.is_dir():
                continue
            tel_path = tel.path
            with os.scandir(tel_path) as objs:
                for obj in objs:
                    if not obj.is_dir():
                        continue
                    if obj.name.lower() in ("darks", "cal", "calibration", "flats"):
                        continue
                    obj_path = obj.path

                    rec = {
                        "telescope": tel.name,
                        "object": obj.name,
                        "path": obj_path,
                        "filters": defaultdict(lambda: {"count": 0, "seconds": 0.0, "durations": defaultdict(int)}),
                        "cr2cr3_count": 0,
                        "latest": None,
                        "has_project": False,
                        "has_final": False,
                        "size_bytes": 0,
                        "final_images": [],
                    }

                    # --- recursive walk: fits / cr2 / cr3 at any depth ---
                    master_path = os.path.join(obj_path, "master")
                    for dirpath, dirnames, filenames in os.walk(obj_path):
                        # recursive size of the whole object directory
                        for fn in filenames:
                            try:
                                rec["size_bytes"] += os.stat(os.path.join(dirpath, fn)).st_size
                            except OSError:
                                pass

                        # pxiproject: only direct children of the object dir
                        if dirpath == obj_path:
                            for d in dirnames:
                                if d.lower().endswith(".pxiproject"):
                                    rec["has_project"] = True

                        # master/ subdir: final image + pxiproject
                        if dirpath == master_path:
                            for d in dirnames:
                                if d.lower().endswith(".pxiproject"):
                                    rec["has_project"] = True
                            for fn in filenames:
                                lower = fn.lower()
                                if lower.endswith(".jpg") or lower.endswith(".jpeg"):
                                    rec["has_final"] = True
                                    rec["final_images"].append(fn)

                        for fn in filenames:
                            lower = fn.lower()
                            if not (lower.endswith(".fits") or lower.endswith(".fit")
                                    or lower.endswith(".cr2") or lower.endswith(".cr3")):
                                continue

                            fpath = os.path.join(dirpath, fn)
                            try:
                                st = os.stat(fpath)
                            except OSError:
                                continue

                            if lower.endswith(".fits") or lower.endswith(".fit"):
                                dur = parse_duration(fn)
                                flt = parse_filter(fn)
                                if flt is None:
                                    flt = "DSLR"
                                rec["filters"][flt]["count"] += 1
                                if dur is not None:
                                    rec["filters"][flt]["seconds"] += dur
                                    rec["filters"][flt]["durations"][int(dur)] += 1
                                ts = parse_timestamp(fn)
                                if ts is not None:
                                    rec["latest"] = max(rec["latest"] or ts, ts)
                                else:
                                    ts2 = datetime.fromtimestamp(st.st_mtime)
                                    rec["latest"] = max(rec["latest"] or ts2, ts2)

                            elif lower.endswith(".cr2") or lower.endswith(".cr3"):
                                rec["cr2cr3_count"] += 1
                                ts = parse_timestamp(fn)
                                if ts is not None:
                                    rec["latest"] = max(rec["latest"] or ts, ts)
                                else:
                                    ts2 = datetime.fromtimestamp(st.st_mtime)
                                    rec["latest"] = max(rec["latest"] or ts2, ts2)

                    # --- plan.md: object still to be captured ---
                    plan_path = os.path.join(obj_path, "plan.md")
                    if os.path.isfile(plan_path):
                        try:
                            st = os.stat(plan_path)
                            rec["plan_mtime"] = datetime.fromtimestamp(st.st_mtime)
                            with open(plan_path, "r", encoding="utf-8") as pf:
                                rec["plan"] = pf.read()
                        except (OSError, UnicodeDecodeError):
                            rec["plan"] = ""
                        # If no images found, use plan.md mtime as latest
                        if rec["latest"] is None and rec.get("plan_mtime"):
                            rec["latest"] = rec["plan_mtime"]
                    else:
                        rec["plan"] = None

                    records.append(rec)

    return records


# --- html --------------------------------------------------------------------

HTML_TEMPLATE = """<!DOCTYPE html>
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
  .total {{ font-weight: bold; }}
  .meta {{ color: #666; margin-bottom: 1em; }}
  .thumbs img {{ max-height: 60px; max-width: 120px; margin: 2px; border: 1px solid #999; vertical-align: middle; }}
</style>
</head>
<body>
<h1>Astronomy Capture Report</h1>
<p class="meta">Root: {root} &middot; Objects: {n} &middot; Generated: {gen}<br>
Click a column header to sort (click again to reverse). Default: Latest image, descending.</p>
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
    headers[i].querySelector('.arrow').textContent = '\u2195';
  }}
  headers[colIdx].dataset.sorted = dir > 0 ? 'asc' : 'desc';
  headers[colIdx].querySelector('.arrow').textContent = dir > 0 ? '\u2191' : '\u2193';
}}

for (let i = 0; i < headers.length; i++) {{
  headers[i].addEventListener('click', () => {{
    const cur = headers[i].dataset.sorted;
    const dir = (i === 0 && !cur) ? -1 : (cur === 'asc' ? -1 : 1);
    sortTable(i, dir);
  }});
}}
// default: latest image desc
sortTable(0, -1);
</script>
</body>
</html>
"""


def build_rows(records):
    rows = []
    for rec in records:
        total_sec = 0.0
        for flt, d in rec["filters"].items():
            total_sec += d["seconds"]
        total_sec += rec["cr2cr3_count"] * 30

        filter_parts = []
        for flt in sorted(rec["filters"], key=lambda f: (f == "DSLR", f)):
            d = rec["filters"][flt]
            durs = ", ".join(f"{fmt_duration(k)}s" for k in sorted(d["durations"]))
            label = FILTERS.get(flt, flt)
            filter_parts.append(
                f"{label}: {d['count']}x ({durs}) = {fmt_total(d['seconds'])}"
            )
        if rec["cr2cr3_count"]:
            filter_parts.append(
                f"DSLR (CR2/CR3): {rec['cr2cr3_count']}x (30s) = "
                f"{fmt_total(rec['cr2cr3_count'] * 30)}"
            )
        filters_html = "<br>".join(filter_parts) or "—"

        latest = rec["latest"]
        ts_sort = latest.timestamp() if latest else 0
        ts_disp = latest.strftime("%Y-%m-%d %H:%M") if latest else "—"

        cls = ""
        if not rec["has_project"] and not rec["has_final"]:
            cls = "red"
        elif rec["has_project"] and not rec["has_final"]:
            cls = "yellow"

        def yn(v):
            return ("1", "Yes") if v else ("0", "No")

        p_sort, p_disp = yn(rec["has_project"])
        f_sort, f_disp = yn(rec["has_final"])

        plan_html = (
            "<br>".join(html.escape(line) for line in rec["plan"].splitlines())
            if rec["plan"]
            else "—"
        )

        size_mb = rec["size_bytes"] / (1024 * 1024)

        thumbs = []
        for fn in rec["final_images"]:
            url = "file://" + os.path.join(rec["path"], "master", fn)
            thumbs.append(
                f'<a href="{html.escape(url)}">'
                f'<img src="{html.escape(url)}" alt="{html.escape(fn)}"></a>'
            )
        thumbs_html = ('<span class="thumbs">' + " ".join(thumbs) + "</span>") if thumbs else "—"

        rows.append(
            f'<tr class="{cls}">'
            f'<td data-sort="{ts_sort}">{html.escape(ts_disp)}</td>'
            f'<td data-sort="{html.escape(rec["telescope"])}">{html.escape(rec["telescope"])}</td>'
            f'<td data-sort="{html.escape(rec["object"])}">{html.escape(rec["object"])}</td>'
            f'<td data-sort="{total_sec}">{fmt_total(total_sec)}</td>'
            f'<td data-sort="{size_mb:.6f}">{size_mb:.1f}</td>'
            f'<td data-sort="">{filters_html}</td>'
            f'<td data-sort="{p_sort}">{p_disp}</td>'
            f'<td data-sort="{f_sort}">{f_disp}</td>'
            f'<td data-sort="">{thumbs_html}</td>'
            f'<td data-sort="">{plan_html}</td>'
            f"</tr>"
        )
    return "\n".join(rows)


def main():
    print(f"Scanning {ROOT} ...")
    records = traverse(ROOT)
    print(f"Found {len(records)} objects")

    html_text = HTML_TEMPLATE.format(
        root=html.escape(ROOT),
        n=len(records),
        gen=datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        rows=build_rows(records),
    )
    with open(OUTPUT, "w") as f:
        f.write(html_text)
    print(f"Report written to {OUTPUT}")


if __name__ == "__main__":
    main()
