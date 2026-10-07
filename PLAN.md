# Staging Review Slideshow with Auto-Stretched FITS Previews

## Summary
Add a **staging review workflow** to the active Rust/axum server (`src/`): a config file declares the CCD repository root, a staging area, and a cache area; the user picks **one staging object directory** (input) and **one `CCD/<telescope>/<object>` directory** (output), browses the input's images in a keyboard-driven slideshow grouped by filter and sorted by a free noise-based quality metric, with auto-stretched FITS previews that have a native 1:1 200×200 detail inset baked in, marks images for **push** (move to the chosen output dir) or **delete**, and a final summary page applies all marks in one batch — then purges the previews it just made.

**Status: implemented, tested (61 unit tests), and running on `:5000`.** This document now describes the
as-built system. Sections that changed during implementation are marked **⚠ as-built**; the measured
baseline in §0 is unchanged because none of it was invalidated.

**⚠ as-built — the staging screen is now a *generic review screen* (§9).** `src/staging.rs` is
`src/review.rs`, the routes are `/review*`, marks are opt-in, and the actions are move / copy /
symlink / delete on **any** directory under a configured root. §3, §4, §5 and §6 below describe the
original staging-only design and are superseded where they conflict with §9.

## 0. Measured baseline (from real data)

Every design decision below is grounded in measurements against the actual corpus.

**Staging sample (`/ssd/sync`, 1,763 FITS · 3,636 files · 63 GB):**

| Property | Measured |
|---|---|
| Structure | `<staging>/<telescope>/<object>/*.fits` — mirrors `CCD/<telescope>/<object>` |
| Telescope dirs | `Esprit` (418) · `GT81` (17) · `Pier` (959) · `RC` (368) · `SW` (0) · `targets` (0) |
| Largest object dir | `Pier/Jacoby1` — 740 FITS, 15 GB |
| FITS format | 100% `BITPIX=16`, `NAXIS=2` |
| Dimensions / size | 6224×4168 (26 MP), **51.9 MB** each, header 5,760 B (2 blocks) |
| Header scaling | **`BZERO = 32768`, `BSCALE = 1`** (signed int16 storing unsigned range) |
| Header metadata | `FILTER='H       '`, `IMAGETYP='Light   '`, `EXPTIME=300` (quoted, space-padded) |
| Real stats (Ha subs) | sky 307–314 ADU, σ 11.9–13.3, `sky/σ` 23.5–26.3; stars saturate at 65535 |
| Syncthing | `.stfolder` in every telescope dir, **no `.stignore`** — actively synced from capture machines |
| Non-image content | `SW/` is C++/Arduino source with `.git` repos; ~40 loose top-level files (`.esq`, `.sh`, `.deb`, `.swp`, `.jpg`); dot-dirs `.astropy .config .dbus .indi .local .PHDGuidingV2` |
| Calibration dirs | `Pier/Darks`, `Pier/Flats`, `Pier/DarksGood` present at object level |
| `.xisf` | **none in staging** (only in `CCD/*/master/`, up to 943 MB) |
| `.fz` | **none** |

**Telescope names do not map 1:1 staging → CCD** (`Esprit`→`Esprit100`, `GT81`→`81GT` transposed, `RC`→`8RC` **or** `10RC` ambiguous, `Pier`→*no CCD counterpart*). This is why the output directory is **chosen explicitly by the user** rather than derived.

**Cost measurements:**

| Operation | Measured |
|---|---|
| `/ssd` sequential read | **1.6 GB/s** |
| Strided stats pass (every 5th row, every 2nd pixel = 2.6M samples) | 68 ms/file in Python/numpy → **~15–25 ms/file estimated in Rust** |
| Strided read volume | **10.4 MB of 51.9 MB** per file (~20%) |
| Full decode + full-frame median (numpy) | 664 ms/file — *inflated by numpy's full sort; not the approach taken* |
| ImageMagick full pipeline (reference only, **not used**) | 0.44 s wall / 3.06 s CPU (OpenMP) |

## 1. Configuration file
- New `config.json` in the project directory (JSON — reuses existing `serde_json`):
  ```json
  {
    "root":           "/data/Astro/CCD",
    "staging":        "/ssd/sync",
    "cache":          "/home/alex/.cache/astro_inventory",
    "page_size":      20,
    "compendium_dir": "data/compendium"
  }
  ```
- **`cache` MUST live outside the staging tree.** Every staging telescope dir carries a Syncthing `.stfolder`; anything written inside staging is replicated to the capture machines. Preview JPEGs for one object dir are 300–500 MB, so an in-staging cache would push hundreds of MB around the mesh.
- Ship `config.example.json`; add `config.json` to `.gitignore` (machine-local paths).
- `main.rs`: `--config <path>` flag. `--root` and `--staging` are `Option<PathBuf>` (they are required args today). Resolution order: **CLI flags > config file > built-in default**.
- **⚠ as-built — config auto-discovery.** `--config` was made optional and `./config.json` is picked up automatically when the flag is omitted (`config_path_in`, split out for testability). This was necessary: `start_server.sh` passes only `--root`, so an explicit-flag-only design left staging **silently disabled** in production — the first restart served `404` on `/staging`. `start_server.sh` is unchanged.
- **A typo'd `--config` path is a hard error**, not a silent fall back to defaults.
- **⚠ as-built — `--port` flag added** (default `5000`) so a test instance can run without killing production.
- `AppState` gains `staging: PathBuf` and `cache: PathBuf`. Note the constructor **already takes 5 args** (`root, page_size, longitude, latitude, compendium_dir`) after the concurrent Compendium work, so this touches more of `main.rs` than it looks.
- **⚠ as-built — two more config keys** (§9): `nav_repeat_ms` (autorepeat throttle, default **500**, CLI `--nav-repeat-ms`) and `review_roots` (extra directories the generic review screen may open, CLI `--review-roots` as a comma-separated list). Review roots are **validated at startup**: a root that does not exist is a hard error, not a silently ignored entry.

## 2. FITS preview renderer (new module `src/fits_preview.rs`)

**Pure Rust — no ImageMagick.** Chosen for future expansion (PNG/TIFF/XISF/RAW decoders can be added in-tree without a system binary, version drift, or a process boundary). Cost: IM's 0.44 s came from OpenMP, so `rayon` is required to stay competitive.

**Module placement:** the crate is now **lib + bin** (`src/lib.rs` declares the modules; `src/bin/extract_compendium.rs` is the second binary). Declare `fits_preview` in **`src/lib.rs`**, not `src/main.rs` — this is also what makes the §7 unit tests straightforward.

### 2.1 Reader (hand-rolled, no FITS crate)
- Parse 2880-byte header blocks until `END`; locate the first IMAGE HDU (primary or extension).
- `BITPIX` 8, 16, 32, −32, −64, **big-endian**.
- **Read exactly `NAXIS1 × NAXIS2 × bytes_per_pixel` — never "the rest of the file".** The data is padded to a 2880-byte boundary; these files carry **2,816 B of trailing filler**. Reading to EOF appends 1,408 garbage pixels and breaks any reshape.
- **`BSCALE`/`BZERO` application is mandatory, not optional.** Every real file carries `BZERO = 32768`; skipping it turns a ~310 ADU sky into ~−32458 and the entire stretch collapses. Apply **before** any statistics.
- Tolerate blank/unparseable card values (`BZERO   =` with no value) → default `BSCALE=1`, `BZERO=0`.
- Read `FILTER`, `IMAGETYP`, `EXPTIME` from the header while parsing (values are quoted and space-padded — strip quotes and trim).
- 2-D mono and 3-D RGB (`NAXIS3 == 3`, plane order per `PIXTYPE`/`CTYPE3`, fallback R,G,B).
- Skip BINTABLE-only files; `.fz` out of scope (none exist in the corpus).

### 2.2 Auto-stretch (fixed, no sliders) — Siril MF'CCD-style
1. Subsample pixels (stride to ≤ ~1M samples).
2. Iterative sigma-clipped **median** and **MAD** → robust background σ.
3. Black = `med − 2·MAD·1.4826`; white = **`max(med + 8σ, 99.99th percentile)`**.
   The `max()` is load-bearing: on real Ha data `med+8σ ≈ 373` ADU while stars saturate at 65535 — without the percentile term the white point is catastrophically low.
4. Normalize to [0,1], midtone transfer `m' = m(1−t)/(1−t·m)` with `t` chosen so the median maps to ≈ 0.30.
5. RGB: compute stats and stretch **jointly across channels** to preserve colour.
6. `rayon` over the stats and stretch passes.

### 2.3 Two passes, deliberately separate
- **Stats pass (cheap, eager per directory).** Strided `pread` of every 5th row at full x-resolution, every 2nd pixel → 2.6M samples, reading only **10.4 MB of 51.9 MB**. Median/MAD via quickselect (not full sort). **~15–25 ms/file.** Memory O(1) (~10 MB scratch).
  - `Pier/Jacoby1` (740 files) → **11–19 s** one-time; `RC/Sh2-124` (263) → 4–7 s; `Esprit/lbn515` (179) → 3–4 s.
  - I/O floor: 740 × 10.4 MB = 7.7 GB at 1.6 GB/s ≈ 5 s.
  - Results cached (§2.5) so re-opening a directory is instant.
- **Render pass (expensive, lazy per viewed image).** Full decode → stretch → 8-bit array → downscale → inset composite → JPEG.

### 2.4 Quality metric — the cheapest one (v1)
**`sky/σ`**, computed directly from the stats pass. **Zero extra cost** — no decode, no star photometry.

- **⚠ as-built — sort key: `σ` ascending** (lowest noise first), within `(IMAGETYP, FILTER)`. `sky/σ` and `median` remain selectable.
- Why it changed: `sky/σ` turned out to be dominated by **sky level**, not noise. In `Pier/Jacoby1` the `Ha` group spans median 302–4620 and σ 10.4–391, so the ratio mostly re-reported how bright the sky had gone. Within one group at fixed exposure, σ alone is the cleaner ranking and is simpler to explain.
- Discriminates on real data: the first `Jacoby1` sub scores **23.5** (σ 13.3) vs **26.1–26.3** (σ 11.9) for the rest at the same sky level — a genuinely noisier sub, correctly flagged.
- **Known blind spot (accepted):** `sky/σ` cannot detect tracking failures or star trailing — a sub with ruined stars but normal noise ranks as good. The 1:1 inset in §2.5 is the mitigation: the metric ranks, the eye catches what it can't.

### 2.5 Preview output and the baked-in detail inset
- **Display JPEG: longest side capped at 1500 px** (6224×4168 → 1500×1005), quality ~88 via the `image` crate. ≈400–700 KB/image; `Pier/Jacoby1` ≈ 300–500 MB of cache.
  - Accepted tradeoff: 1500 px is narrower than a 1920-wide viewport, so the fit-to-window view is CSS-upscaled ~1.28×. The inset carries the true 1:1 detail.
- **A 200×200 native 1:1 crop is composited directly into the preview JPEG.** No separate endpoint, no canvas, no client-side overlay — the inset is baked once and costs nothing on repeat views.
  - **⚠ as-built — anchor: the brightest *round, non-saturated* source, ranked by aperture flux**, not the geometric centre. Measured: the geometric centre of these subs peaks at only **~700 ADU above sky (≈sky+12σ)** while the brightest star is **~30,000 ADU and sits 1,339–3,462 px away** — a centre crop would show near-blank sky.
  - The original "brightest non-saturated pixel" rule **picked hot pixels**, which is what it is now designed against. As built:
    1. Scan 16×16 blocks for **local maxima** above `median + 20σ`, skipping saturated pixels individually (a saturated core must not disqualify the star it belongs to).
    2. Reject candidates failing a shape test: **≥ 5 pixels above floor** in a 5×5, **and** the brightest immediate neighbour **≥ 20% of the peak**. The neighbour test is the load-bearing one — a summed ring is not enough, because on cloud frames σ reaches ~390 ADU and 24 noise pixels then sum past any absolute threshold. A single neighbour cannot be faked: noise gives ~2.5σ, a hot pixel gives nothing.
    3. Rank by **total light in a 9×9 aperture**, not by peak. Peak ranking is what found junk: on the cloud-affected 2×2-binned frames the highest peak belongs to a cosmic-ray track (six ~30,000 ADU pixels with sky between them, flux ~130,000) while the real star has a lower peak and **~613,000** of flux.
    4. Centre the crop on a **flux-weighted centroid** over a 15×15 window (wider than the ranking aperture: ranking wants compact, centring wants the whole PSF).
  - **Measured on 12 frames spanning the σ range: 8/12 land squarely on a star, 0 hot pixels.** The 4 weak ones are all σ 332–391 cloud frames — see §8, they are a *stretch* artifact, not an anchor miss.
  - Tried and rejected: a second-moment elongation test to reject cosmic-ray chains. It does not separate (0.001–0.097 for both chains and genuine stars), so it was dropped rather than shipped as a knob tuned on 7 frames.
  - The star scan is **free in this pass**: it runs on pixels the render pass already holds (~3–10 ms with `rayon`).
  - Paste bottom-right with a 1 px border. The inset is true 1:1 while the host image is downscaled ~4.15×, so star cores read clearly.
  - Optional: outline the inset's source region on the main image (200 source px → ~48 px box at 1500 px scale) so the user knows where they are looking.
- **No in-memory array cache.** Nothing re-crops after the preview is written, so the previously-planned LRU of stretched full-res arrays (≈78 MB resident) is dropped entirely.

### 2.6 Cache
- Rendered display JPEGs and per-file stats live under `<cache>/`, keyed by `hash(RENDER_VERSION + path + mtime + size)`.
  - **`RENDER_VERSION` is in the key** (`staging.rs`) so an anchor or stretch change invalidates every stale preview at once. It has been bumped twice as the anchor algorithm changed (now `5`).
- **Orphaned entries are resolved by the apply-time purge (§3)** — no content-derived key and no eviction machinery needed for the normal workflow.
- Still open: eviction for directories that are browsed but never applied. Flagged, not solved in v1.

## 3. New routes (`src/server.rs`)

**⚠ as-built — all routes below are `/review*`** (`/review`, `/review/manifest`, `/review/preview`,
`/review/destinations`, `/review/destfiles`, `/review/newdest`, `/review/apply`); `/staging` survives
as a launcher that lists staging dirs and links into `/review?dir=`. See §9 for the current shape.

| Route | Behavior |
|---|---|
| `GET /staging` | **Two-level browse**: telescope dirs → object dirs. Object dirs are the selectable unit. Dirs with 0 images filtered out. **Calibration dirs (`Darks`/`Flats`/`DarksGood`) are shown** — they are reviewable and importable. `.git`, `.stfolder`, and all dot-dirs skipped. **All four columns (`Object`/`Telescope`/`Frames`/`Size`) are sortable** — client-side, keyed off raw `data-bytes`/`data-frames` numbers because ordering by the display string puts `4.1 kB` below `64 B`; counts and sizes open high-first, names A→Z. Link added to the index page header. |
| `GET /staging/review?dir=<abs>` | Slideshow page. `dir` must resolve under the staging root **and** be an object dir. |
| `GET /staging/manifest?dir=<rel>` | JSON: per-file `{path, filter, imagetype, exptime, median, sigma, sky_over_sigma}`. Drives grouping, sorting, and the progress indicator. Runs the stats pass with progress. |
| `GET /staging/preview?path=<abs>` | Stretched display JPEG (with baked inset) for FITS; original bytes for jpg/png. |
| `GET /staging/destinations` | JSON list of `CCD/<telescope>/<object>` dirs — **420 live**. **Use a cheap two-level `read_dir`, NOT `get_records()`** — that path holds a `std::sync::Mutex` across a traverse measured at **47.8 s** in `server.log`. Enumeration costs ~3 ms. Note `read_dir` + `is_dir()` follows symlinks, so this count exceeds `find -type d` (417) by the 3 symlinked object dirs.
| `GET /staging/destfiles?dir=<abs>` | Basenames already present in a destination, for the collision pre-check. |
| `POST /staging/newdest` | `{telescope, name}` → creates `CCD/<telescope>/<name>` **plus its `plan.md`**, returns the `Destination`. Shares `create_object()` with the index `POST /add`, so a destination made here is indistinguishable from one made there — `traverse.rs` reads `plan.md` for RA/Dec and the latest-image fallback, so a bare directory would behave differently in the inventory. |
| `POST /staging/apply` | JSON `{pushes: [{src, dst}], deletes: [src]}`; validates every src under staging, every dst under root **and existing**; executes; purges cache; returns per-item success/failure JSON. |

- **⚠ as-built — path guards deviate.** Query params carry **absolute** paths, not relative ones. They are validated by `resolve_within`, which canonicalizes the **parent** and then re-joins the leaf — that ordering is deliberate: canonicalizing the whole path first would let a symlinked leaf escape, and joining first lets `..` escape. Verified: a preview path outside staging and a destfiles path outside the CCD root both return `400`.
- **Apply semantics:**
  - Moves use `rename` with cross-filesystem copy+delete fallback.
  - Name collisions at destination are reported as failures — **never overwrite**.
  - Deletes are **permanent `remove_file`** (see §6).
  - `apply` **never creates destination directories** — the dropdown only offers existing `CCD/<telescope>/<object>` dirs; a missing one is an error, not a silent `mkdir`.
  - **⚠ as-built — creating one is a first-class action on the review page**, via `＋ new destination…` at the end of the dropdown: an inline row with a telescope select (the CCD prefixes already in the list) and a name field **prefilled with the staging object's basename** — the common case is `Pier/Jacoby1` → `81GT/Jacoby1`, one Enter press. On success the list refreshes, the new dir is selected, and collision pre-check runs against it. Creation goes through `create_object()`, shared with the index "Add a new object" form, so the new object gets the same `plan.md` and behaves identically in the inventory.
  - Two deliberate limits: **new telescopes are not offered** (that stays on the index form — and `Pier` has no CCD counterpart at all, so you pick `81GT` or similar), and **`apply` still never creates a directory**. Creation is explicit and visible, so a typo cannot quietly scatter a batch into a fresh dir.
  - Runs in `tokio::task::spawn_blocking` behind a `Semaphore(1)` render lock, with in-flight dedup on the cache key. **Never do FITS decode / stretch / large file moves inside an `async fn`** — it stalls a tokio worker.
  - **Must invalidate the scan cache: `cache.records = None`.** Note that `root_mtime()` (`server.rs:53`) only stats entries of `root`, so a file moved into `CCD/10RC/M57/` bumps `M57/`'s mtime, not `10RC/`'s or `CCD/`'s — the cache cannot detect pushes on its own. Every other mutator already nulls the cache explicitly.
  - Partial failures are listed on the summary page, not rolled back. Re-applying must skip already-moved sources.
- **Cache purge on apply:**
  - **Snapshot the cache keys *before* mutating anything** — the key includes `mtime + size`, and once a file is moved its stat is gone.
  - After each successful item, delete its display JPEG **and** its manifest/stats entry.
  - If the directory ends up empty, drop its whole manifest entry so a re-open shows an empty dir instantly.
  - Pushed files are **not** re-keyed to their new CCD path — the preview is simply re-rendered if it is ever viewed from the index.

## 4. Slideshow UI (`src/render.rs`, embedded HTML/JS)

**⚠ as-built — the mark model is inverted** (mark = act, `Space` toggles, `A`/`N`/`I` mark all /
none / invert) and the action is chosen from an op select. See §9; the `Space` = skip / `z` = delete
rows below are superseded.

- **Single page: JS swaps `<img src>`.** No per-image server navigation — otherwise in-memory mark state dies on every arrow press.
- Large centered image (fit to window), filename + index counter (e.g. `7 / 134`), current mark badge (PUSH → dest / DELETE / none). The 1:1 detail inset is already inside that image.
- **Grouping:** images grouped by `(IMAGETYP, FILTER)` with group headers, e.g. `Light · H (263)`. Prefer header values (`FILTER`, `IMAGETYP`) over the filename regex; fall back to `parse_filter` (`RE_FILTER = _([LRBGSOH])_`) and `FILTERS`/`filter_label` from `traverse.rs` for consistency. Navigation walks within a group or across all.
- **⚠ as-built — Sorting:** `σ` ascending (default) / `sky/σ` / `median`, always **within `(IMAGETYP, FILTER)`**. Sorting a mixed-filter dir by any of these is meaningless (Ha sky ≫ L sky).
- **Destination dropdown (output dir):** one session-level choice, not per mark. Default suggested by case-insensitive **basename match** of the staging object dir against CCD object dirs (a fuzzy default, no alias table). Shown in the header; re-validated server-side at apply.
- **Collision pre-check:** list the destination's filenames at load and flag marked files that would collide, *before* apply.
- **Keyboard:**
  | Key | Action |
  |---|---|
  | `Space` | **skip** this frame (exclude from push), then advance |
  | `z` | toggle delete-mark, then advance |
  | `←` / `→` (or `h` / `l`) | navigate — **hold to scroll** |
  | `U` | clear the mark on the current frame (back to default push) |
  | `G` | summary / apply page |
  - **⚠ as-built — push is the default.** Every frame pushes unless marked `del` or `off`, so `Space` *excludes* rather than includes. The culling gesture becomes "flag the duds", and a 740-frame directory needs **zero** marks. Badges: `PUSH → dest` / `DELETE` / `SKIPPED — will not push`.
  - `z` for delete is deliberate: it is far from the arrow cluster, which matters because deletes are permanent.
  - **`Space` gotchas:** call `preventDefault()` (Space scrolls the page and activates focused buttons); **blur the destination `<select>` after choosing** (Space on a focused select opens it, and it also swallows the arrow keys).
  - **⚠ as-built — autorepeat is selective.** A blanket `e.repeat` guard made browsing painful. Repeat is now allowed for the arrow/`h`/`l` keys but throttled to **120 ms**, so the OS repeat rate cannot outrun image loading (a cold preview is ~270 ms and renders are serialised). `Space` and `z` keep the guard so marks can never machine-gun.
- **Marks persisted to `localStorage`** keyed by staging dir, so a reload, tab discard, or crash cannot lose them while deletes are permanent.
  - **⚠ as-built — only exceptions are stored** (`del` / `off`); absence means push. Storage key is `stagemarks2:` and any legacy value that is not `del`/`off` is discarded on load, so an old all-explicit mark set cannot leak into the new model.
- **Manifest progress indicator** during the stats pass (up to ~19 s for the largest dir).
- **Summary page** (client-rendered before POST, then server result): two lists — "Move N files → destination" and "Delete N files" — with a single confirm button; after apply, shows per-item results and re-fetches the (now drained) directory.
  - **⚠ as-built — only the exceptions are listed.** With push as the default, "Move N of 740" is stated as a count and the tables show just the skipped and deleted frames; otherwise every directory would render a 740-row table nobody reads. Apply is **two-step armed** ("Confirm — this cannot be undone") because deletes are permanent and propagate over Syncthing.

## 5. Behaviour changes / additions
- Index page gains a "Review staging" link; everything else unchanged.
  - **⚠ as-built — every object row on `/` carries a `Review` button** (`window.open('/review?dir=<abs>')`),
    and the header links `/review`. See §9.
  - Note: `index()` passes `flashes = String::new()` (`server.rs:218`), so post-apply feedback cannot appear on `/`.
- New Rust dependencies: `image` (`default-features = false, features = ["jpeg"]` — it is **not** in `Cargo.lock` today, so the build needs network) and `rayon`.
- Nothing derived is ever written inside the staging tree (Syncthing shares).

## 6. Explicit assumptions and accepted risks
- Implementation is in the **Rust server only**; the Python Flask server is untouched (legacy).
- **Unit of work is one staging object directory + one explicitly chosen CCD destination.** The whole staging area is never processed.
- **Calibration dirs are first-class reviewable content** — shown so they can be imported as well as deleted.
  - Known behaviour, not a bug to fix here: `traverse.rs:260` skips object dirs named `darks`/`cal`/`calibration`/`flats`, and `CCD/calibration` already exists — so files pushed into some calibration destinations **will not appear in the index report**.
- **Deletes are permanent (`remove_file`).** Accepted risk, recorded explicitly:
  - Batch confirmation is the only guard.
  - Staging is Syncthing-synced (`.stfolder` present), so a delete **propagates to the capture machines**.
  - A `trash/` directory outside staging was considered and declined.
- **Server binds `0.0.0.0:5000` with no auth and no CSRF token.** Accepted risk. JSON request bodies make classic form-CSRF awkward, but any LAN host can still drive `POST /staging/apply`.
- **`sky/σ` is blind to tracking/trailing** (§2.4). The 1:1 inset is the manual mitigation.
- `.cr2`/`.cr3` listed in the manifest with a "no preview" placeholder (no RAW decoder); still markable. (CCD holds 2,625 CR3 + 711 CR2; staging holds none.)
- `.xisf` out of scope — it appears only in `CCD/*/master/` (up to 943 MB), never in staging. A hand-rolled FITS reader will not read it.
- Staging scanner skips `.git`, `.stfolder`, `*.swp`, `*.esq`, and dot-dirs; **only image files may ever be marked**. `SW/` is a C++/Arduino source tree with `.git` repos and must never be touched.
- **Concurrent work in this repo:** an unrelated Compendium feature (`calamine`/`zip`/`quick-xml` deps, `src/compendium.rs`, `src/lib.rs`, `/compendium*` routes, `--compendium-dir`/`--latitude`) is in flight on the same files this plan touches — `main.rs`, `server.rs`, `render.rs`, `Cargo.toml`. Expect merge friction; re-check the line anchors in this document before implementing.

## 7. Testing & verification
- **Unit tests (`cargo test`)** — **61 pass as built** (24 lib + 35 bin + 2 compendium). No HTTP-level tests, so no `tower` dev-dependency is needed (the crate has zero dev-deps today):
  - FITS round-trip on synthetic files written in-test: BITPIX 16 mono, −32 RGB, BINTABLE-skip, truncated-file error.
  - **`BZERO = 32768` round-trip asserting the sky median stays positive** (guards the single most important line in the reader).
  - **Block-padding test:** a synthetic file with trailing 2880-boundary filler reads exactly `NAXIS1×NAXIS2` pixels, with no garbage appended.
  - Blank/unparseable `BSCALE`/`BZERO` cards default to 1/0.
  - Header `FILTER`/`IMAGETYP` parsing (quoted, space-padded) takes precedence over the filename regex.
  - Stretch sanity: background near-black, bright star near-white, midtone median ≈ 0.30; and the `max(med+8σ, p99.99)` guard on a synthetic dataset shaped like the real Ha data (sky ~310, σ ~12, saturated stars).
  - `sky/σ` ordering test, and grouping by `(IMAGETYP, FILTER)`.
  - **Inset anchor tests:** picks a real star over the geometric centre; skips saturated peaks; **rejects a lone hot pixel**; **rejects a 2×2 hot cluster**; returns the **flux centroid** rather than the highest pixel; keeps an unsaturated star sharing a block with a saturated one; and **prefers a star over a higher-peak cosmic ray** (the real-data regression).
  - **`render_review_index` tests:** every column exposes a `data-key`, rows carry raw `data-bytes`/`data-frames` next to human-readable display text, and the empty case still renders sortable headers.
  - **`render_review` tests:** the configured `nav_repeat_ms` reaches the page (`data-repeat`, the hint text, and no unsubstituted placeholder), all four operations are offered, and every mark gesture (`Space`/`A`/`N`/`I`/`U`) is bound; the confirmation string carries operation + quantity.
  - **Review listing guards:** dot-directories (`.stfolder`, `.config`, `.syncthing.*`) are never offered; telescope-level rows must not double-count their object dirs; **the cap is per root** so a huge first root cannot crowd a second root out of the list; destinations cover every root and are labelled root-relative.
  - **Apply-op tests:** move, copy, symlink (absolute target), delete; **clobber refusal** for all three write ops; destination must exist and must be inside a root; sources must resolve inside the reviewed dir; `delete` needs no destination.
  - **`create_object` tests:** creates the dir and writes `plan.md` (with and without front matter); rejects `..`, `.`, `/`, `\`, NUL and blank names; rejects a telescope that is not a directory or contains a separator; refuses to adopt an existing directory; and creates nothing at all when a name is rejected.
  - Path-guard helper unit test: escape via `..`, absolute paths, symlink escape → rejected.
- **Manual scenarios:**
  1. `/staging` lists telescope dirs then object dirs, including `Pier/Darks`/`Flats`, excluding `SW` (0 images) and all dot-dirs.
  2. Open `RC/Sh2-124` (263 files): stats pass completes in ~5 s with a progress indicator; images group as `Light · H`.
  3. Slideshow over a dir with mixed FITS + JPG: FITS appear stretched (stars visible, background not clipped), JPGs appear as-is.
  4. Every FITS preview shows a sharp 200×200 1:1 inset anchored on a bright star (star cores round, no downscale blur) — not on blank centre sky.
  5. Sort by `sky/σ` within `Light · H` puts the noisier subs last (on `Jacoby1`, the first sub should rank worst at 23.5).
  6. Mark 3 push + 2 delete, change destination mid-session, apply: files moved into the chosen `CCD/<tel>/<obj>`, deletes removed, collisions flagged before apply and reported without overwrite.
  7. Push into a destination that does not exist → rejected with a clear message pointing at the add-object form.
  8. After apply, the cache entries for every processed file are gone; re-viewing a moved file from staging 404s.
  9. Re-open an untouched directory: manifest and previews load instantly from cache.
  10. Reload the page mid-session: marks are restored from `localStorage`.
  11. After apply, `/` reflects the pushed files (scan cache invalidated).
  12. Space does not scroll the page or open the destination select; a held Space marks exactly one image.
  13. Path traversal attempts (`?dir=../`, absolute paths) return 404.
  14. Nothing new appears inside the staging tree after browsing (Syncthing safety).
  15. Create a destination from the review page, then push into it in the same session.
- **⚠ as-built — the generic screen was verified end-to-end on a throwaway fixture** (`--config /tmp/rev_config.json --port 5055`, never production): move, copy, symlink, delete, clobber refusal, missing destination (message points at `＋ new destination`), source escape and destination escape (`400`), cross-root move, `newdest` creation + `plan.md`, and `--review-roots` / `--nav-repeat-ms` overrides (including the hard startup error when a review root does not exist). Both embedded JS scripts pass `node --check`.
- **⚠ as-built — `newdest` verified end-to-end on a throwaway instance** (`--root /tmp/e2e_ccd --port 5055`, never production): created `81GT/Jacoby1` + `plan.md`, appeared in `/staging/destinations`, and a push moved a file into it. All seven hostile payloads (`../RC`, `a/b`, `..`, blank, a telescope containing `/`, a nonexistent telescope, a duplicate) returned `400` and left the tree unchanged. Bug caught by the unit tests: `resolve_within` canonicalizes the **parent**, so passing a relative `<tel>/<name>` candidate resolved against the process CWD instead of the CCD root — the candidate must be absolute.
- **⚠ as-built — verified against live data** (`/ssd/sync`, `Pier/Jacoby1` 740 frames / 15.7 GB): cold manifest **3.95 s**, warm **13 ms**; cold preview **268 ms**, warm **19 ms**; grouping correct as `Light·Ha (621)` + `Light·Luminance (119)`. Push moved the file and refused a collision without clobbering; delete removed the source permanently; the manifest drained and the preview cache purged (3 → 1 → 0). `find /ssd/sync -newermt '-10 minutes' -type f` stayed empty. Bugs found and fixed this way: telescope rows double-counting object dirs, dot-dirs offered as telescopes, `&` in a directory name truncating the `dir=` query, and the cache purge not firing because the manifest was not loaded before the key snapshot.

## 8. Deferred — measured, if revisited

Recorded so the numbers are on the record and re-deciding does not require re-measuring.

| Feature | Measured cost | Why deferred |
|---|---|---|
| **Stretch on high-σ frames** | Not an anchor bug: on σ 332–391 cloud frames `max(med+8σ, p99.99)` resolves to `p99.99`, which is saturated, so a genuine 12,000 ADU anchor star maps to ~17% grey and the inset *looks* empty. 4 of the 12 sampled frames. | The anchor is correct; the white point needs clipping when saturation is present. Main remaining quality gap. |
| **Star SNR sort** (aperture photometry on ~25 stars) | photometry only **29 ms/file**; needs the full decode → **~60–90 ms/file** → `Pier/Jacoby1` 44–66 s, `RC/Sh2-124` 16–24 s (~4× the stats pass) | `sky/σ` is free and already discriminates |
| **Star elongation** (trailing detection) | free alongside star SNR; measured 0.97–1.04 on `Jacoby1` (round stars, good guiding) | Needs the star pass above |
| **Interactive zoom / variable inset extent** | would need a separate crop endpoint + a cache-key dimension, or the dropped full-res array LRU (~78 MB) | Baked-in fixed 200×200 inset covers the need |
| **Cache eviction** for browsed-but-never-applied dirs | — | Apply-time purge covers the normal workflow |
| **RAW (CR2/CR3) previews** | CCD holds 2,625 CR3 + 711 CR2; staging holds none | No decoder; files remain markable |
| **`.xisf` previews** | masters up to 943 MB | Only in `CCD/*/master/`, never in staging |

## 9. Generic review screen (as-built)

The staging workflow was generalised: the same screen now reviews **any** directory, so a CCD object
dir, a calibration dir, and a staging dir are all handled by one code path with one mark model.

**Module:** `src/staging.rs` → `src/review.rs` (`git mv`, history preserved). Exports `Op` (Move /
Copy / Symlink / Delete), `ApplyReport`, `ReviewDir`, `Destination`, `resolve_within`,
`resolve_within_any`, `list_review_dirs`, `list_destinations`, `list_dir_files`, `scan`, `apply`.

**Roots.** The reviewable set is `CCD root + staging + review_roots` (config array, CLI
`--review-roots`). Every path in every endpoint is validated against that set:
- sources must resolve inside the **reviewed directory** (`resolve_within`);
- destinations must resolve inside **any root** and must already exist (`resolve_within_any`);
- previews are served for any root, not only the CCD root.

**Routes** (`/review*`): `GET /review` (directory listing across all roots), `GET /review?dir=` (the
screen), `GET /review/manifest`, `GET /review/preview`, `GET /review/destinations`,
`GET /review/destfiles`, `POST /review/newdest`, `POST /review/apply`. `/staging` stays as a launcher.

**Mark model (inverted from §4).** Marks are **opt-in**: a marked file is the file the action applies
to. `Space` toggles the current frame and advances; `A` marks all, `N` unmarks all, `I` inverts, `U`
unmarks the current frame. `localStorage` key is `marks4:<dir>` and stores **marked** paths only.

**Actions.** Four action buttons in the bar (`Move` / `Copy` / `Symlink` / `Delete`) plus one
destination select. Pressing a button states the operation (the pressed button stays highlighted,
`Delete` is red) and opens the confirmation; `Move` is the default and `Delete` disables the
destination select.
- `move` — `rename` with a cross-filesystem copy+delete fallback.
- `copy` — `fs::copy` + `fsync` of the destination.
- `symlink` — **absolute** source path as the link target (a relative target would be wrong the moment
  the link is moved, and Syncthing does move links).
- `delete` — permanent `remove_file`; needs no destination.
- Every op **refuses to overwrite** an existing name at the destination and reports it as a failure;
  `apply` never creates a destination directory.

**Confirmation states the operation and the quantity.** The summary shows
`Move 12 of 740 file(s) → dest`, lists the marked files (first 200), flags name collisions, and warns
on deletes. The Apply button is two-step armed and repeats the operation, the count, and the
destination on the confirm press.

**Autorepeat is configurable.** `nav_repeat_ms` (config) / `--nav-repeat-ms` (CLI), default **500 ms**,
is injected into the page as `data-repeat` and used as the throttle for the arrow/`h`/`l` keys. Only
nav keys honour `e.repeat`; `Space`, `A`, `N`, `I`, `U` keep the guard so marks can never machine-gun.

**Listing.** `/review` lists every directory under any root that holds FITS files **directly**
(nested dirs are not double-counted), labelled root-relative (`CCD/102CF/M8`), with frames and bytes
sortable by raw numbers. The cap is **per root** — a 400-directory CCD tree must not crowd the staging
dirs out of the list (measured: 400 CCD + 14 staging rows on real data). Dot-dirs (`.stfolder`, `.git`)
are never offered. **FITS-only** for now: non-FITS files are not listed.

**Index integration.** Every object row on `/` gains a `Review` button; the review URL uses `DIR_QUERY`
so `/` stays readable in the query string.

**Verified against real data** (`:5057` with the production config): roots reported as
`/data/Astro/CCD, /ssd/sync (autorepeat 500 ms)`; 414 directories listed; `Esprit/LBN458` manifest
(101 frames) and preview (20 KB JPEG) served; cross-root move, symlink, delete, clobber refusal,
missing-destination, and path-escape (`400`) all exercised on a temp fixture; both embedded JS
scripts pass `node --check`.
