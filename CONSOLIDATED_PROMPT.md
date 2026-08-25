# Consolidated Prompt: Astronomy Image Report Generator

The directory `/data/Astro/CCD` contains raw and processed astronomy images. Structure: `Telescope -> Object -> (recursive fits/cr3 files) + (flat master/ and *.pxiproject)`.

Create a Python program in the current directory that:

## Traversal

- Scan `ROOT = /data/Astro/CCD` in a single pass (no looping/re-scanning). Save results in memory.
- Top-level dirs are telescopes (e.g. `10RC`). Inside each telescope, dirs are objects.
- **Skip** object directories named (case-insensitive): `darks`, `cal`, `calibration`, `flats`.
- For each object directory:
  - **Recursively** scan (via `os.walk`) for `.fits`/`.fit`/`.cr2`/`.cr3` files at any depth.
  - `has_project`: check only **direct children** of the object dir for `*.pxiproject` subdirs, plus the `master/` subdir.
  - `has_final`: check only the `master/` subdir for `.jpg`/`.jpeg` files.

## Per-object data to collect

1. Object name
2. Telescope name
3. Timestamp of the latest image (from filename timestamps, fallback to file mtime)
4. Total exposure in seconds
5. For FITS files: number of files per filter, duration of each exposure, total time per filter.
   - Filter letters in filenames: `_L_` (Luminance), `_R_` (Red), `_B_` (Blue), `_G_` (Green), `_S_` (SII), `_O_` (OIII), `_H_` (Ha). No filter letter = DSLR.
   - Exposure duration patterns: `_300s_`, `_1200_Seconds_`, `_1200s_1x1_`, `_600_Seconds_2020-...`, `EXPOSURE-30.00s`.
   - For CR2/CR3 files: assume 30s duration each, label as "DSLR (CR2/CR3)".
6. `has_project` (bool)
7. `has_final` (bool)

## Output: single `report.html` in the script's directory

- HTML table with columns (in this order): **Latest image → Telescope → Object → Total exposure → Filters (count / duration / total) → Project → Final image**
- Sortable by each column (click header to sort, click again to reverse). Default sort: Latest image, descending.
- Row colors:
  - **Red**: no project AND no final image
  - **Yellow**: project present but no final image
- Show root path, object count, and generation timestamp in a meta line above the table.

## Constraints

- Single directory traversal, no re-scanning.
- Per-file `os.stat` wrapped in `try/except OSError` so one bad file doesn't abort the walk.
- Use `html.escape` for all user-facing strings in HTML output.
- Open output file with `encoding="utf-8"`.
