# Rust Rewrite Prompts — Astro Imaging Tracker

This file contains a sequence of self-contained prompts for reproducing the entire
Python application in this directory as a Rust application. Give the prompts to an
AI coding agent **in order** (each builds on the previous). The original Python
sources (`astro_inventory.py`, `astro_inventory_server.py`) remain the source of
truth; every behavior, regex, formula, and edge case below is extracted from them.

The final Rust application is a single binary web server (no separate static HTML
generation mode is required; the old `inventory.html` mode is superseded by the
server). Suggested framework: **Axum** (any framework with equivalent capability
is fine, but the prompts assume Axum + tower).

---

## Prompt 1 — Project scaffolding

```
Create a new Rust project named `astro_inventory` (binary crate) in a new
directory. It will be a web server for an astronomy image inventory.

Requirements:
- Edition 2021, single binary crate.
- Dependencies (use latest stable versions):
  - axum (web framework)
  - tokio (async runtime, features: full)
  - tower-http (features: fs is NOT needed; we need no special middleware)
  - serde + serde_json (JSON)
  - chrono (features: std, clock; plus a local-timezone feature or the
    `tz`/`chrono-tz` approach — see Prompt 4; if using chrono-tz, add it)
  - regex
  - reqwest (features: json; blocking NOT required)
  - mmdb-latest (or `mmdb` crate) for GeoLite2-City.mmdb lookups
  - clap (derive) for CLI args
  - anyhow or thiserror for errors
  - percent-encoding for URL quoting
- Layout:
  - src/main.rs        — CLI parsing + server startup
  - src/traverse.rs    — directory traversal & record building (Prompt 2/3)
  - src/transit.rs     — transit-at-midnight computation (Prompt 4)
  - src/location.rs    — GeoLite2 longitude detection (Prompt 5)
  - src/plan.rs        — plan.md parsing/rendering, constellations (Prompt 6)
  - src/server.rs      — axum app, routes, state (Prompt 7/8)
  - src/render.rs      — HTML rendering helpers (Prompt 7)
- The server must bind to 0.0.0.0:5000.
- CLI args (clap), matching the original:
  - --root <PATH>      (required) root directory of CCD data
  - --page-size <N>    (optional, default 20) rows per page; 0 disables paging
- Do NOT hardcode any default root.
- Build must pass with no warnings. No functionality yet beyond a stub
  GET / returning "ok".
```

---

## Prompt 2 — Core data model & directory traversal

```
In src/traverse.rs, implement the data model and single-pass directory
traversal that mirrors astro_inventory.py `traverse()`.

Data model (struct `ObjectRecord`):
- telescope: String
- object: String
- path: PathBuf                      // full path of the object directory
- filters: BTreeMap<String, FilterStats>
    where FilterStats { count: u64, seconds: f64, durations: BTreeMap<i64, u64> }
    (durations maps exposure-seconds -> file count)
- cr2cr3_count: u64
- latest: Option<DateTime<FixedOffset>>  // or a UTC timestamp + local tz
- has_project: bool
- has_final: bool
- size_bytes: u64
- final_images: Vec<String>         // filenames in master/
- plan: Option<String>              // full text of plan.md
- plan_mtime: Option<DateTime>
- transit: Option<String>           // "YYYY-MM-DD" display
- transit_sort: f64                 // unix timestamp for sorting (0 if none)

Traversal rules (exact):
1. Scan ROOT once. Top-level directories are telescopes. Inside each,
   directories are objects.
2. SKIP object directories whose lowercased name is one of:
   "darks", "cal", "calibration", "flats".
3. For each object directory, do a recursive walk (std::fs::read_dir
   recursion or walkdir) over the whole object tree:
   - Accumulate recursive size: sum st_size of every regular file found
     at any depth. Wrap each stat in error handling so one bad file
     never aborts the walk.
   - has_project: true if any DIRECT child of the object dir is a
     directory whose name (lowercased) ends with ".pxiproject", OR any
     direct child of the object's "master/" subdir ends with ".pxiproject".
   - has_final: true if the object's "master/" subdir (direct children
     only) contains any file ending in ".jpg" or ".jpeg" (case-insensitive).
     Collect those filenames into final_images.
   - Image files at ANY depth: files ending in .fits/.fit/.cr2/.cr3
     (case-insensitive):
     * .fits/.fit:
         - duration = parse_duration(filename)
         - filter = parse_filter(filename), or "DSLR" if none
         - filters[filter].count += 1
         - if duration is Some: seconds += duration;
           durations[duration as i64] += 1
         - timestamp: parse_timestamp(filename); if None, use file mtime.
           rec.latest = max(rec.latest, ts)
     * .cr2/.cr3:
         - cr2cr3_count += 1
         - timestamp: parse_timestamp(filename) else file mtime;
           rec.latest = max(rec.latest, ts)
4. plan.md handling (direct child of object dir):
   - If it exists: read as UTF-8 (on error, treat as empty string),
     record plan_mtime (file mtime).
   - If rec.latest is still None, use plan_mtime as latest.
   - Parse RA from plan text (see Prompt 3) and compute transit date
     (see Prompt 4) using the global longitude (Prompt 5):
       rec.transit = Some(date formatted "YYYY-MM-DD")
       rec.transit_sort = that date's unix timestamp
   - If plan.md does not exist: plan = None.
5. Return Vec<ObjectRecord>.

Also implement the helper `fn total_seconds(rec) -> f64`:
  sum of filters[flt].seconds over all filters, plus cr2cr3_count * 30.0.
```

---

## Prompt 3 — Filename parsing (filters, durations, timestamps, RA)

```
In src/traverse.rs (or a parsing module), implement exact filename parsing
mirroring astro_inventory.py. Use the `regex` crate.

Filter letter (RE_FILTER):
  pattern: _([LRBGSOH])_
  Return the letter, or None (meaning DSLR for FITS files).
  Mapping for display: L=Luminance, R=Red, B=Blue, G=Green,
  S=SII, O=OIII, H=Ha.

Exposure duration — try in this order, first match wins:
  RE_DUR_S:       _(\d+(?:\.\d+)?)s_
  RE_DUR_SECS:    _(\d+(?:\.\d+)?)_secs_   (case-insensitive)
  RE_DUR_EXPOSURE: EXPOSURE-(\d+(?:\.\d+)?)s  (case-insensitive)
  Return f64 seconds or None.
  Examples that must parse: "_300s_", "_1200_Seconds_", "_1200s_1x1_",
  "_600_Seconds_2020-...", "EXPOSURE-30.00s".

Timestamp — try in this order:
  RE_TS_ISO:     (\d{4}-\d{2}-\d{2})[T_ ](\d{2})[._-](\d{2})[._-](\d{2})
    -> datetime YYYY-MM-DD HH:MM:SS (local, naive)
  RE_TS_COMPACT: (\d{4})(\d{2})(\d{2})_(\d{2})(\d{2})(\d{2})
    -> datetime YYYYMMDD_HHMMSS (local, naive)
  Return Option<DateTime> or None; invalid dates are treated as None.

RA parsing from plan.md text (parse_ra_hours) -> Option<f64> hours:
  1) YAML frontmatter style: line-start "ra: 23h20m29s"
     pattern (multiline): ^ra:\s*(\d+)h(\d+)m(\d+)s
  2) Old format: "- **RA:** 23h20m29s"
     pattern: \*\*RA:\*\*\s*(\d+)h(\d+)m(\d+)s
  hours = h + m/60 + s/3600.

Formatting helpers (must match Python output exactly):
  fmt_duration(s): None -> "?"; integer values -> "300"; else shortest
    representation (e.g. 30.0 -> "30", 30.5 -> "30.5").
  fmt_total(seconds): Option<f64> -> "2h 30m 15s" style:
    truncate to int seconds; h = s/3600, m = (s%3600)/60, r = s%60;
    include "Xh" if h>0, "Xm" if m>0, and "Xs" if r>0 OR no parts yet;
    join with single spaces. None -> "?".

Filters column rendering (per record, used in HTML rows):
  For each filter in sorted order (DSLR last, others alphabetical):
    "Label: {count}x ({durations joined as '30s, 60s' with ', '}) = {fmt_total}"
  If cr2cr3_count > 0, append:
    "DSLR (CR2/CR3): {n}x (30s) = {fmt_total(n*30)}"
  Join lines with <br>. Empty -> "—".
```

---

## Prompt 4 — Transit date computation (astronomy)

```
In src/transit.rs, implement transit_date(ra_hours: f64, lon_deg: f64)
-> Option<LocalMidnightDate>, exactly mirroring astro_inventory.py.

Algorithm (must be identical):
1. GMST in hours for a datetime dt:
     jd = dt.unix_timestamp() / 86400.0 + 2440587.5
     d  = jd - 2451545.0
     gmst = (18.697374558 + 24.06570982441908 * d) % 24.0
2. LST in hours: lst = (gmst + lon_deg / 15.0) % 24.0
3. Circular difference in hours:
     circ_diff(a, b) = min(|a-b| % 24, 24 - (|a-b| % 24))
4. Let now = current local time (system timezone, DST-aware).
   midnight = now with hour/minute/second/microsecond zeroed.
5. Coarse scan: for day in 0..366:
     candidate = midnight + day days
     diff = circ_diff(lst(candidate), ra_hours)
     keep the candidate with the smallest diff.
6. Refine by bisection on f(t) = circ_diff(lst(t), ra_hours) over
   [best - 1.5 days, best + 1.5 days], up to 50 iterations or until
   interval < 0.01 day (standard bisection: if f(t0) <= f(tm) then
   t1 = tm else t0 = tm).
7. Return the refined time truncated to local midnight
   (hour=0, minute=0, second=0).

Timezone handling: the function must work for ANY location automatically
using the system's local timezone (DST-aware), like Python's tzlocal.
In Rust, use `chrono::Local` (Local::now(), Local::from_local_datetime)
or an equivalent. The returned value is a local civil-midnight date.

Unit tests (mirror test_transit_date.py, LON = -74.0, US NJ):
  - transit_date(21 + 42/60.0, -74.0) returns Some; result has
    hour==0 and minute==0.
  - transit_date(1 + 43/60.0, -74.0) returns Some.
  - For ra in [0.0, 6.0, 12.0, 18.0, 23.0]: result (if Some) always has
    hour==0, minute==0.
  - transit_date(23.34, -74.0) returns Some.
  All tests must pass.
```

---

## Prompt 5 — Location detection (GeoLite2)

```
In src/location.rs, implement detect_longitude() -> f64, mirroring
astro_inventory.py:

1. Cache file `.location` in the executable's working directory (or
   next to the data root — use the app's base directory, same location
   the Python script used: directory of the program). If it exists and
   contains a parseable float, return it.
2. Otherwise:
   a. GET https://api.ipify.org (timeout 10 s) -> public IPv4 string.
   b. Open GeoLite2-City.mmdb (file located next to the program, same
      directory as .location) with the mmdb crate.
   c. Look up the IP -> city -> location.longitude.
   d. Write the longitude to .location and return it.
3. On ANY failure (network, file, parse), print a warning
   "WARNING: GeoIP lookup failed: {err}" to stderr and return 0.0.

The longitude is computed once at startup and stored in shared state,
used by transit computation.
```

---

## Prompt 6 — Plan.md parsing, rendering, constellations

```
In src/plan.rs, implement plan.md handling mirroring astro_inventory.py.

1. _parse_plan(text) -> (meta: HashMap<String,String>, body: String):
   - If the first line (trimmed) is not exactly "---", return ({}, text).
   - Otherwise read lines until the next line (trimmed) that is "---".
     Each line containing ":" splits into key/value at the FIRST colon;
     key is trimmed and lowercased, value trimmed.
   - body = the lines AFTER the closing "---", with any line whose
     ltrimmed form starts with "#" removed, joined by "\n", then trimmed.
   - If no closing "---" is found, return ({}, text).

2. render_plan(plan_text) -> String (HTML fragment):
   - Empty/whitespace-only -> "—".
   - Build "top" row:
     * if meta has "constellation": a badge span with the EXPANDED
       constellation name (HTML-escaped).
     * if meta has "link": an anchor styled .plan-link, target=_blank,
       rel="noopener noreferrer"; label is "AstroBin" if the URL
       (lowercased) contains "astrobin", else "Link"; append " ↗".
   - Meta bits (each on its own line, HTML-escaped, joined with <br>):
     "RA {meta[ra]}" if present, "Dec {meta[dec]}" if present,
     "Rot {meta[rotation]}" if present.
   - Body: each line HTML-escaped, URLs (pattern
     https?://[^\s<>"']+ ) wrapped in
     <a href="URL" target="_blank" rel="noopener noreferrer">URL</a>,
     lines joined with <br>.
   - Wrap: <div class="plan"> + optional
     <div class="plan-top">top parts</div> +
     optional <div class="plan-meta">meta bits</div> +
     optional <div class="plan-body">body</div> + </div>.
   - If no parts at all, return the whole text escaped+linkified.

3. Constellation expansion — include the COMPLETE 88-constellation
   abbreviation map from astro_inventory.py (And->Andromeda, Aqr->Aquarius,
   ... Vol->Volans). expand_constellation(value): trim the value, return
   the full name if in the map, else the value unchanged; empty -> empty.
   (Copy the full table verbatim from astro_inventory.py CONSTELLATIONS.)
```

---

## Prompt 7 — Web server: state, index page, sorting, pagination, theme

```
In src/server.rs and src/render.rs, implement the axum server mirroring
astro_inventory_server.py.

Shared state (Arc):
- root: PathBuf (from --root)
- page_size_default: usize (from --page-size, default 20)
- longitude: f64 (from Prompt 5)
- scan cache: Mutex<{records: Option<Vec<ObjectRecord>>,
                     generated: Option<DateTime>, root_mtime: f64}>

Cache invalidation (mirror Python _root_mtime):
- root_mtime = max mtime of top-level entries under ROOT (0 if none/err).
- On each request: if force (query ?refresh=1), or cache empty, or
  current root_mtime != cached root_mtime -> re-run traverse().
- All responses must send headers:
  Cache-Control: no-cache, no-store, must-revalidate
  Pragma: no-cache
  Expires: 0

GET / (index):
Query params: sort (default "latest"), dir (default per-column default),
page (default 1), page_size (default from CLI; 0 disables paging),
refresh=1 to force rescan.

Sortable columns, in display order (key, name, sortable, default dir):
  project   "Project"                                   asc
  final     "Final image"                               asc
  latest    "Latest image"                              desc
  telescope "Telescope"                                 asc
  object    "Object"                                    asc
  transit   "Transit"                                   asc   (sort value:
              transit_sort, or a large number 9999999999 if none)
  total     "Total exposure"                            desc   (total_seconds)
  size      "Size (MB)"                                 desc   (size_bytes/1MB)
  filters   "Filters (count / duration / total)"       not sortable
  masters   "Master images"                             not sortable
  plan      "Plan"                                      not sortable
  actions   "Actions"                                   not sortable
If sort key is invalid/unsortable, fall back to "latest". dir must be
"asc" or "desc" else "asc". Sort the FULL record list server-side, then
paginate.

Pagination:
- page_size == 0 -> single page with all records.
- else pages = ceil(total/page_size); clamp page to [1, pages];
  slice records.

Row rendering (mirror build_rows in astro_inventory.py, but with the
server's image/actions URLs):
- Row class: "red" if !has_project && !has_final; "yellow" if
  has_project && !has_final; else none.
- Project / Final image cells: green ✔ (✔, color #4caf50, bold) or red ✘
  (color #f44336, bold) — sort values "1"/"0".
- Latest image cell: "YYYY-MM-DD HH:MM" or "—"; sort by unix timestamp.
- Transit cell: rec.transit or "—"; sort by transit_sort.
- Total exposure: fmt_total(total_seconds); sort by numeric seconds.
- Size: "{mb:.1}" MB (one decimal); sort by full-precision MB.
- Filters: the rendered filters string from Prompt 3.
- Master images: for each final image file, a thumbnail
  <a href="{img_url}" target="_blank"><img src="{img_url}" alt="{fn}"></a>
  where img_url = "/img/" + percent-encoded path relative to ROOT
  (i.e. <telescope>/<object>/master/<file>). Wrap in <span class="thumbs">.
  None -> "—".
- Plan: render_plan(rec.plan) from Prompt 6.
- Actions: two buttons:
  * "Edit plan" -> opens /edit/<telescope>/<name> in a new tab.
  * "Delete directory" -> JS confirm() with message
    "Delete {telescope}/{object} and all its files? This cannot be undone."
    then submits a hidden form (method POST, hidden input name=confirm
    value=yes) to /delete/<telescope>/<name>.

Page chrome (mirror PAGE_TEMPLATE dark theme EXACTLY — copy the full CSS
from astro_inventory_server.py verbatim):
- Title: "Astro Imaging Tracker" (both <title> and <h1>).
- Meta line: "Root: {root} · Objects: {n} · Generated: {gen}" plus
  "Click a column header to sort. Refresh scan" link (adds ?refresh=1
  preserving current sort/dir/page_size).
- "Add a new object" box (see Prompt 8 for the AstroBin button).
- Pager above AND below the table: Prev/Next, page numbers
  (1, current-1, current, current+1, last with "…" gaps), "Page X of Y —
  N objects", plus a page-size <select> (10/20/50/100/All (no paging))
  that reloads with the chosen ?page_size= preserving sort/dir.
- Sortable column headers are links: clicking a sorted column toggles
  direction; clicking another column uses that column's DEFAULT direction
  and resets to page 1. Arrows: ↑ (sorted asc), ↓ (sorted desc), ↕
  (unsorted), inline after the header text.
- All user-facing strings HTML-escaped.

GET /img/<relpath>:
- Resolve ROOT/relpath with canonicalization; reject (404) unless the
  canonical path is strictly under canonical ROOT and is a regular file.
- Serve the file with correct content type (at minimum image/jpeg,
  image/png).
```

---

## Prompt 8 — Actions: add, edit plan, delete, AstroBin lookup

```
Continue src/server.rs. Mirror astro_inventory_server.py routes.

Flash messages: the Python app uses Flask flash (session-based). In Rust,
implement an equivalent: a signed/cookie-based or query-based mechanism is
acceptable, but the simplest faithful approach is a short-lived cookie
(e.g. name=flash, value=category|message, HttpOnly, path=/, expires in
~10 min) that the index and edit pages render as
<div class="flash {cat}">{msg}</div> right after the add-box (index) or
at top of body (edit page), then cleared. Categories: "ok", "error".

Name validation helper (mirror _resolve_object):
- Reject telescope or object containing NUL, '/', or '\\' and equal to
  "", ".", "..".
- For edit/delete: both ROOT/<telescope> and ROOT/<telescope>/<object>
  must be existing directories; the canonical object dir must be under
  canonical ROOT. On error: flash the error and redirect back to index
  (preserving the referrer's query string if it is the index, else "/").

POST /add:
Form fields: telescope, object, constellation, ra, dec, rotation, sample
(= Link).
- Strip whitespace from all.
- If telescope or object empty -> flash error
  "Please choose a telescope and enter an object name." redirect index.
- Telescope dir must exist under ROOT else flash error
  "Telescope directory not found: {telescope}".
- Object name must pass validation else flash "Invalid object name."
- If ROOT/<telescope>/<object> already exists -> flash error
  "Object directory already exists: {telescope}/{name}".
- Create the directory and write plan.md:
    ---
    constellation: {expand_constellation(constellation)}   (if set)
    ra: {ra}                                               (if set)
    dec: {dec}                                             (if set)
    rotation: {rotation}                                   (if set)
    link: {sample}                                         (if set)
    ---
    (blank line)
    # {name}
    (blank line)
    ## Plan
  (frontmatter block only if at least one field is set; keys in exactly
  that order)
- Flash "Created {telescope}/{name}/plan.md" (ok), force cache refresh,
  redirect index.

GET /edit/<telescope>/<name> and POST /edit/<telescope>/<name>:
- GET: page (same dark-theme shell as index — reuse the CSS) with
  <h1>Edit plan: {telescope}/{name}</h1>, meta line with the full plan
  path, a form POSTing to the same URL with a single textarea
  name="plan" rows=24 cols=100 spellcheck=false (monospace, dark-theme
  styled, background slightly lighter than page bg), containing the
  current plan.md content (HTML-escaped; empty if missing), a Save
  button and a Cancel link back to index.
- POST: write the textarea value to plan.md (UTF-8). On failure flash
  "Failed to save plan: {err}" (error). On success flash
  "Saved {telescope}/{name}/plan.md" (ok), force cache refresh,
  redirect index.

POST /delete/<telescope>/<name>:
- Only valid via POST with hidden field confirm=yes (the hidden form
  from the Actions column). Validate names, then recursively remove
  ROOT/<telescope>/<name> (fs::remove_dir_all). On error flash
  "Failed to delete {path}: {err}". On success flash
  "Deleted {telescope}/{name}", force cache refresh, redirect index.

GET /astrobin/<slug>:
- GET https://www.astrobin.com/api/v2/images/image/?hash={slug}
  with header User-Agent: Mozilla/5.0 (astro-inventory), timeout 15 s.
- On request/parse failure: JSON {"error": "AstroBin API request failed: {e}"}
  with status 502.
- If results array empty: {"error": "No AstroBin image found for slug '{slug}'"}
  with status 404.
- Otherwise take results[0] and return JSON:
  name:          img.title or ""
  constellation: expand_constellation(img.constellation or "")
  ra:            decimal degrees -> "HHhMMmSSs"
                 (deg % 360; total seconds of time = deg*240;
                 h = sec//3600, m = (sec%3600)//60, s = sec%60;
                 format with zero-padded 2-digit h, m, s)
  dec:           decimal degrees -> "+DD°MM′SS″"
                 (sign + or -; d = floor(abs); m = floor((abs-d)*60);
                 s = floor(((abs-d)*60 - m)*60);
                 format "{sign}{d}°{m:02}′{s:02}″")

"Add a new object" form (index page, inside .addbox):
- Fields: Telescope (select of all telescope names found, required,
  placeholder option "— choose —"), Object Name (text, required,
  placeholder "e.g. M31"), Constellation (text, placeholder
  "e.g. Andromeda"), RA (text, placeholder "e.g. 00h42m44s"), DEC
  (text, placeholder "e.g. +41°16′09″"), Rotation (number, step any,
  placeholder "e.g. 45"), Link (text, placeholder
  "e.g. https://app.astrobin.com/...").
- Buttons: "Populate from Astrobin" (type=button) and "Add" (submit).
- JS for the populate button (mirror the Python template):
  read the Link field, alert if empty; extract slug = last non-empty
  path segment of the URL; alert if missing; disable button, text
  "Loading..."; fetch('/astrobin/' + encodeURIComponent(slug)); on
  success fill object/constellation/ra/dec fields from the JSON (only
  non-empty values); on failure alert('AstroBin lookup failed: ' + msg);
  finally restore the button.
```

---

## Prompt 9 — CLI, startup, verification

```
Finalize src/main.rs and verify the whole application.

main.rs:
- Parse CLI: --root (required, no default), --page-size (default 20).
- At startup: run detect_longitude() (Prompt 5) — this may touch the
  network once; on failure it must fall back to 0.0 with a warning,
  never crash.
- Build shared state, spawn axum server on 0.0.0.0:5000.
- Log the listen address.

Create start_server.sh (executable) mirroring the original:
  #!/usr/bin/env bash
  set -euo pipefail
  cd "$(dirname "$0")"
  echo "Starting astro inventory server on http://127.0.0.1:5000 ..."
  exec ./target/release/astro_inventory --root /data/Astro/CCD 2>&1 | tee -a server.log

Verification checklist — run through ALL of these against a real data
tree (or a small fixture tree you create) and fix any mismatch:
 1. Server starts with --root pointing at a fixture tree containing:
    - telescope A: object with FITS files covering all three duration
      patterns and all 7 filter letters plus a no-filter (DSLR) file,
      a .cr2 file, a master/ dir with a .jpg and a .pxiproject dir,
      and a plan.md with YAML frontmatter (constellation abbreviation,
      ra, dec, rotation, link) and a body line containing a URL.
    - telescope A: object with NO project and NO final (row must be red).
    - telescope A: object with project but no final (row must be
      yellow).
    - an object dir named "flats" (must be skipped).
 2. Index page shows all rows; verify each column's values against
     hand-computed expectations (counts, durations, totals, size MB,
     latest timestamp, transit date).
 3. Sorting: click every sortable column header via URL params
     (?sort=...&dir=...); verify asc/desc and default directions;
     verify invalid sort key falls back to "latest".
 4. Pagination: with page_size=2 and 5 records -> 3 pages; page
     clamping; page_size=0 shows all; page-size select present and
     functional; pager appears above and below the table.
 5. ?refresh=1 forces a rescan (touch a file, refresh, see change).
 6. /img/ serves a master jpg; /img/../etc/passwd and any path
     escaping ROOT return 404.
 7. Add form: creates dir + plan.md with correct frontmatter;
     duplicate name rejected; unknown telescope rejected; invalid
     object name (e.g. "a/b") rejected. Flash messages appear.
 8. Edit plan: GET shows content; POST saves; file on disk matches.
 9. Delete: hidden-form POST removes the directory; it disappears from
    the index after redirect.
 10. /astrobin/<slug> returns correct JSON shape (test against the
     real API or a mocked response).
 11. Transit unit tests from Prompt 4 pass.
 12. No-cache headers present on all HTML responses.
 13. `cargo build --release` succeeds with no warnings;
     `cargo test` passes.
```

---

## Notes for the implementing agent

- The Python files in this directory are the reference implementation.
  When in doubt about a behavior (an edge case, a format, a message
  string), open `astro_inventory.py` / `astro_inventory_server.py` and
  match them exactly.
- Keep all user-facing strings byte-identical to the Python version
  (column names, flash messages, button labels, placeholders, CSS
  class names).
- The CSS block in `astro_inventory_server.py` (PAGE_TEMPLATE /
  PAGE_SHELL) must be reproduced verbatim — the dark theme is part of
  the required output.
- CR2/CR3 files always count as 30 s exposures under the
  "DSLR (CR2/CR3)" label.
- The transit computation uses CIVIL midnight (local wall-clock
  midnight, DST-aware), not solar midnight.
