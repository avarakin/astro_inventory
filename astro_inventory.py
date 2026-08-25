#!/usr/bin/env python3
"""
Astronomy image inventory report.

Traverses the CCD directory tree and produces a summary of all captured objects,
including telescope, date range, filter/exposure stats, and processing status.
"""

import os
import re
import sys
from collections import defaultdict
from datetime import datetime
from pathlib import Path

# Directories that are calibration/utility and should be skipped
SKIP_DIRS = {
    'cal', 'calibration', 'calib', 'calibrations',
    'flat', 'flats', 'flatfield',
    'dark', 'darks', 'darkframe',
    'bias', 'biases',
    'master',  # stacked files, not a capture
    'logs', 'log',
    'lib',
    'junk', 'tmp', 'temp', 'tmp',
    'tools', 'tool',
    'template',
    'phd',
    'research',
    'tests', 'test',
    'unsorted',
    'align',
    'dropbox',
}

# RAW camera formats to treat as light frames
RAW_EXTENSIONS = {'.cr3', '.cr2', '.nef', '.arw', '.dng', '.raf', '.srw', '.mrw'}

# Directories that start with '.' are hidden/config and should be skipped
HIDDEN_PREFIXES = {'.'}

# Filter mappings (single letter in filenames)
FILTER_MAP = {
    'L': 'Luminance',
    'R': 'Red',
    'G': 'Green',
    'B': 'Blue',
    'H': 'Ha',
    'O': 'OIII',
    'S': 'SII',
}

# Regex patterns to extract filter and exposure from fits filenames
# Pattern 1: _L_1200_secs_, _H_600_secs_, _B_300s_ (CCD style)
FILTER_SECS_RE = re.compile(r'_(L|H|O|S|R|G|B)_(\d+)_?s(?:ecs)?', re.IGNORECASE)
# Pattern 2: _B_20200412_ (DSLR comet style — filter letter before date)
FILTER_DATE_RE = re.compile(r'_(L|H|O|S|R|G|B)_(\d{6,8})', re.IGNORECASE)
# Pattern 3: _300s_1x1_, _600s_1x1_ (DSL style with duration but no filter)
DURATION_NO_FILTER_RE = re.compile(r'_(\d+)s_', re.IGNORECASE)


def is_skip_dir(name: str) -> bool:
    """Check if a directory name should be skipped (calibration/utility)."""
    if name.lower() in SKIP_DIRS:
        return True
    if name.startswith('.'):
        return True
    return False


def parse_fits_filename(filename: str):
    """
    Parse a FITS/RAW filename to extract filter and exposure duration.
    Returns (filter_name, exposure_seconds, is_calibration).
    Note: RAW files return duration=None — use get_raw_exposures() separately.
    """
    name_lower = filename.lower()

    # RAW files have no filter/duration metadata in filename
    _, ext = os.path.splitext(name_lower)
    if ext in RAW_EXTENSIONS:
        return 'DSLR', None, False

    # Skip non-light frames (dark, flat, bias)
    if any(kw in name_lower for kw in ['dark', 'flat', 'bias', 'dfl']):
        return None, None, True  # is_calibration

    # Try pattern: _FILTER_DURATION_secs_ (CCD style)
    m = FILTER_SECS_RE.search(filename)
    if m:
        filt = m.group(1).upper()
        duration = int(m.group(2))
        return FILTER_MAP.get(filt, filt), duration, False

    # Try pattern: _FILTER_DATE_ (DSLR comet style)
    m = FILTER_DATE_RE.search(filename)
    if m:
        filt = m.group(1).upper()
        # Check if the "date" part is actually a date (6+ digits)
        date_part = m.group(2)
        if len(date_part) >= 6:
            return FILTER_MAP.get(filt, filt), None, False

    # Try pattern: _DURATIONs_ without filter (DSLR style)
    m = DURATION_NO_FILTER_RE.search(filename)
    if m:
        duration = int(m.group(1))
        return 'DSLR', duration, False

    # No filter detected — treat as DSLR
    return 'DSLR', None, False


# Default assumed exposure for RAW files (CR3, CR2, NEF, etc.) when not parseable
RAW_DEFAULT_EXPOSURE = 30  # seconds


def get_fits_timestamp(filepath: str):
    """Get the modification timestamp of a file."""
    try:
        return datetime.fromtimestamp(os.path.getmtime(filepath))
    except OSError:
        return None


def is_light_file(filename: str) -> bool:
    """Check if a file is a light frame (FITS or RAW)."""
    lower = filename.lower()
    if lower.endswith('.fits') or lower.endswith('.fit'):
        return True
    _, ext = os.path.splitext(lower)
    if ext in RAW_EXTENSIONS:
        return True
    return False


def collect_fits_recursive(dirpath: str):
    """
    Recursively collect all .fits/.fit and RAW files under a directory,
    skipping calibration/utility subdirectories.
    """
    fits_files = []
    for sub_dirpath, sub_dirnames, sub_filenames in os.walk(dirpath):
        sub_dirnames[:] = [d for d in sub_dirnames if not is_skip_dir(d)]
        for fn in sub_filenames:
            if is_light_file(fn):
                fits_files.append(os.path.join(sub_dirpath, fn))
    return fits_files


def scan_directory(root_dir: str):
    """
    Traverse the directory tree and collect object information.
    Returns a list of dicts with object details.
    """
    root_path = Path(root_dir).resolve()
    objects = []

    # Collect object directories at depth 2 (CCD/Telescope/Object).
    # Subdirs like Light/H/, satellite/, etc. are merged into the parent.
    # For telescopes with 3-level structure (e.g. 6D/15mm/CSSP-2025),
    # depth 3 is used as the object level.
    object_dirs = set()
    for dirpath, dirnames, filenames in os.walk(root_path):
        # Skip the root directory itself
        if dirpath == str(root_path):
            dirnames[:] = [d for d in dirnames if not is_skip_dir(d)]
            continue

        # Skip calibration-like directories in-place
        dirnames[:] = [d for d in dirnames if not is_skip_dir(d)]

        rel = Path(dirpath).relative_to(root_path)
        depth = len(rel.parts)

        if depth in (2, 3):
            has_fits = any(is_light_file(fn) for fn in filenames)
            if has_fits:
                # For depth 3, only add if parent is not already an object dir
                if depth == 3:
                    parent = str(Path(dirpath).parent)
                    if parent in object_dirs:
                        continue
                object_dirs.add(dirpath)

    # Second pass: process each object directory
    for dirpath in object_dirs:
        object_path = Path(dirpath)
        object_name = object_path.name
        # Telescope is the parent directory name
        telescope_name = object_path.parent.name

        # Collect all fits files recursively under this object
        all_fits = collect_fits_recursive(dirpath)

        # Collect filter stats
        filter_stats = defaultdict(lambda: {'count': 0, 'durations': [], 'timestamps': []})

        for fpath in all_fits:
            filt, duration, is_cal = parse_fits_filename(os.path.basename(fpath))
            if is_cal:
                continue  # skip calibration frames

            ts = get_fits_timestamp(fpath)
            stats = filter_stats[filt]
            stats['count'] += 1
            if duration is not None:
                stats['durations'].append(duration)
            if ts is not None:
                stats['timestamps'].append(ts)

        # Add default exposure for RAW files
        raw_count = sum(1 for fpath in all_fits if os.path.splitext(os.path.basename(fpath))[1].lower() in RAW_EXTENSIONS)
        if raw_count:
            stats = filter_stats.get('DSLR')
            if stats:
                stats['durations'].extend([RAW_DEFAULT_EXPOSURE] * raw_count)

        # Collect all timestamps for date range
        all_timestamps = []
        for stats in filter_stats.values():
            all_timestamps.extend(stats['timestamps'])

        date_min = min(all_timestamps) if all_timestamps else None
        date_max = max(all_timestamps) if all_timestamps else None

        # Check for project file (.pxiproject directory)
        project_present = any(
            d.lower().endswith('.pxiproject') for d in os.listdir(dirpath)
            if os.path.isdir(os.path.join(dirpath, d))
        )

        # Check for final image (.jpg files in master dir only)
        jpg_files = []
        master_dir = os.path.join(dirpath, 'master')
        if os.path.isdir(master_dir):
            for fn in os.listdir(master_dir):
                if fn.lower().endswith('.jpg'):
                    jpg_files.append(f"master/{fn}")
        final_image_present = len(jpg_files) > 0

        # Build filter summary
        filter_summary = {}
        for filt_name, stats in sorted(filter_stats.items()):
            durations = stats['durations']
            total_seconds = sum(durations)
            filter_summary[filt_name] = {
                'count': stats['count'],
                'exposures': sorted(set(durations)) if durations else [],
                'total_seconds': total_seconds,
                'total_hours': total_seconds / 3600 if total_seconds else 0,
            }

        objects.append({
            'object_name': object_name,
            'telescope': telescope_name,
            'path': str(dirpath),
            'date_min': date_min,
            'date_max': date_max,
            'filters': filter_summary,
            'project_present': project_present,
            'final_image_present': final_image_present,
        })

    # Sort by telescope, then object name
    objects.sort(key=lambda o: (o['telescope'].lower(), o['object_name'].lower()))
    return objects


def format_duration(seconds: int) -> str:
    """Format seconds into a human-readable duration."""
    if seconds < 60:
        return f"{seconds}s"
    elif seconds < 3600:
        mins = seconds // 60
        secs = seconds % 60
        return f"{mins}m {secs}s" if secs else f"{mins}m"
    else:
        hours = seconds // 3600
        mins = (seconds % 3600) // 60
        return f"{hours}h {mins}m" if mins else f"{hours}h"


def print_report(objects: list):
    """Print a formatted report of all objects."""
    print(f"{'='*100}")
    print(f"ASTRONOMY IMAGE INVENTORY — {len(objects)} objects found")
    print(f"{'='*100}\n")

    current_telescope = None

    for obj in objects:
        # Print telescope header
        if obj['telescope'] != current_telescope:
            current_telescope = obj['telescope']
            print(f"\n{'─'*100}")
            print(f"  Telescope: {current_telescope}")
            print(f"{'─'*100}")

        # Object header
        print(f"\n  Object: {obj['object_name']}")
        print(f"  Path:   {obj['path']}")

        # Date range
        if obj['date_min'] and obj['date_max']:
            print(f"  Dates:  {obj['date_min'].strftime('%Y-%m-%d %H:%M')} — {obj['date_max'].strftime('%Y-%m-%d %H:%M')}")
        else:
            print(f"  Dates:  N/A")

        # Filter details
        print(f"  Filters:")
        total_exposure = 0
        for filt_name, stats in obj['filters'].items():
            exp_list = stats['exposures']
            exp_str = ', '.join(f"{d}s" for d in exp_list) if exp_list else 'unknown'
            total_hours = stats['total_hours']
            total_exp_str = format_duration(int(total_hours * 3600)) if total_hours else 'N/A'
            print(f"    {filt_name:12s}  —  {stats['count']:3d} frames,  exposure: {exp_str},  total: {total_exp_str}")
            total_exposure += stats['total_seconds']

        print(f"  {'─'*60}")
        print(f"  Total exposure: {format_duration(total_exposure)}")
        print(f"  Project:   {'Yes' if obj['project_present'] else 'No'}")
        print(f"  Final JPG: {'Yes' if obj['final_image_present'] else 'No'}")

    # Summary
    print(f"\n\n{'='*100}")
    print(f"SUMMARY")
    print(f"{'='*100}")
    telescopes = sorted(set(o['telescope'] for o in objects))
    for tel in telescopes:
        tel_objs = [o for o in objects if o['telescope'] == tel]
        with_project = sum(1 for o in tel_objs if o['project_present'])
        with_final = sum(1 for o in tel_objs if o['final_image_present'])
        print(f"  {tel:20s}  —  {len(tel_objs):3d} objects  "
              f"({with_project} with project, {with_final} with final image)")
    print(f"\n  Total: {len(objects)} objects across {len(telescopes)} telescopes")


def html_report(objects: list, output_path: str):
    """Generate an HTML report with one row per object."""
    rows = []
    for obj in objects:
        total_exposure = sum(s['total_seconds'] for s in obj['filters'].values())
        exposures_per_filter = '; '.join(
            f"{k}: {v['count']} frames" for k, v in obj['filters'].items()
        )
        has_project = 'Yes' if obj['project_present'] else 'No'
        has_jpg = 'Yes' if obj['final_image_present'] else 'No'
        last_modified = obj['date_max'].strftime('%Y-%m-%d %H:%M') if obj['date_max'] else 'N/A'
        last_modified_ts = int(obj['date_max'].timestamp()) if obj['date_max'] else 0
        rows.append({
            'telescope': obj['telescope'],
            'object': obj['object_name'],
            'total_exposure': format_duration(total_exposure),
            'total_seconds': total_exposure,
            'exposures_per_filter': exposures_per_filter,
            'has_project': has_project,
            'has_jpg': has_jpg,
            'last_modified': last_modified,
            'last_modified_ts': last_modified_ts,
        })

    # Escape HTML in cell values
    def esc(s):
        return str(s).replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;').replace('"', '&quot;')

    rows_json = []
    for row in rows:
        rows_json.append({
            'telescope': row['telescope'],
            'object': row['object'],
            'total_exposure': row['total_exposure'],
            'total_seconds': int(row['total_seconds']),
            'exposures_per_filter': row['exposures_per_filter'],
            'has_project': row['has_project'],
            'has_jpg': row['has_jpg'],
            'last_modified': row['last_modified'],
            'last_modified_ts': row['last_modified_ts'],
        })

    import json
    data_json = json.dumps(rows_json)

    html = f"""<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>Astronomy Image Inventory</title>
<style>
  body {{ font-family: sans-serif; margin: 1em; }}
  table {{ border-collapse: collapse; width: 100%; }}
  th, td {{ border: 1px solid #ccc; padding: 6px 8px; text-align: left; }}
  th {{ background: #f4f4f4; cursor: pointer; user-select: none; }}
  th:hover {{ background: #e8e8e8; }}
  th .sort-arrow {{ font-size: 0.8em; color: #888; }}
  tr:nth-child(even) {{ background: #fafafa; }}
  tr.no-project {{ background: #fdd; }}
  tr.no-project:nth-child(even) {{ background: #fdd; }}
  tr.no-jpg {{ background: #fff3cd; }}
  tr.no-jpg:nth-child(even) {{ background: #fff3cd; }}
  tr.no-project.no-jpg {{ background: #fdd; }}
</style>
</head>
<body>
<h1>Astronomy Image Inventory</h1>
<p>{len(rows)} objects</p>
<table id="inventory">
<thead>
<tr>
  <th data-col="0" onclick="sortTable(0)">Telescope <span class="sort-arrow">▲</span></th>
  <th data-col="1" onclick="sortTable(1)">Object <span class="sort-arrow">▲</span></th>
  <th data-col="2" onclick="sortTable(2)">Total Exposure <span class="sort-arrow">▲</span></th>
  <th data-col="3" onclick="sortTable(3)">Exposures per Filter <span class="sort-arrow">▲</span></th>
  <th data-col="4" onclick="sortTable(4)">Has Project <span class="sort-arrow">▲</span></th>
  <th data-col="5" onclick="sortTable(5)">Has JPG <span class="sort-arrow">▲</span></th>
  <th data-col="6" onclick="sortTable(6)">Last Modified <span class="sort-arrow">▲</span></th>
</tr>
</thead>
<tbody>
"""
    for row in rows:
        classes = []
        if row['has_jpg'] == 'No':
            if row['has_project'] == 'No':
                classes.append('no-project')
            else:
                classes.append('no-jpg')
        class_str = ' '.join(classes) if classes else ''
        html += f"""<tr class="{class_str}">
  <td>{esc(row['telescope'])}</td>
  <td>{esc(row['object'])}</td>
  <td>{esc(row['total_exposure'])}</td>
  <td>{esc(row['exposures_per_filter'])}</td>
  <td>{esc(row['has_project'])}</td>
  <td>{esc(row['has_jpg'])}</td>
  <td>{esc(row['last_modified'])}</td>
</tr>
"""
    js = """</tbody>
</table>
<script>
var data = DATA_PLACEHOLDER;
var currentCol = -1;
var ascending = true;

function sortTable(col) {
    if (currentCol === col) { ascending = !ascending; }
    currentCol = col;
    var keys = ['telescope','object','total_exposure','exposures_per_filter','has_project','has_jpg','last_modified'];
    var key = keys[col];
    var sorted = data.slice().sort(function(a, b) {
        var cmp = 0;
        if (key === 'total_exposure') { cmp = a.total_seconds - b.total_seconds; }
        else if (key === 'last_modified') { cmp = a.last_modified_ts - b.last_modified_ts; }
        else { cmp = a[key] < b[key] ? -1 : (a[key] > b[key] ? 1 : 0); }
        return ascending ? cmp : -cmp;
    });
    var tbody = document.querySelector('#inventory tbody');
    var frag = document.createDocumentFragment();
    for (var i = 0; i < sorted.length; i++) {
        var tr = document.createElement('tr');
        if (sorted[i].has_jpg === 'No') {
            if (sorted[i].has_project === 'No') tr.classList.add('no-project');
            else tr.classList.add('no-jpg');
        }
        var cells = [sorted[i].telescope, sorted[i].object, sorted[i].total_exposure,
                     sorted[i].exposures_per_filter, sorted[i].has_project, sorted[i].has_jpg,
                     sorted[i].last_modified];
        for (var j = 0; j < cells.length; j++) {
            var td = document.createElement('td');
            td.textContent = cells[j];
            tr.appendChild(td);
        }
        frag.appendChild(tr);
    }
    tbody.innerHTML = '';
    tbody.appendChild(frag);
    document.querySelectorAll('th .sort-arrow').forEach(function(el, idx) {
        el.textContent = idx === col ? (ascending ? '▲' : '▼') : '▲';
    });
}
</script>
</body>
</html>"""
    html += js.replace('DATA_PLACEHOLDER', data_json)

    with open(output_path, 'w') as f:
        f.write(html)
    print(f"HTML report written to {output_path}")


def main():
    root_dir = sys.argv[1] if len(sys.argv) > 1 else '/data/Astro/CCD'
    output_file = sys.argv[2] if len(sys.argv) > 2 else 'inventory.html'

    if not os.path.isdir(root_dir):
        print(f"Error: Directory not found: {root_dir}")
        sys.exit(1)

    print(f"Scanning {root_dir}...")
    objects = scan_directory(root_dir)
    html_report(objects, output_file)
    print_report(objects)


if __name__ == '__main__':
    main()
