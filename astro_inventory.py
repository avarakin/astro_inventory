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
import math
import argparse
import urllib.request
from datetime import datetime, timedelta
from tzlocal import get_localzone
from collections import defaultdict

ROOT = None
OUTPUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "inventory.html")

# --- location detection (GeoLite2) -------------------------------------------

_GEOIP_DB = os.path.join(os.path.dirname(os.path.abspath(__file__)), "GeoLite2-City.mmdb")
_LOCATION_CACHE = os.path.join(os.path.dirname(os.path.abspath(__file__)), ".location")


def detect_longitude():
    """Return the local longitude in degrees using GeoLite2 + public IP.
    Cached in .location file so we only hit the network once."""
    # try cache first
    try:
        with open(_LOCATION_CACHE) as f:
            val = f.read().strip()
        if val:
            return float(val)
    except (OSError, ValueError):
        pass
    # try GeoLite2
    try:
        import geoip2.database
        ip = urllib.request.urlopen("https://api.ipify.org", timeout=10).read().decode().strip()
        with geoip2.database.Reader(_GEOIP_DB) as reader:
            city = reader.city(ip)
            lon = city.location.longitude
            with open(_LOCATION_CACHE, "w") as f:
                f.write(str(lon))
            return lon
    except Exception as e:
        print(f"WARNING: GeoIP lookup failed: {e}", flush=True)
    return 0.0


LONGITUDE = detect_longitude()


# --- transit calculation ------------------------------------------------------

def parse_ra_hours(text):
    """Extract RA from plan.md text. Supports YAML frontmatter 'ra: 23h20m29s'
    and old format '- **RA:** 23h20m29s'. Returns hours (float) or None."""
    if not text:
        return None
    # try YAML frontmatter: ra: 23h20m29s
    m = re.search(r"^ra:\s*(\d+)h(\d+)m(\d+)s", text, re.MULTILINE)
    if not m:
        # try old format: - **RA:** 23h20m29s
        m = re.search(r"\*\*RA:\*\*\s*(\d+)h(\d+)m(\d+)s", text)
    if not m:
        return None
    h, mi, s = int(m.group(1)), int(m.group(2)), int(m.group(3))
    return h + mi / 60.0 + s / 3600.0


def transit_date(ra_hours, lon_deg):
    """
    Find the upcoming date when an object transits at local civil midnight.

    Scans forward from today, computing LST at local midnight each night.
    Returns the date (datetime) where LST_midnight is closest to RA_object,
    i.e. the object crosses the meridian at ~midnight.

    Uses tzlocal to detect the system timezone (handles DST automatically).
    """

    def gmst_hours(dt):
        jd = dt.timestamp() / 86400.0 + 2440587.5
        d = jd - 2451545.0
        gmst = (18.697374558 + 24.06570982441908 * d) % 24.0
        return gmst

    def lst_hours(dt):
        return (gmst_hours(dt) + lon_deg / 15.0) % 24.0

    def circ_diff(a, b):
        d = abs(a - b) % 24.0
        return min(d, 24.0 - d)

    tz = get_localzone()
    now = datetime.now(tz)
    midnight = now.replace(hour=0, minute=0, second=0, microsecond=0)

    # Scan forward up to 366 days; find the midnight where
    # LST is closest to RA_object (transit closest to midnight)
    best = None
    best_diff = 999.0
    for day in range(366):
        m = midnight + timedelta(days=day)
        diff = circ_diff(lst_hours(m), ra_hours)
        if diff < best_diff:
            best_diff = diff
            best = m
    if best is None:
        return None

    # Refine: bisect to find the exact midnight closest to transit
    t0 = best - timedelta(days=1.5)
    t1 = best + timedelta(days=1.5)

    def f(t):
        return circ_diff(lst_hours(t), ra_hours)

    for _ in range(50):
        tm = t0 + (t1 - t0) / 2
        if f(t0) <= f(tm):
            t1 = tm
        else:
            t0 = tm
        if (t1 - t0) < timedelta(days=0.01):
            break
    tm = t0 + (t1 - t0) / 2
    return tm.replace(hour=0, minute=0, second=0, microsecond=0)

FILTERS = {
    "L": "Luminance",
    "R": "Red",
    "B": "Blue",
    "G": "Green",
    "S": "SII",
    "O": "OIII",
    "H": "Ha",
}

# --- constellation abbreviation expansion -------------------------------------

CONSTELLATIONS = {
    "And": "Andromeda",
    "Ant": "Antlia",
    "Aps": "Apus",
    "Ape": "Apus",
    "Aqr": "Aquarius",
    "Aql": "Aquila",
    "Ara": "Ara",
    "Ari": "Aries",
    "Aur": "Auriga",
    "Boo": "Bootes",
    "Cae": "Caelum",
    "Cam": "Camelopardalis",
    "Cap": "Capricornus",
    "Car": "Carina",
    "Cen": "Centaurus",
    "Cet": "Cetus",
    "Cha": "Chamaeleon",
    "Cir": "Circinus",
    "Col": "Columba",
    "Com": "Coma Berenices",
    "Crt": "Crater",
    "CVn": "Canes Venatici",
    "CMi": "Canis Minor",
    "CMa": "Canis Major",
    "Crt": "Cancer",
    "Crt2": "Crater",
    "Cas": "Cassiopeia",
    "Cep": "Cepheus",
    "CrA": "Corona Australis",
    "CrB": "Corona Borealis",
    "Crv": "Corvus",
    "Cru": "Crux",
    "Cyg": "Cygnus",
    "Del": "Delphinus",
    "Dor": "Dorado",
    "Dra": "Draco",
    "Equ": "Equuleus",
    "Eri": "Eridanus",
    "For": "Fornax",
    "Gem": "Gemini",
    "Grus": "Grus",
    "Her": "Hercules",
    "Hor": "Horologium",
    "Hya": "Hydra",
    "Hyi": "Hydrus",
    "Ind": "Indus",
    "Lac": "Lacerta",
    "Leo": "Leo",
    "LMi": "Leo Minor",
    "Lep": "Lepus",
    "Lib": "Libra",
    "Lup": "Lupus",
    "Lyn": "Lynx",
    "Lyr": "Lyra",
    "Men": "Mensa",
    "Mic": "Microscopium",
    "Mon": "Monoceros",
    "Mus": "Musca",
    "Nor": "Norma",
    "Oct": "Octans",
    "Oph": "Ophiuchus",
    "Ori": "Orion",
    "Pav": "Pavo",
    "Peg": "Pegasus",
    "Per": "Perseus",
    "Phe": "Phoenix",
    "Pic": "Pictor",
    "Psc": "Pisces",
    "PsA": "Piscis Austrinus",
    "Pup": "Puppis",
    "Pyx": "Pyxis",
    "Rta": "Reticulum",
    "Sge": "Sagitta",
    "Sgr": "Sagittarius",
    "Sco": "Scorpius",
    "Scl": "Sculptor",
    "Sct": "Scutum",
    "Ser": "Serpens",
    "Sex": "Sextans",
    "Tau": "Taurus",
    "Tel": "Telescopium",
    "Tri": "Triangulum",
    "TrA": "Triangulum Australe",
    "Tuc": "Tucana",
    "UMi": "Ursa Minor",
    "UMa": "Ursa Major",
    "Vel": "Vela",
    "Vir": "Virgo",
    "Vul": "Vulpecula",
    "Vol": "Volans",
}


def expand_constellation(value):
    """Expand a constellation abbreviation to its full name.
    Unknown values are returned unchanged."""
    if not value:
        return value
    return CONSTELLATIONS.get(value.strip(), value)

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
                    rec["transit"] = None
                    rec["transit_sort"] = 0
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
                        # compute transit date from RA in plan
                        ra_h = parse_ra_hours(rec["plan"])
                        if ra_h is not None:
                            td = transit_date(ra_h, LONGITUDE)
                            if td is not None:
                                rec["transit"] = td.strftime("%Y-%m-%d")
                                rec["transit_sort"] = td.timestamp()
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
  th .arrow {{ display: block; font-size: 20px; font-weight: bold; text-align: center; margin-top: 2px; }}
  .btn {{ padding: 4px 10px; font-size: 12px; border: none; border-radius: 4px; cursor: pointer; }}
  .btn-edit {{ background: #4a90d9; color: #fff; }}
  .btn-edit:hover {{ background: #357abd; }}
  .btn-del {{ background: #d9534f; color: #fff; }}
  .btn-del:hover {{ background: #c9302c; }}
  tr:nth-child(even) {{ background: #f4f6fa; }}
  tr.red {{ background: #f8d7da !important; }}
  tr.yellow {{ background: #fff3cd !important; }}
  .total {{ font-weight: bold; }}
  .meta {{ color: #666; margin-bottom: 1em; }}
  .thumbs img {{ max-height: 60px; max-width: 120px; margin: 2px; border: 1px solid #999; vertical-align: middle; }}
  .plan {{ font-size: 12px; }}
  .plan-top {{ margin-bottom: 4px; }}
  .badge {{ display: inline-block; background: #2b3a55; color: #fff; border-radius: 3px; padding: 1px 7px; font-size: 11px; font-weight: 600; margin-right: 6px; }}
  .plan-link {{ display: inline-block; padding: 1px 8px; background: #4a90d9; color: #fff; border-radius: 4px; text-decoration: none; font-size: 11px; }}
  .plan-link:hover {{ background: #357abd; }}
  .plan-body {{ margin: 2px 0; line-height: 1.4; }}
  .plan-meta {{ color: #666; font-size: 11px; margin-top: 3px; }}
</style>
</head>
<body>
<h1>Astro Imaging Tracker</h1>
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
  <th data-type="str">Actions<span class="arrow"></span></th>
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


_URL_RE = re.compile(r"https?://[^\s<>\"']+")


def linkify(escaped_text):
    """Wrap http(s) URLs in already-escaped text with target=_blank anchors."""
    return _URL_RE.sub(
        lambda m: f'<a href="{m.group(0)}" target="_blank" rel="noopener noreferrer">{m.group(0)}</a>',
        escaped_text,
    )


def _parse_plan(text):
    """Split a plan.md into (meta_dict, body). meta keys lowercased.
    Returns ({}, text) when there is no YAML frontmatter."""
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return {}, text
    meta = {}
    for i in range(1, len(lines)):
        if lines[i].strip() == "---":
            body = "\n".join(
                l for l in lines[i + 1:] if not l.lstrip().startswith("#")
            )
            return meta, body.strip()
        if ":" in lines[i]:
            k, v = lines[i].split(":", 1)
            meta[k.strip().lower()] = v.strip()
    return {}, text


def _plan_link_label(url):
    return "AstroBin" if "astrobin" in url.lower() else "Link"


def render_plan(plan_text):
    """Render a plan.md string as a compact, styled HTML block."""
    if not plan_text or not plan_text.strip():
        return "—"
    meta, body = _parse_plan(plan_text)
    top = []
    if meta.get("constellation"):
        top.append(f'<span class="badge">{html.escape(expand_constellation(meta["constellation"]))}</span>')
    if meta.get("link"):
        url = meta["link"]
        top.append(
            f'<a class="plan-link" href="{html.escape(url)}" '
            f'target="_blank" rel="noopener noreferrer">{html.escape(_plan_link_label(url))} &#8599;</a>'
        )
    parts = []
    if top:
        parts.append(f'<div class="plan-top">{"".join(top)}</div>')
    meta_bits = [
        f"RA {meta['ra']}" if meta.get("ra") else "",
        f"Dec {meta['dec']}" if meta.get("dec") else "",
        f"Rot {meta['rotation']}" if meta.get("rotation") else "",
    ]
    meta_bits = [b for b in meta_bits if b]
    if meta_bits:
        parts.append(f'<div class="plan-meta">{"<br>".join(html.escape(b) for b in meta_bits)}</div>')
    if body:
        body_html = "<br>".join(linkify(html.escape(l)) for l in body.splitlines())
        parts.append(f'<div class="plan-body">{body_html}</div>')
    if not parts:
        return linkify(html.escape(plan_text))
    return '<div class="plan">' + "".join(parts) + "</div>"


def build_rows(records, image_url=None, actions_url=None):
    """image_url: callable(path) -> url for final images; default is file://
    actions_url: callable(telescope, object) -> (edit_url, delete_url) or None"""
    if image_url is None:
        image_url = lambda p: "file://" + p
    rows = []
    for idx, rec in enumerate(records):
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

        plan_html = render_plan(rec["plan"])

        size_mb = rec["size_bytes"] / (1024 * 1024)

        thumbs = []
        for fn in rec["final_images"]:
            url = image_url(os.path.join(rec["path"], "master", fn))
            thumbs.append(
                f'<a href="{html.escape(url)}" target="_blank">'
                f'<img src="{html.escape(url)}" alt="{html.escape(fn)}"></a>'
            )
        thumbs_html = ('<span class="thumbs">' + " ".join(thumbs) + "</span>") if thumbs else "—"

        if actions_url is None:
            actions_html = "—"
        else:
            edit_url, delete_url = actions_url(rec["telescope"], rec["object"])
            obj_label = html.escape(rec["telescope"] + "/" + rec["object"])
            actions_html = (
                f'<button class="btn btn-edit" onclick="window.open(\'{html.escape(edit_url)}\', \'_blank\')">Edit plan</button> '
                f'<button class="btn btn-del" '
                f'onclick="if(confirm(\'Delete {obj_label} and all its files? This cannot be undone.\'))document.getElementById(\'del_{idx}\').submit();return false">'
                f'Delete directory</button>'
                f'<form id="del_{idx}" method="post" action="{html.escape(delete_url)}" style="display:none"><input type="hidden" name="confirm" value="yes"></form>'
            )

        transit_disp = rec.get("transit") or "—"
        transit_sort = rec.get("transit_sort") or 0

        rows.append(
            f'<tr class="{cls}">'
            f'<td data-sort="{ts_sort}">{html.escape(ts_disp)}</td>'
            f'<td data-sort="{html.escape(rec["telescope"])}">{html.escape(rec["telescope"])}</td>'
            f'<td data-sort="{html.escape(rec["object"])}">{html.escape(rec["object"])}</td>'
            f'<td data-sort="{transit_sort}">{html.escape(transit_disp)}</td>'
            f'<td data-sort="{total_sec}">{fmt_total(total_sec)}</td>'
            f'<td data-sort="{size_mb:.6f}">{size_mb:.1f}</td>'
            f'<td data-sort="">{filters_html}</td>'
            f'<td data-sort="{p_sort}">{p_disp}</td>'
            f'<td data-sort="{f_sort}">{f_disp}</td>'
            f'<td data-sort="">{thumbs_html}</td>'
            f'<td data-sort="">{plan_html}</td>'
            f'<td data-sort="">{actions_html}</td>'
            f"</tr>"
        )
    return "\n".join(rows)


def main():
    global ROOT
    parser = argparse.ArgumentParser(description="Astronomy image inventory generator")
    parser.add_argument("--root", required=True, help="Root directory for CCD data")
    args = parser.parse_args()
    ROOT = args.root

    print(f"Scanning {ROOT} ...")
    records = traverse(ROOT)
    print(f"Found {len(records)} objects")

    html_text = HTML_TEMPLATE.format(
        root=html.escape(ROOT),
        n=len(records),
        gen=datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        rows=build_rows(records),
    )
    with open(OUTPUT, "w", encoding="utf-8") as f:
        f.write(html_text)
    print(f"Report written to {OUTPUT}")


if __name__ == "__main__":
    main()
