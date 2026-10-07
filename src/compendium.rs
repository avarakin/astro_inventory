//! Compendium catalog: shared record type, lazy loader, and web page.
//!
//! Data is produced by `src/bin/extract_compendium.rs` (run via `make compendium`)
//! and lives in `<dir>/objects.json` + `<dir>/thumbs/`. Nothing is loaded until
//! the first request to a `/compendium*` route.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

/// One deep-sky object from the Compendium Main sheet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompendiumObject {
    /// Sequential id in sheet order, 1-based (matches thumbnail filename stem).
    pub id: u32,
    pub name: String,
    #[serde(rename = "type")]
    pub obj_type: String,
    pub subtype: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_arcmin: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diameter: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ra_hms: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ra_deg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dec_dms: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dec_deg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constellation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearby: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_mag: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surf_brightness: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inclination: Option<f64>,
    /// Catalogue memberships, e.g. {"NGC": "1220", "Messier": "31"}.
    #[serde(default)]
    pub catalogs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simbad_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aladin_fov: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filters: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub astrobin_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simbad_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aladin_url: Option<String>,
    /// Thumbnail filename inside `<dir>/thumbs/`, e.g. "0001.Abell_01.jpg".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
}

/// Observer location, used as the page default (browser recomputes per user).
#[derive(Debug, Clone, Copy)]
pub struct Observer {
    pub latitude: f64,
    pub longitude: f64,
}

/// Process-wide lazy cache: parsed at most once, on first request.
static CACHE: OnceLock<Arc<Vec<CompendiumObject>>> = OnceLock::new();

/// Return the catalog, parsing `objects.json` on first call.
pub fn get_objects(dir: &Path) -> Result<Arc<Vec<CompendiumObject>>, String> {
    if let Some(v) = CACHE.get() {
        return Ok(v.clone());
    }
    let path = dir.join("objects.json");
    let data = std::fs::read(&path).map_err(|_| {
        format!(
            "Compendium data not found at {}. Run `make compendium` first.",
            path.display()
        )
    })?;
    let objs: Vec<CompendiumObject> =
        serde_json::from_slice(&data).map_err(|e| format!("Failed to parse {}: {e}", path.display()))?;
    let arc = Arc::new(objs);
    let _ = CACHE.set(arc.clone());
    Ok(arc)
}

/// Rise/set threshold altitude, standard refraction + solar semidiameter convention.
const RISE_SET_H0_DEG: f64 = -0.833;
/// Sidereal-to-solar time conversion factor.
const SIDERAL_TO_SOLAR: f64 = 1.00273790934;

fn mod24(h: f64) -> f64 {
    let h = h % 24.0;
    if h < 0.0 {
        h + 24.0
    } else {
        h
    }
}

/// Sky times for one object on a given local date, at an observer location.
/// Times are local civil decimal hours of that date (mod 24).
pub fn sky_times(
    ra_deg: f64,
    dec_deg: f64,
    lat_deg: f64,
    lon_deg: f64,
    date: chrono::NaiveDate,
) -> SkyTimes {
    use chrono::{Local, TimeZone};

    let midnight_naive = date.and_hms_opt(0, 0, 0).unwrap();
    let midnight = match Local.from_local_datetime(&midnight_naive).single() {
        Some(m) => m,
        None => return SkyTimes {
            transit_local: f64::NAN,
            transit_alt: f64::NAN,
            rise_local: None,
            set_local: None,
        },
    };
    // Julian date (UT) of local midnight.
    let jd = midnight.timestamp() as f64 / 86400.0 + 2440587.5;
    let d = jd - 2451545.0;
    let gmst = mod24(18.697374558 + 24.06570982441908 * d);
    let lst0 = mod24(gmst + lon_deg / 15.0);

    let ra_h = ra_deg / 15.0;
    // Sidereal interval from local midnight to transit -> solar hours.
    let transit_local = mod24((ra_h - lst0) / SIDERAL_TO_SOLAR);

    let (lat, dec) = (lat_deg.to_radians(), dec_deg.to_radians());
    let transit_alt = 90.0 - (lat_deg - dec_deg).abs();

    // Hour angle at rise/set.
    let cos_h = (RISE_SET_H0_DEG.to_radians().sin() - lat.sin() * dec.sin())
        / (lat.cos() * dec.cos());
    let (rise_local, set_local) = if cos_h >= 1.0 {
        (None, None) // never rises that day
    } else if cos_h <= -1.0 {
        (None, None) // circumpolar
    } else {
        let h_hours = cos_h.acos().to_degrees() / 15.0 / SIDERAL_TO_SOLAR;
        (Some(mod24(transit_local - h_hours)), Some(mod24(transit_local + h_hours)))
    };

    SkyTimes { transit_local, transit_alt, rise_local, set_local }
}

pub struct SkyTimes {
    pub transit_local: f64,
    pub transit_alt: f64,
    pub rise_local: Option<f64>,
    pub set_local: Option<f64>,
}

/// Replace every non-alphanumeric ASCII char with `_`.
pub fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// The Compendium browser page. Self-contained; fetches `/compendium/data`
/// only when opened. `data_available` toggles a setup hint banner.
/// Sky times are computed in the browser; `observer`/`when` seed the inputs.
pub fn render_compendium_page(data_available: bool, observer: Observer, when: &str) -> String {
    let hint = if data_available {
        String::new()
    } else {
        "<div class=\"hint\">No data yet — run <code>make compendium</code> in the project directory, then reload.</div>".to_string()
    };
    COMPENDIUM_HTML
        .replace("{{HINT}}", &hint)
        .replace("{{LAT}}", &format!("{:.4}", observer.latitude))
        .replace("{{LON}}", &format!("{:.4}", observer.longitude))
        .replace("{{WHEN}}", when)
        .to_string()
}

const COMPENDIUM_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Deep Sky Compendium</title>
<style>
body { font-family: system-ui, sans-serif; margin: 1em; background: #11151c; color: #dde3ea; }
h1 { font-size: 1.4em; margin-bottom: .2em; }
a { color: #7db8ff; }
.meta { color: #9aa7b5; font-size: .9em; margin-top: 0; }
.hint { background: #3a2f14; border: 1px solid #8a6d1a; padding: .6em 1em; border-radius: 6px; margin: .8em 0; }
#filters { display: flex; flex-wrap: wrap; gap: .6em; align-items: center; margin: .8em 0; position: sticky; top: 0; background: #11151c; padding: .4em 0; z-index: 2; }
#filters input, #filters select { background: #1c2430; color: #dde3ea; border: 1px solid #33404f; border-radius: 4px; padding: .35em .5em; }
#filters input[type=search] { min-width: 16em; }
#count { color: #9aa7b5; font-size: .9em; margin-left: auto; }
table { border-collapse: collapse; width: 100%; font-size: .85em; }
th, td { padding: .3em .5em; border-bottom: 1px solid #232d3a; text-align: left; vertical-align: top; }
th { cursor: pointer; user-select: none; position: relative; white-space: nowrap; background: #161d27; }
th .arrow { color: #7db8ff; margin-left: .2em; }
tr:hover { background: #18212d; cursor: pointer; }
img.thumb { width: 64px; height: 48px; object-fit: cover; border-radius: 4px; background: #000; display: block; }
.num { text-align: right; font-variant-numeric: tabular-nums; }
.cat { color: #9aa7b5; font-size: .9em; }
#detail { position: fixed; top: 0; right: 0; width: 34em; max-width: 92vw; height: 100vh; background: #161d27; border-left: 1px solid #33404f; padding: 1em 1.4em; overflow-y: auto; display: none; box-shadow: -4px 0 18px rgba(0,0,0,.5); }
#detail.open { display: block; }
#detail h2 { margin: .2em 0 .1em; }
#detail .sub { color: #9aa7b5; margin-bottom: .8em; }
#detail img { max-width: 100%; max-height: 40vh; width: auto; height: auto; object-fit: contain; display: block; margin: .6em auto; background: #000; border-radius: 6px; }
#detail img.full { max-height: none; }
#hdr { position: sticky; top: 0; z-index: 3; background: #161d27; text-align: right; padding: .2em 0; }
#hdr span { cursor: pointer; color: #9aa7b5; margin-left: 1em; }
#fullsize { font-size: .9em; border: 1px solid #33404f; border-radius: 4px; padding: .2em .6em; }
#detail dl { display: grid; grid-template-columns: max-content 1fr; gap: .25em .8em; font-size: .9em; }
#detail dt { color: #9aa7b5; }
#detail dd { margin: 0; }
#detail .notes { margin-top: .8em; white-space: pre-wrap; font-size: .92em; line-height: 1.35; }
#detail .links a { margin-right: .8em; }
#close { font-size: 1.2em; }
</style>
</head>
<body>
<h1>Deep Sky Compendium</h1>
<p class="meta"><a href="/">&larr; Inventory</a> &middot; Imm Deep Sky Compendium &mdash; 2026 &mdash; 6th Edition</p>
{{HINT}}
<div id="filters">
  <input type="search" id="q" placeholder="Search name / nickname / notes / catalogue id...">
  <select id="type"><option value="">Type: all</option></select>
  <select id="subtype"><option value="">Subtype: all</option></select>
  <select id="const"><option value="">Constellation: all</option></select>
  <select id="cat"><option value="">Catalogue: any</option></select>
  <select id="rating">
    <option value="">Rating: any</option>
    <option value="5">5 only</option>
    <option value="4">4+</option>
    <option value="3">3+</option>
    <option value="2">2+</option>
  </select>
  <label class="ctl">When <input id="when" type="datetime-local" value="{{WHEN}}"></label>
  <label class="ctl">Lat <input id="lat" type="number" step="0.01" value="{{LAT}}" style="width:5.5em"></label>
  <label class="ctl">Lon <input id="lon" type="number" step="0.01" value="{{LON}}" style="width:6em"></label>
  <button id="geo">Use my location</button>
  <button id="clear">Clear</button>
  <span id="count"></span>
</div>
<table>
<thead><tr>
  <th data-k="thumb"></th>
  <th data-k="id">#</th>
  <th data-k="name">Name</th>
  <th data-k="type">Type</th>
  <th data-k="subtype">Subtype</th>
  <th data-k="size_arcmin" class="num">Size&prime;</th>
  <th data-k="distance" class="num">Dist</th>
  <th data-k="rating" class="num">Rating</th>
  <th data-k="visual_mag" class="num">Mag</th>
  <th data-k="ra_deg" class="num">RA</th>
  <th data-k="dec_deg" class="num">Dec</th>
  <th data-k="constellation">Const</th>
  <th data-k="transit_time" class="num">Transit</th>
  <th data-k="transit_alt" class="num">Alt&deg;</th>
  <th data-k="rise_time" class="num">Rise</th>
  <th data-k="set_time" class="num">Set</th>
  <th data-k="cats">Catalogues</th>
</tr></thead>
<tbody id="rows"></tbody>
</table>
<div id="detail">
  <div id="hdr"><span id="fullsize">Full size</span><span id="close">&times;</span></div>
  <div id="detail-body"></div>
</div>
<script>
let DATA = null, sortKey = 'id', sortDir = 1, currentId = null;
const $ = id => document.getElementById(id);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const fmt = v => v == null ? '' : (Math.abs(v) >= 100 ? Math.round(v) : Math.round(v * 100) / 100);
const fmtTime = v => { if (v == null || isNaN(v)) return '\u2014'; const m = Math.round(v * 60); return String(Math.floor(m / 60) % 24).padStart(2, '0') + ':' + String(m % 60).padStart(2, '0'); };

// --- sky times, computed in the browser (Rust reference impl: sky_times) ---
const SIDERAL = 1.00273790934, H0 = -0.833;
const mod24 = h => ((h % 24) + 24) % 24;
function skyTimes(ra, dec, lat, lon, jd) {
  const d = jd - 2451545.0;
  const lst0 = mod24(mod24(18.697374558 + 24.06570982441908 * d) + lon / 15);
  const transit = mod24((ra / 15 - lst0) / SIDERAL);
  const alt = 90 - Math.abs(lat - dec);
  const l = lat * Math.PI / 180, c = dec * Math.PI / 180;
  const ch = (Math.sin(H0 * Math.PI / 180) - Math.sin(l) * Math.sin(c)) / (Math.cos(l) * Math.cos(c));
  let rise = null, set = null;
  if (ch < 1 && ch > -1) {
    const h = Math.acos(ch) * 180 / Math.PI / 15 / SIDERAL;
    rise = mod24(transit - h); set = mod24(transit + h);
  }
  return { transit, alt, rise, set };
}
function computeAll() {
  const w = $('when').value;
  const d = w ? new Date(w) : new Date();
  const mid = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const jd = mid.getTime() / 86400000 + 2440587.5;
  const lat = parseFloat($('lat').value) || 0, lon = parseFloat($('lon').value) || 0;
  for (const o of DATA) {
    if (o.ra_deg == null || o.dec_deg == null) { o._tt = o._ta = o._rt = o._st = null; continue; }
    const t = skyTimes(o.ra_deg, o.dec_deg, lat, lon, jd);
    o._tt = t.transit; o._ta = t.alt; o._rt = t.rise; o._st = t.set;
  }
  try {
    localStorage.setItem('comp_lat', $('lat').value);
    localStorage.setItem('comp_lon', $('lon').value);
    localStorage.setItem('comp_when', w);
  } catch (e) {}
}

fetch('/compendium/data').then(r => {
  if (!r.ok) return r.text().then(t => { $('count').textContent = t; throw new Error(t); });
  return r.json();
}).then(d => {
  DATA = d;
  try {
    const la = localStorage.getItem('comp_lat'), lo = localStorage.getItem('comp_lon'), w = localStorage.getItem('comp_when');
    if (la != null && isFinite(parseFloat(la))) $('lat').value = la;
    if (lo != null && isFinite(parseFloat(lo))) $('lon').value = lo;
    if (w) $('when').value = w;
  } catch (e) {}
  computeAll();
  fillSelect('type', [...new Set(d.map(o => o.type))].sort());
  fillSelect('subtype', [...new Set(d.map(o => o.subtype).filter(Boolean))].sort());
  fillSelect('const', [...new Set(d.map(o => o.constellation).filter(Boolean))].sort());
  fillSelect('cat', [...new Set(d.flatMap(o => Object.keys(o.catalogs)))].sort());
  apply();
});

function fillSelect(id, opts) {
  const sel = $(id);
  for (const o of opts) { const e = document.createElement('option'); e.value = o; e.textContent = o; sel.appendChild(e); }
}

function filtered() {
  const q = $('q').value.trim().toLowerCase();
  const ty = $('type').value, st = $('subtype').value, co = $('const').value, ca = $('cat').value;
  const minR = $('rating').value ? Number($('rating').value) : null;
  return DATA.filter(o => {
    if (ty && o.type !== ty) return false;
    if (st && o.subtype !== st) return false;
    if (co && o.constellation !== co) return false;
    if (ca && !(ca in o.catalogs)) return false;
    if (minR != null && !(o.rating >= minR)) return false;
    if (q) {
      const hay = [o.name, o.nickname, o.notes, o.alt_id, Object.entries(o.catalogs).map(([k,v]) => k + ' ' + v).join(' ')].join(' ').toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });
}

const NUMKEYS = new Set(['id','size_arcmin','distance','diameter','rating','visual_mag','ra_deg','dec_deg','transit_time','transit_alt','rise_time','set_time']);
const CKEY = { transit_time: '_tt', transit_alt: '_ta', rise_time: '_rt', set_time: '_st' };
function sortVal(o, k) {
  if (CKEY[k]) { const v = o[CKEY[k]]; return v == null ? Infinity : v; }
  if (k === 'cats') return Object.entries(o.catalogs).map(([kk,v]) => kk + v).join(' ');
  const v = o[k];
  if (v == null) return NUMKEYS.has(k) ? Infinity : '';
  return v;
}

function apply() {
  if (!DATA) return;
  let rows = filtered();
  rows.sort((a, b) => {
    const va = sortVal(a, sortKey), vb = sortVal(b, sortKey);
    if (typeof va === 'number' && typeof vb === 'number') return (va - vb) * sortDir;
    return String(va).localeCompare(String(vb)) * sortDir;
  });
  $('count').textContent = rows.length + ' / ' + DATA.length + ' objects';
  $('rows').innerHTML = rows.map(o => `<tr data-id="${o.id}">
    <td>${o.thumb ? `<img class="thumb" loading="lazy" src="/compendium/thumb/${o.id}" alt="">` : ''}</td>
    <td class="num">${o.id}</td>
    <td>${esc(o.name)}${o.nickname ? `<div class="cat">${esc(o.nickname)}</div>` : ''}</td>
    <td>${esc(o.type)}</td>
    <td>${esc(o.subtype)}</td>
    <td class="num">${fmt(o.size_arcmin)}</td>
    <td class="num">${fmt(o.distance)}</td>
    <td class="num">${fmt(o.rating)}</td>
    <td class="num">${fmt(o.visual_mag)}</td>
    <td class="num">${fmt(o.ra_deg)}</td>
    <td class="num">${fmt(o.dec_deg)}</td>
    <td>${esc(o.constellation)}</td>
    <td class="num">${fmtTime(o._tt)}</td>
    <td class="num">${o._ta == null ? '' : fmt(o._ta)}</td>
    <td class="num">${fmtTime(o._rt)}</td>
    <td class="num">${fmtTime(o._st)}</td>
    <td class="cat">${esc(Object.entries(o.catalogs).map(([k,v]) => k + ' ' + v).join(', '))}</td>
  </tr>`).join('');
  document.querySelectorAll('th[data-k]').forEach(th => {
    th.querySelector('.arrow')?.remove();
    if (th.dataset.k === sortKey) { const s = document.createElement('span'); s.className = 'arrow'; s.textContent = sortDir === 1 ? '\u25b2' : '\u25bc'; th.appendChild(s); }
  });
}

document.querySelectorAll('th[data-k]').forEach(th => th.addEventListener('click', () => {
  const k = th.dataset.k;
  if (!k || k === 'thumb') return;
  if (sortKey === k) sortDir = -sortDir; else { sortKey = k; sortDir = 1; }
  apply();
}));
for (const id of ['q','type','subtype','const','cat','rating']) $(id).addEventListener('input', apply);
for (const id of ['when','lat','lon']) $(id).addEventListener('change', () => { computeAll(); apply(); });
$('geo').addEventListener('click', () => {
  if (!navigator.geolocation) { alert('Geolocation unavailable (needs https or localhost)'); return; }
  navigator.geolocation.getCurrentPosition(
    p => { $('lat').value = p.coords.latitude.toFixed(4); $('lon').value = p.coords.longitude.toFixed(4); computeAll(); apply(); },
    e => alert('Location unavailable: ' + e.message));
});
$('clear').addEventListener('click', () => { for (const id of ['q','type','subtype','const','cat','rating']) $(id).value = ''; apply(); });

$('rows').addEventListener('click', ev => {
  const tr = ev.target.closest('tr'); if (!tr) return;
  const o = DATA.find(x => x.id === Number(tr.dataset.id)); if (!o) return;
  const rows = [];
  const add = (k, v) => { if (v != null && v !== '') rows.push(`<dt>${k}</dt><dd>${esc(v)}</dd>`); };
  add('Class', o.class); add('Diameter', o.diameter); add('Surf. brightness', o.surf_brightness);
  add('Inclination', o.inclination); add('RA', o.ra_hms); add('Dec', o.dec_dms);
  add('Transit', fmtTime(o._tt)); add('Alt at transit', o._ta == null ? null : fmt(o._ta) + '\u00b0');
  add('Rise', fmtTime(o._rt)); add('Set', fmtTime(o._st));
  add('Alt. ID', o.alt_id); add('Nearby', o.nearby); add('Filters', o.filters);
  for (const [k, v] of Object.entries(o.catalogs)) add(k, v);
  const links = [];
  if (o.astrobin_url) links.push(`<a href="${esc(o.astrobin_url)}" target="_blank">Astrobin</a>`);
  if (o.simbad_url) links.push(`<a href="${esc(o.simbad_url)}" target="_blank">SIMBAD</a>`);
  if (o.aladin_url) links.push(`<a href="${esc(o.aladin_url)}" target="_blank">Aladin</a>`);
  if (o.thumb) links.push(`<a href="/compendium/thumb/${o.id}" target="_blank">Full image</a>`);
  $('detail-body').innerHTML = `
    <h2>${esc(o.name)}</h2><div class="sub">${esc(o.type)} / ${esc(o.subtype)} &middot; ${esc(o.constellation)} &middot; #${o.id}</div>
    ${o.thumb ? `<img id="detail-img" src="/compendium/thumb/${o.id}" alt="">` : ''}
    <dl>${rows.join('')}</dl>
    ${o.notes ? `<div class="notes">${esc(o.notes)}</div>` : ''}
    <p class="links">${links.join(' ')}</p>`;
  currentId = o.id;
  $('detail').classList.add('open');
  $('detail').scrollTop = 0;
  const img = $('detail-img');
  if (img) img.classList.remove('full');
  $('fullsize').textContent = 'Full size';
});
$('fullsize').addEventListener('click', () => {
  const img = $('detail-img');
  if (!img || currentId == null) return;
  $('fullsize').textContent = img.classList.toggle('full') ? 'Fit' : 'Full size';
});
$('close').addEventListener('click', () => $('detail').classList.remove('open'));
document.addEventListener('keydown', e => { if (e.key === 'Escape') $('detail').classList.remove('open'); });
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_non_alphanumeric() {
        assert_eq!(sanitize_name("Abell 01"), "Abell_01");
        assert_eq!(sanitize_name("M13 (Rosette)!"), "M13__Rosette__");
        assert_eq!(sanitize_name("NGC 1234"), "NGC_1234");
        assert_eq!(sanitize_name("ZW II 96"), "ZW_II_96");
    }

    #[test]
    fn record_round_trips_through_json() {
        let mut catalogs = BTreeMap::new();
        catalogs.insert("NGC".to_string(), "7000".to_string());
        let obj = CompendiumObject {
            id: 1,
            name: "Abell 01".into(),
            obj_type: "Neb".into(),
            subtype: "PN".into(),
            class: Some("SHN/h".into()),
            size_arcmin: Some(0.8),
            distance: Some(8200.0),
            diameter: Some(2.0),
            rating: Some(2.0),
            notes: Some("HII much stronger than OIII.".into()),
            ra_hms: Some("1236".into()),
            ra_deg: Some(3.15),
            dec_dms: Some("691041".into()),
            dec_deg: Some(69.178),
            constellation: Some("Cep".into()),
            nickname: None,
            alt_id: None,
            nearby: None,
            visual_mag: Some(19.0),
            surf_brightness: None,
            inclination: None,
            catalogs,
            simbad_key: Some("PN A66 01".into()),
            aladin_fov: Some("0.2".into()),
            filters: Some("x".into()),
            astrobin_url: None,
            simbad_url: None,
            aladin_url: None,
            thumb: Some("0001.Abell_01.jpg".into()),
        };
        let json = serde_json::to_string(&obj).unwrap();
        let back: CompendiumObject = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 1);
        assert_eq!(back.name, "Abell 01");
        assert_eq!(back.catalogs["NGC"], "7000");
        assert_eq!(back.ra_deg, Some(3.15));
        assert!(json.contains("\"type\":\"Neb\""));
    }

    #[test]
    fn sky_times_vega_sanity() {
        // Vega: RA 279.23 deg, Dec +38.62 deg. From lat 31 N, lon -95 W in
        // early October it transits in the early evening, near the zenith.
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        let t = sky_times(279.23, 38.62, 31.0, -95.0, date);
        assert!((19.0..23.5).contains(&t.transit_local), "transit {:?}", t.transit_local);
        assert!((82.0..83.0).contains(&t.transit_alt), "alt {:?}", t.transit_alt);
        let rise = t.rise_local.unwrap();
        let set = t.set_local.unwrap();
        // Half-day arc for dec +38.6 from lat 31 is ~8h: rise ~8h before
        // transit, set ~8h after (mod 24).
        assert!((mod24(t.transit_local - rise) - 8.0).abs() < 0.5, "rise {rise}");
        assert!((mod24(set - t.transit_local) - 8.0).abs() < 0.5, "set {set}");
    }

    #[test]
    fn sky_times_circumpolar_and_never_rise() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        // dec +89 from lat 31 N: circumpolar -> no rise/set
        let t = sky_times(0.0, 89.0, 31.0, -95.0, date);
        assert!(t.rise_local.is_none() && t.set_local.is_none());
        // dec -89: never rises
        let t = sky_times(180.0, -89.0, 31.0, -95.0, date);
        assert!(t.rise_local.is_none() && t.set_local.is_none());
    }

    #[test]
    fn transit_altitude_meridian_formula() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        // dec == lat -> transits at zenith
        let t = sky_times(45.0, 31.0, 31.0, -95.0, date);
        assert!((t.transit_alt - 90.0).abs() < 1e-9);
    }
}
