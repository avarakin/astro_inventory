//! Pure-Rust FITS reading, auto-stretch, and preview rendering for the
//! staging review workflow.
//!
//! Deliberately self-contained: this module has **no `crate::` dependencies**
//! so it can be exposed from `src/lib.rs` (like `compendium`) and unit-tested
//! in the lib target.
//!
//! Two deliberately separate passes:
//! - [`stats_pass`] — cheap, strided, reads ~20% of the file. Gives the
//!   `sky/σ` quality metric used for sorting.
//! - [`render_preview`] — expensive, full decode, run lazily per viewed image.

use std::cmp::Ordering;
use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ExtendedColorType, ImageEncoder};
use rayon::prelude::*;

const BLOCK: usize = 2880;
const CARD: usize = 80;
const MAX_HEADER_BLOCKS: usize = 64;

/// Strides for the cheap stats pass: every 5th row at full x resolution, every
/// 2nd pixel. Measured on the real 6224x4168 files: 2.6M samples read from
/// 10.4 MB of 51.9 MB (~20% of the file).
const STATS_ROW_STRIDE: usize = 5;
const STATS_PIX_STRIDE: usize = 2;

/// Block size for the brightest-star scan that anchors the 1:1 inset.
const STAR_BLOCK: usize = 16;

// --- Errors -----------------------------------------------------------------

#[derive(Debug)]
pub enum FitError {
    Io(io::Error),
    /// File is shorter than its header claims.
    Truncated,
    /// No readable FITS header.
    NotFits,
    /// Header parsed but this kind of data cannot be rendered.
    Unsupported(String),
    Image(image::ImageError),
}

impl fmt::Display for FitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FitError::Io(e) => write!(f, "I/O error: {e}"),
            FitError::Truncated => write!(f, "file is shorter than its FITS header claims"),
            FitError::NotFits => write!(f, "not a FITS file (no readable header)"),
            FitError::Unsupported(s) => write!(f, "unsupported FITS: {s}"),
            FitError::Image(e) => write!(f, "image error: {e}"),
        }
    }
}

impl std::error::Error for FitError {}

impl From<io::Error> for FitError {
    fn from(e: io::Error) -> Self {
        FitError::Io(e)
    }
}

impl From<image::ImageError> for FitError {
    fn from(e: image::ImageError) -> Self {
        FitError::Image(e)
    }
}

// --- Header -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitPix {
    U8,
    I16,
    I32,
    F32,
    F64,
}

impl BitPix {
    fn from_raw(v: i64) -> Option<Self> {
        match v {
            8 => Some(BitPix::U8),
            16 => Some(BitPix::I16),
            32 => Some(BitPix::I32),
            -32 => Some(BitPix::F32),
            -64 => Some(BitPix::F64),
            _ => None,
        }
    }

    fn bytes(self) -> usize {
        match self {
            BitPix::U8 => 1,
            BitPix::I16 => 2,
            BitPix::I32 => 4,
            BitPix::F32 => 4,
            BitPix::F64 => 8,
        }
    }

    /// Largest value the stored integers can represent, after BSCALE/BZERO.
    /// `None` for float data, which has no meaningful saturation ceiling.
    fn ceiling(self, bscale: f32, bzero: f32) -> Option<f32> {
        let top = match self {
            BitPix::U8 => 255.0f64,
            BitPix::I16 => i16::MAX as f64,
            BitPix::I32 => i32::MAX as f64,
            BitPix::F32 | BitPix::F64 => return None,
        };
        Some((top * bscale as f64 + bzero as f64) as f32)
    }
}

#[derive(Debug, Clone)]
pub struct FitsHeader {
    pub width: usize,
    pub height: usize,
    /// 1 for mono, 3 for an RGB cube.
    pub planes: usize,
    pub bitpix: BitPix,
    pub bscale: f32,
    pub bzero: f32,
    pub data_offset: u64,
    pub filter: Option<String>,
    pub imagetype: Option<String>,
    pub exptime: Option<f64>,
}

impl FitsHeader {
    /// Exact byte length of the pixel region. The file may be longer, because
    /// FITS pads data to a 2880-byte boundary — the real files in this corpus
    /// carry 2,816 B of trailing filler. Never read "to EOF".
    pub fn data_len(&self) -> usize {
        self.width * self.height * self.planes * self.bitpix.bytes()
    }

    pub fn sat_ceiling(&self) -> Option<f32> {
        self.bitpix.ceiling(self.bscale, self.bzero)
    }
}

fn card_value(card: &str) -> Option<&str> {
    if card.len() > 8 && card.as_bytes()[8] == b'=' {
        Some(card[9..].split('/').next().unwrap_or(""))
    } else {
        None
    }
}

/// Unquote and trim a FITS string value (`'H       '` -> `H`).
fn clean_str(v: &str) -> Option<String> {
    let v = v.trim().trim_matches('\'').trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

/// Blank or garbage cards (`BZERO   =`) are common in the wild; treat them as
/// absent rather than failing the whole file.
fn clean_num(v: &str) -> Option<f64> {
    v.trim().parse::<f64>().ok()
}

/// Parse header blocks until the first IMAGE HDU is found, skipping
/// BINTABLE extensions and NAXIS=0 primaries.
pub fn read_header(path: &Path) -> Result<FitsHeader, FitError> {
    let mut f = File::open(path).map_err(FitError::Io)?;
    let mut blocks = 0usize;

    loop {
        let mut keys: Vec<(String, String)> = Vec::new();
        let mut ended = false;

        while !ended {
            if blocks >= MAX_HEADER_BLOCKS {
                return Err(FitError::Unsupported("header chain too long".into()));
            }
            let mut blk = [0u8; BLOCK];
            match f.read_exact(&mut blk) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    // Every read here sits on a header boundary, so EOF means
                    // "the file ended without an IMAGE HDU".
                    return Err(if blocks == 0 {
                        FitError::NotFits
                    } else {
                        FitError::Unsupported("no IMAGE HDU in file".into())
                    });
                }
                Err(e) => return Err(FitError::Io(e)),
            }
            blocks += 1;
            let text = String::from_utf8_lossy(&blk);

            for i in 0..BLOCK / CARD {
                let card = &text[i * CARD..(i + 1) * CARD];
                let key = card[..8].trim();
                if key == "END" {
                    ended = true;
                    break;
                }
                if key == "COMMENT" || key == "HISTORY" || key.is_empty() {
                    continue;
                }
                if let Some(v) = card_value(card) {
                    keys.push((key.to_string(), v.trim().to_string()));
                }
            }
        }

        let get = |k: &str| -> Option<&str> {
            keys.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
        };
        let axis = |n: usize| -> u64 {
            let k = format!("NAXIS{n}");
            get(&k).and_then(clean_num).unwrap_or(0.0) as u64
        };

        let is_image = match get("XTENSION").and_then(clean_str).as_deref() {
            Some(x) => x == "IMAGE",
            None => get("SIMPLE").is_some_and(|v| v.trim() == "T"),
        };
        let naxis = get("NAXIS").and_then(clean_num).map(|v| v as i64).unwrap_or(0);
        let bp_raw = get("BITPIX").and_then(clean_num).map(|v| v as i64);

        if is_image && naxis >= 2 {
            let width = axis(1) as usize;
            let height = axis(2) as usize;
            let n3 = axis(3) as usize;
            let bitpix = match bp_raw.and_then(BitPix::from_raw) {
                Some(b) => b,
                None => {
                    return Err(FitError::Unsupported(format!("BITPIX {bp_raw:?}")));
                }
            };
            if width == 0 || height == 0 {
                return Err(FitError::Unsupported("zero-length axes".into()));
            }
            let planes = if naxis == 3 {
                if n3 == 3 {
                    3
                } else {
                    return Err(FitError::Unsupported(format!("NAXIS3 = {n3}")));
                }
            } else {
                1
            };

            return Ok(FitsHeader {
                width,
                height,
                planes,
                bitpix,
                bscale: get("BSCALE").and_then(clean_num).unwrap_or(1.0) as f32,
                // BZERO = 32768 is present on every real file here. Skipping it
                // turns a ~310 ADU sky into ~-32458 and the stretch collapses.
                bzero: get("BZERO").and_then(clean_num).unwrap_or(0.0) as f32,
                data_offset: (blocks * BLOCK) as u64,
                filter: get("FILTER").and_then(clean_str),
                imagetype: get("IMAGETYP").and_then(clean_str),
                exptime: get("EXPTIME").and_then(clean_num),
            });
        }

        // Not an image HDU: skip its data and try the next header block.
        if naxis < 1 {
            continue; // NAXIS = 0 carries no data at all
        }
        let mut len = 1u64;
        for a in 1..=naxis as usize {
            len = len.saturating_mul(axis(a));
        }
        let bpp = bp_raw
            .and_then(BitPix::from_raw)
            .map(|b| b.bytes())
            .unwrap_or(0);
        let data = (len * bpp as u64).div_ceil(BLOCK as u64) * BLOCK as u64;
        if data == 0 {
            continue;
        }
        f.seek(SeekFrom::Current(data as i64)).map_err(FitError::Io)?;
    }
}

// --- Pixel access -----------------------------------------------------------

#[inline]
fn stored(raw: &[u8], i: usize, bp: BitPix) -> f32 {
    match bp {
        BitPix::U8 => raw[i] as f32,
        BitPix::I16 => {
            let j = i * 2;
            i16::from_be_bytes([raw[j], raw[j + 1]]) as f32
        }
        BitPix::I32 => {
            let j = i * 4;
            i32::from_be_bytes([raw[j], raw[j + 1], raw[j + 2], raw[j + 3]]) as f32
        }
        BitPix::F32 => {
            let j = i * 4;
            f32::from_be_bytes([raw[j], raw[j + 1], raw[j + 2], raw[j + 3]])
        }
        BitPix::F64 => {
            let j = i * 8;
            f64::from_be_bytes([
                raw[j],
                raw[j + 1],
                raw[j + 2],
                raw[j + 3],
                raw[j + 4],
                raw[j + 5],
                raw[j + 6],
                raw[j + 7],
            ]) as f32
        }
    }
}

/// Physical pixel value, BSCALE/BZERO applied. FITS stores BITPIX 16 as
/// *signed*, so `BZERO = 32768` maps -32768..32767 onto 0..65535.
#[inline]
pub fn value(raw: &[u8], i: usize, h: &FitsHeader) -> f32 {
    stored(raw, i, h.bitpix) * h.bscale + h.bzero
}

/// Read exactly `data_len()` bytes — never the rest of the file, which carries
/// 2880-byte boundary padding.
pub fn read_pixels(path: &Path, h: &FitsHeader) -> Result<Vec<u8>, FitError> {
    let mut f = File::open(path).map_err(FitError::Io)?;
    f.seek(SeekFrom::Start(h.data_offset))
        .map_err(FitError::Io)?;
    let mut buf = vec![0u8; h.data_len()];
    match f.read_exact(&mut buf) {
        Ok(()) => Ok(buf),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(FitError::Truncated),
        Err(e) => Err(FitError::Io(e)),
    }
}

// --- Statistics -------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub median: f32,
    /// MAD-based sigma, already multiplied by 1.4826.
    pub sigma: f32,
    pub p99_99: f32,
    pub max: f32,
    pub n: usize,
}

impl Stats {
    /// Cheap noise-based quality metric; higher is better (more sky signal per
    /// unit noise, i.e. closer to shot-noise limited).
    ///
    /// Blind spot: it cannot see tracking failures or star trailing.
    pub fn sky_over_sigma(&self) -> f32 {
        if self.sigma <= 0.0 {
            0.0
        } else {
            self.median / self.sigma
        }
    }
}

fn fcmp(a: &f32, b: &f32) -> Ordering {
    a.partial_cmp(b).unwrap_or(Ordering::Equal)
}

fn nth(v: &mut [f32], k: usize) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let k = k.min(v.len() - 1);
    let (_, el, _) = v.select_nth_unstable_by(k, fcmp);
    *el
}

/// Iteratively sigma-clipped median + MAD, plus the *unclipped* p99.99 and max.
fn clipped_stats(samples: &mut Vec<f32>) -> Stats {
    if samples.is_empty() {
        return Stats {
            median: 0.0,
            sigma: 0.0,
            p99_99: 0.0,
            max: 0.0,
            n: 0,
        };
    }

    let n = samples.len();
    let mut sorted = samples.clone();
    sorted.sort_by(fcmp);
    let p99_99 = sorted[((n as f64 * 0.9999) as usize).min(n - 1)];
    let max = sorted[n - 1];
    drop(sorted);

    let mut median = 0.0f32;
    let mut sigma = 1.0f32;
    for _ in 0..3 {
        let mid = samples.len() / 2;
        median = nth(samples, mid);
        let mut dev: Vec<f32> = samples.iter().map(|v| (v - median).abs()).collect();
        let half = dev.len() / 2;
        let mad = nth(&mut dev, half) * 1.4826;
        if mad > 0.0 {
            sigma = mad;
        }
        let lo = median - 3.0 * sigma;
        let hi = median + 3.0 * sigma;
        samples.retain(|v| *v >= lo && *v <= hi);
        if samples.len() < 16 {
            break;
        }
    }

    Stats {
        median,
        sigma,
        p99_99,
        max,
        n,
    }
}

/// Cheap pass: seek to every `STATS_ROW_STRIDE`-th row and read only that row,
/// so ~20% of the file is touched.
pub fn stats_pass(path: &Path, h: &FitsHeader) -> Result<Stats, FitError> {
    let row_bytes = h.width * h.bitpix.bytes();
    let plane_bytes = row_bytes * h.height;

    let mut f = File::open(path).map_err(FitError::Io)?;
    let mut row = vec![0u8; row_bytes];
    let mut samples: Vec<f32> =
        Vec::with_capacity(h.width / STATS_PIX_STRIDE * (h.height / STATS_ROW_STRIDE) * h.planes);

    for p in 0..h.planes {
        for y in (0..h.height).step_by(STATS_ROW_STRIDE) {
            let at = h.data_offset + (p * plane_bytes + y * row_bytes) as u64;
            f.seek(SeekFrom::Start(at)).map_err(FitError::Io)?;
            match f.read_exact(&mut row) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Err(FitError::Truncated);
                }
                Err(e) => return Err(FitError::Io(e)),
            }
            for x in (0..h.width).step_by(STATS_PIX_STRIDE) {
                samples.push(stored(&row, x, h.bitpix) * h.bscale + h.bzero);
            }
        }
    }

    Ok(clipped_stats(&mut samples))
}

/// Same statistics from an already-decoded buffer (striding in memory).
pub fn stats_from_raw(raw: &[u8], h: &FitsHeader) -> Stats {
    let npix = h.width * h.height;
    let mut samples: Vec<f32> = Vec::with_capacity(npix / (STATS_ROW_STRIDE * STATS_PIX_STRIDE));
    for p in 0..h.planes {
        for y in (0..h.height).step_by(STATS_ROW_STRIDE) {
            let base = p * npix + y * h.width;
            for x in (0..h.width).step_by(STATS_PIX_STRIDE) {
                samples.push(value(raw, base + x, h));
            }
        }
    }
    clipped_stats(&mut samples)
}

// --- Stretch ----------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct Stretch {
    black: f32,
    white: f32,
    t: f32,
}

impl Stretch {
    /// Siril MF'CCD-style screen stretch.
    pub fn new(s: &Stats) -> Self {
        let black = s.median - 2.0 * s.sigma;
        // The max() is load-bearing: on real Ha data med+8*sigma is ~406 ADU
        // while p99.99 is ~975 ADU, so the percentile term lifts the white
        // point ~2.4x and keeps faint stars out of the clip.
        let white = (s.median + 8.0 * s.sigma).max(s.p99_99);
        let white = if white > black + 1.0 { white } else { black + 1.0 };

        // Midtone transfer m' = m(1-t)/(1-t*m), with t chosen so the sky median
        // lands on TARGET. Negative t (brightening) is safe: the denominator
        // 1 - t*m only vanishes for t >= 1.
        const TARGET: f32 = 0.30;
        let m0 = ((s.median - black) / (white - black)).clamp(0.001, 0.999);
        let t = ((TARGET - m0) / (m0 * (TARGET - 1.0))).clamp(-1000.0, 0.95);

        Stretch { black, white, t }
    }

    #[inline]
    fn map(&self, v: f32) -> u8 {
        let m = ((v - self.black) / (self.white - self.black)).clamp(0.0, 1.0);
        let m = (m * (1.0 - self.t)) / (1.0 - self.t * m);
        (m.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    }
}

const CHUNK: usize = 1 << 14;

fn stretch_mono(raw: &[u8], h: &FitsHeader, s: &Stretch) -> Vec<u8> {
    let n = h.width * h.height;
    let mut out = vec![0u8; n];
    out.par_chunks_mut(CHUNK).enumerate().for_each(|(ci, chunk)| {
        let base = ci * CHUNK;
        for (j, o) in chunk.iter_mut().enumerate() {
            *o = s.map(value(raw, base + j, h));
        }
    });
    out
}

fn stretch_rgb(raw: &[u8], h: &FitsHeader, s: &Stretch) -> Vec<u8> {
    let npix = h.width * h.height;
    let mut out = vec![0u8; npix * 3];
    out.par_chunks_mut(CHUNK * 3).enumerate().for_each(|(ci, chunk)| {
        let base = ci * CHUNK;
        for (j, o) in chunk.iter_mut().enumerate() {
            let px = base + j / 3;
            let plane = j % 3;
            *o = s.map(value(raw, plane * npix + px, h));
        }
    });
    out
}

// --- Brightest-star anchor --------------------------------------------------

/// Fewest pixels above sky that a candidate's 5x5 neighbourhood must contain.
/// Kills single hot pixels and 2x2 clusters; a real star, even an undersampled
/// one, has its peak plus at least four lit neighbours.
const MIN_STAR_PIX: usize = 5;
/// Fraction of the peak that a candidate's surroundings must carry. This is the
/// test that actually separates stars from hot pixels: the 20-sigma floor is
/// only ~7 real sigmas on this camera, so thousands of noise pixels clear it and
/// an absolute neighbour count alone is not enough.
const MIN_STAR_CONTRAST: f64 = 0.25;
/// Half-width of the window used to score a candidate and to compute its
/// flux centroid.
const STAR_WINDOW: isize = 4;

/// Locate the brightest **non-saturated, non-spike** star.
///
/// Anchoring the 1:1 inset on the geometric centre is wrong on this data: the
/// centre peaks ~700 ADU above sky while the brightest star is ~30,000 ADU and
/// sits 1,300-3,500 px away, so a centre crop shows near-blank sky.
///
/// Candidates are local maxima that pass [`is_star`], and they are ranked by
/// **total light in a 9x9 aperture**, not by peak. Peak ranking is what made
/// this pick junk: on the cloud-affected 2x2-binned frames here the brightest
/// peak belongs to a cosmic-ray track (six pixels ~30,000 ADU with sky between
/// them, aperture flux ~130,000) or a clump of hot pixels, while the real star
/// has a lower peak and ~613,000 of flux. Saturated pixels are skipped when
/// picking a peak but still counted as light, because a bright star clipping at
/// the core is exactly the object the inset exists to judge.
pub fn find_anchor(raw: &[u8], h: &FitsHeader, st: &Stats) -> Option<(usize, usize)> {
    if h.planes != 1 {
        return None;
    }
    let ceiling = h.sat_ceiling().map(|c| c as f64 * 0.999);
    let floor = (st.median + 20.0 * st.sigma.max(1.0)) as f64;
    let median = st.median as f64;
    let bands: Vec<usize> = (0..h.height).step_by(STAR_BLOCK).collect();

    let best = bands
        .par_iter()
        .map(|&by| {
            let mut local: Option<(f64, usize, usize)> = None;
            let y1 = (by + STAR_BLOCK).min(h.height);
            for bx in (0..h.width).step_by(STAR_BLOCK) {
                let x1 = (bx + STAR_BLOCK).min(h.width);
                for y in by..y1 {
                    let row = y * h.width;
                    for x in bx..x1 {
                        let v = value(raw, row + x, h) as f64;
                        if v <= floor || ceiling.is_some_and(|c| v >= c) {
                            continue;
                        }
                        if !local_max(raw, h, y, x, by, y1, bx, x1, v, ceiling) {
                            continue;
                        }
                        if !is_star(raw, h, y, x, floor, median) {
                            continue;
                        }
                        let flux = aperture_flux(raw, h, y, x, median, ceiling);
                        if local.is_none_or(|(b, _, _)| flux > b) {
                            local = Some((flux, y, x));
                        }
                    }
                }
            }
            local
        })
        .reduce(
            || None,
            |a, b| match (a, b) {
                (Some(x), Some(y)) => {
                    if y.0 > x.0 {
                        Some(y)
                    } else {
                        Some(x)
                    }
                }
                (a, b) => a.or(b),
            },
        );

    best.map(|(_, y, x)| centroid(raw, h, st, y, x, floor, ceiling))
}

/// Highest unsaturated pixel of its neighbourhood, so each source yields one
/// candidate rather than one per pixel. Saturated neighbours are ignored: a
/// bright star clips at the core, and its brightest *wing* pixel is the
/// candidate that should represent it.
#[allow(clippy::too_many_arguments)]
fn local_max(
    raw: &[u8],
    h: &FitsHeader,
    y: usize,
    x: usize,
    by: usize,
    y1: usize,
    bx: usize,
    x1: usize,
    v: f64,
    ceiling: Option<f64>,
) -> bool {
    let hotter = |ny: usize, nx: usize| {
        let nv = value(raw, ny * h.width + nx, h) as f64;
        nv > v && !ceiling.is_some_and(|c| nv >= c)
    };
    !(x + 1 < x1 && hotter(y, x + 1))
        && !(x > bx && hotter(y, x - 1))
        && !(y + 1 < y1 && hotter(y + 1, x))
        && !(y > by && hotter(y - 1, x))
}

/// Half-width of the window used to centre the crop on a source. Wider than the
/// flux window: ranking wants a compact aperture, centring wants the whole PSF.
const CENTROID_WINDOW: isize = 7;

/// Light above sky in a `(2*STAR_WINDOW+1)^2` box. Saturated pixels contribute
/// at the ceiling: they are real light that merely clipped.
fn aperture_flux(
    raw: &[u8],
    h: &FitsHeader,
    py: usize,
    px: usize,
    median: f64,
    ceiling: Option<f64>,
) -> f64 {
    let mut flux = 0.0f64;
    for dy in -STAR_WINDOW..=STAR_WINDOW {
        for dx in -STAR_WINDOW..=STAR_WINDOW {
            let (y, x) = (py as isize + dy, px as isize + dx);
            if y < 0 || x < 0 || y as usize >= h.height || x as usize >= h.width {
                continue;
            }
            let v = value(raw, y as usize * h.width + x as usize, h) as f64;
            let v = ceiling.map_or(v, |c| v.min(c));
            flux += (v - median).max(0.0);
        }
    }
    flux
}

/// A star spreads its light over its neighbours; a hot pixel does not.
///
/// Measured on this rig: the brightest things above the floor in a Ha frame are
/// ~10,700 isolated single pixels, several of them at 53,000 ADU and sitting at
/// the *same coordinates in every exposure* - fixed-pattern hot pixels, not
/// stars. Only ~20 above-floor components in the frame are extended. So the
/// discriminator is contrast: the 5x5 surroundings must carry at least
/// [`MIN_STAR_CONTRAST`] of the peak above sky. A hot pixel's surroundings sit
/// at sky level and score ~0; a star, even an undersampled one, scores far more.
fn is_star(
    raw: &[u8],
    h: &FitsHeader,
    py: usize,
    px: usize,
    floor: f64,
    median: f64,
) -> bool {
    let peak = value(raw, py * h.width + px, h) as f64 - median;
    if peak <= 0.0 {
        return false;
    }
    let mut n = 0usize;
    let mut best_neighbour = 0.0f64;
    for dy in -2..=2 {
        for dx in -2..=2 {
            let (y, x) = (py as isize + dy, px as isize + dx);
            if y < 0 || x < 0 || y as usize >= h.height || x as usize >= h.width {
                continue;
            }
            let v = value(raw, y as usize * h.width + x as usize, h) as f64;
            if v > floor {
                n += 1;
            }
            if dy == 0 && dx == 0 {
                continue;
            }
            // Saturated neighbours only ever help: they belong to a bright star.
            best_neighbour = best_neighbour.max(v - median);
        }
    }
    n >= MIN_STAR_PIX && best_neighbour >= MIN_STAR_CONTRAST * peak
}


/// Flux-weighted centre of the source around `(py, px)`, clamped to the frame.
fn centroid(
    raw: &[u8],
    h: &FitsHeader,
    st: &Stats,
    py: usize,
    px: usize,
    floor: f64,
    ceiling: Option<f64>,
) -> (usize, usize) {
    let (mut wsum, mut xsum, mut ysum) = (0.0f64, 0.0f64, 0.0f64);
    for dy in -CENTROID_WINDOW..=CENTROID_WINDOW {
        for dx in -CENTROID_WINDOW..=CENTROID_WINDOW {
            let (y, x) = (py as isize + dy, px as isize + dx);
            if y < 0 || x < 0 || y as usize >= h.height || x as usize >= h.width {
                continue;
            }
            let v = value(raw, y as usize * h.width + x as usize, h) as f64;
            if v <= floor || ceiling.is_some_and(|c| v >= c) {
                continue;
            }
            let w = (v - st.median as f64).max(0.0);
            if w <= 0.0 {
                continue;
            }
            wsum += w;
            xsum += x as f64 * w;
            ysum += y as f64 * w;
        }
    }
    if wsum <= 0.0 {
        return (py, px);
    }
    let cy = (ysum / wsum).round().clamp(0.0, h.height as f64 - 1.0) as usize;
    let cx = (xsum / wsum).round().clamp(0.0, h.width as f64 - 1.0) as usize;
    (cy, cx)
}

// --- Preview rendering ------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    pub max_side: u32,
    pub inset: u32,
    pub quality: u8,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            max_side: 1500,
            inset: 200,
            quality: 88,
        }
    }
}

/// Render a stretched display JPEG with a native 1:1 detail inset baked in.
///
/// The inset is composited into the JPEG itself: no separate crop endpoint, no
/// canvas, no client-side overlay, and nothing left resident in memory.
pub fn render_preview(src: &Path, dst: &Path, opts: &RenderOptions) -> Result<(), FitError> {
    let h = read_header(src)?;
    let raw = read_pixels(src, &h)?;

    let stats = stats_from_raw(&raw, &h);
    let st = Stretch::new(&stats);

    let long = h.width.max(h.height);
    let scale = (opts.max_side as f64 / long as f64).min(1.0);
    let dw = ((h.width as f64 * scale).round().max(1.0)) as usize;
    let dh = ((h.height as f64 * scale).round().max(1.0)) as usize;
    let ch = h.planes;

    // Anchor on full-resolution data before anything is downsampled.
    let anchor = find_anchor(&raw, &h, &stats);

    let full = if ch == 1 {
        stretch_mono(&raw, &h, &st)
    } else {
        stretch_rgb(&raw, &h, &st)
    };

    let mut small = resize_flat(&full, h.width, h.height, ch, dw, dh);
    composite_inset(
        &mut small,
        dw,
        dh,
        ch,
        &full,
        h.width,
        h.height,
        anchor,
        opts.inset as usize,
        scale,
    );

    let color = if ch == 1 {
        ExtendedColorType::L8
    } else {
        ExtendedColorType::Rgb8
    };
    encode_jpeg(dst, &small, dw as u32, dh as u32, color, opts.quality)
}

/// Downscale a flat `w*h*ch` u8 buffer using the `image` crate's filter.
fn resize_flat(
    src: &[u8],
    w: usize,
    h: usize,
    ch: usize,
    nw: usize,
    nh: usize,
) -> Vec<u8> {
    if ch == 1 {
        let img = image::GrayImage::from_vec(w as u32, h as u32, src.to_vec())
            .expect("buffer length matches dimensions");
        let out = image::imageops::resize(&img, nw as u32, nh as u32, FilterType::CatmullRom);
        out.into_raw()
    } else {
        let img = image::RgbImage::from_vec(w as u32, h as u32, src.to_vec())
            .expect("buffer length matches dimensions");
        let out = image::imageops::resize(&img, nw as u32, nh as u32, FilterType::CatmullRom);
        out.into_raw()
    }
}

/// Paste a native-resolution crop into the downscaled host image (bottom-right)
/// and outline where on the host image it came from.
#[allow(clippy::too_many_arguments)] // one flat pixel-buffer signature; all args are the image
fn composite_inset(
    small: &mut [u8],
    sw: usize,
    sh: usize,
    ch: usize,
    full: &[u8],
    fw: usize,
    fh: usize,
    anchor: Option<(usize, usize)>,
    side: usize,
    scale: f64,
) {
    if side == 0 || side + 2 >= sw.min(sh) || side >= fw.min(fh) {
        return;
    }

    // Centre the inset on the anchor, clamped inside the full image.
    let (ay, ax) = anchor.unwrap_or((fh / 2, fw / 2));
    let sx = ax.saturating_sub(side / 2).min(fw - side);
    let sy = ay.saturating_sub(side / 2).min(fh - side);

    let ox = sw - side - 2;
    let oy = sh - side - 2;

    for y in 0..side {
        let srow = (sy + y) * fw;
        let drow = (oy + y) * sw;
        for x in 0..side {
            for c in 0..ch {
                small[(drow + ox + x) * ch + c] = full[(srow + sx + x) * ch + c];
            }
        }
    }

    // 1 px border separating the inset from the host image.
    let x0 = ox.saturating_sub(1);
    let y0 = oy.saturating_sub(1);
    let x1 = (ox + side).min(sw - 1);
    let y1 = (oy + side).min(sh - 1);
    for x in x0..=x1 {
        put(small, sw, ch, x, y0, 255);
        put(small, sw, ch, x, y1, 255);
    }
    for y in y0..=y1 {
        put(small, sw, ch, x0, y, 255);
        put(small, sw, ch, x1, y, 255);
    }

    // Outline the inset's source region on the host image, so the user knows
    // where in the frame they are looking.
    let hw = (side as f64 * scale).round() as usize;
    if hw >= 4 {
        let hx = (sx as f64 * scale).round() as usize;
        let hy = (sy as f64 * scale).round() as usize;
        let hx1 = (hx + hw).min(sw - 1);
        let hy1 = (hy + hw).min(sh - 1);
        for x in hx..=hx1 {
            put(small, sw, ch, x, hy, 255);
            put(small, sw, ch, x, hy1, 255);
        }
        for y in hy..=hy1 {
            put(small, sw, ch, hx, y, 255);
            put(small, sw, ch, hx1, y, 255);
        }
    }
}

#[inline]
fn put(buf: &mut [u8], w: usize, ch: usize, x: usize, y: usize, v: u8) {
    let at = (y * w + x) * ch;
    for c in 0..ch {
        buf[at + c] = v;
    }
}

/// Write atomically so a concurrent reader never sees a half-written JPEG.
fn encode_jpeg(
    dst: &Path,
    pixels: &[u8],
    w: u32,
    h: u32,
    color: ExtendedColorType,
    quality: u8,
) -> Result<(), FitError> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(FitError::Io)?;
    }
    let tmp = dst.with_extension("jpg.tmp");
    {
        let mut out = BufWriter::new(File::create(&tmp).map_err(FitError::Io)?);
        let enc = JpegEncoder::new_with_quality(&mut out, quality);
        enc.write_image(pixels, w, h, color)?;
        out.flush().map_err(FitError::Io)?;
    }
    std::fs::rename(&tmp, dst).map_err(FitError::Io)?;
    Ok(())
}

// --- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn card(key: &str, val: &str) -> String {
        let mut s = format!("{key:<8}= {val:>20}");
        s.truncate(80);
        s.push_str(&" ".repeat(80 - s.len()));
        s
    }

    fn card_str(key: &str, val: &str) -> String {
        let mut s = format!("{key:<8}= '{val}'");
        s.truncate(80);
        s.push_str(&" ".repeat(80 - s.len()));
        s
    }

    /// Assemble a minimal primary-IMAGE FITS file, padding data to 2880.
    fn build_fits(w: usize, h: usize, bitpix: i64, cards: &[String], data: &[u8]) -> Vec<u8> {
        let mut hdr = String::new();
        hdr.push_str(&card("SIMPLE", "T"));
        hdr.push_str(&card("BITPIX", &bitpix.to_string()));
        hdr.push_str(&card("NAXIS", "2"));
        hdr.push_str(&card("NAXIS1", &w.to_string()));
        hdr.push_str(&card("NAXIS2", &h.to_string()));
        for c in cards {
            hdr.push_str(c);
        }
        hdr.push_str(&format!("{:<80}", "END"));
        while hdr.len() % BLOCK != 0 {
            hdr.push_str(&format!("{:<80}", ""));
        }
        let mut out = hdr.into_bytes();
        out.extend_from_slice(data);
        while out.len() % BLOCK != 0 {
            out.extend_from_slice(&[0u8; 8]);
        }
        out
    }

    fn i16_data(w: usize, h: usize, mut f: impl FnMut(usize, usize) -> i16) -> Vec<u8> {
        let mut d = Vec::with_capacity(w * h * 2);
        for y in 0..h {
            for x in 0..w {
                d.extend_from_slice(&f(x, y).to_be_bytes());
            }
        }
        d
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "astro_fits_test_{}_{name}",
            std::process::id()
        ));
        p
    }

    fn write_tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = tmp(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn reads_header_with_bzero_and_cleaned_strings() {
        let cards = vec![
            card("BZERO", "32768"),
            card("BSCALE", "1"),
            card_str("FILTER", "H       "),
            card_str("IMAGETYP", "Light   "),
            card("EXPTIME", "300"),
        ];
        let f = write_tmp("hdr.fits", &build_fits(4, 4, 16, &cards, &i16_data(4, 4, |_, _| 0)));
        let h = read_header(&f).unwrap();
        assert_eq!((h.width, h.height, h.planes), (4, 4, 1));
        assert_eq!(h.bitpix, BitPix::I16);
        assert_eq!(h.bzero, 32768.0);
        assert_eq!(h.bscale, 1.0);
        // Quoted and space-padded values must be unquoted and trimmed.
        assert_eq!(h.filter.as_deref(), Some("H"));
        assert_eq!(h.imagetype.as_deref(), Some("Light"));
        assert_eq!(h.exptime, Some(300.0));
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn blank_numeric_cards_default_to_bscale_one_bzero_zero() {
        // `BZERO   =` with no value is exactly the kind of garbage real
        // headers contain; it must not fail the file or poison the stretch.
        let cards = vec![card("BZERO", ""), card("BSCALE", "")];
        let f = write_tmp("blank.fits", &build_fits(4, 4, 16, &cards, &i16_data(4, 4, |_, _| 0)));
        let h = read_header(&f).unwrap();
        assert_eq!(h.bzero, 0.0);
        assert_eq!(h.bscale, 1.0);
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn bzero_shifts_sky_positive() {
        // The single most important line in the reader: without BZERO the sky
        // reads ~-32458 and the whole stretch collapses.
        let cards = vec![card("BZERO", "32768")];
        let stored: i16 = (310i32 - 32768) as i16; // sky of 310 ADU on disk
        let f = write_tmp(
            "bzero.fits",
            &build_fits(8, 8, 16, &cards, &i16_data(8, 8, |_, _| stored)),
        );
        let h = read_header(&f).unwrap();
        let s = stats_pass(&f, &h).unwrap();
        assert!((s.median - 310.0).abs() < 1.0, "median was {}", s.median);
        assert!(s.median > 0.0);
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn reads_exact_pixel_count_not_trailing_padding() {
        // Real files pad data to a 2880-byte boundary (2,816 B of filler here).
        // Reading to EOF would append 1,408 garbage pixels.
        let w = 64;
        let h = 64;
        let data = i16_data(w, h, |x, y| (x * 3 + y) as i16);
        let mut bytes = build_fits(w, h, 16, &[], &data);
        let hdr_len = w * h * 2;
        assert!(bytes.len() > hdr_len, "expected padding, got {}", bytes.len());
        let f = write_tmp("pad.fits", &bytes);
        let hd = read_header(&f).unwrap();
        assert_eq!(hd.data_len(), hdr_len);
        let raw = read_pixels(&f, &hd).unwrap();
        assert_eq!(raw.len(), hdr_len);
        // Last real pixel must be the last written value, not filler.
        let last = value(&raw, w * h - 1, &hd);
        assert_eq!(last, ((w - 1) * 3 + (h - 1)) as f32);
        bytes.clear();
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn truncated_file_reports_truncated() {
        let f = write_tmp("trunc.fits", &build_fits(64, 64, 16, &[], &i16_data(64, 64, |_, _| 0)));
        let mut bytes = std::fs::read(&f).unwrap();
        bytes.truncate(bytes.len() - 5000);
        std::fs::write(&f, &bytes).unwrap();
        let h = read_header(&f).unwrap();
        match read_pixels(&f, &h) {
            Err(FitError::Truncated) => {}
            other => panic!("expected Truncated, got {other:?}"),
        }
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn skips_bintable_primary_and_finds_image_extension() {
        // Primary HDU with NAXIS=0, then an IMAGE extension.
        let mut hdr = String::new();
        hdr.push_str(&card("SIMPLE", "T"));
        hdr.push_str(&card("BITPIX", "8"));
        hdr.push_str(&card("NAXIS", "0"));
        hdr.push_str(&format!("{:<80}", "END"));
        // Each header must be padded to its own 2880-byte boundary.
        while hdr.len() % BLOCK != 0 {
            hdr.push_str(&format!("{:<80}", ""));
        }
        hdr.push_str(&card("XTENSION", "'IMAGE   '"));
        hdr.push_str(&card("BITPIX", "16"));
        hdr.push_str(&card("NAXIS", "2"));
        hdr.push_str(&card("NAXIS1", "8"));
        hdr.push_str(&card("NAXIS2", "8"));
        hdr.push_str(&card("BZERO", "32768"));
        hdr.push_str(&format!("{:<80}", "END"));
        while hdr.len() % BLOCK != 0 {
            hdr.push_str(&format!("{:<80}", ""));
        }
        let mut bytes = hdr.into_bytes();
        bytes.extend_from_slice(&i16_data(8, 8, |_, _| (310i32 - 32768) as i16));
        while bytes.len() % BLOCK != 0 {
            bytes.extend_from_slice(&[0u8; 8]);
        }
        let f = write_tmp("ext.fits", &bytes);
        let h = read_header(&f).unwrap();
        assert_eq!((h.width, h.height), (8, 8));
        assert_eq!(h.bzero, 32768.0);
        assert!(h.data_offset > BLOCK as u64, "should point past both headers");
        let raw = read_pixels(&f, &h).unwrap();
        assert!((value(&raw, 0, &h) - 310.0).abs() < 1.0);
        std::fs::remove_file(&f).ok();
    }

    /// Ha-shaped synthetic frame: sky ~310, sigma ~12, stars up to ~30k.
    /// Values are held as physical ADU and converted to stored (BZERO-shifted)
    /// integers only at the end.
    fn ha_frame(w: usize, h: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        let mut rand = move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 33) as f32) / ((1u64 << 31) as f32)
        };
        let mut px = vec![0f32; w * h];
        for v in px.iter_mut() {
            *v = 310.0 + (rand() + rand() + rand() + rand() - 2.0) * 20.8;
        }
        let mut splat = |cx: usize, cy: usize, amp: f32, radius: isize| {
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let (x, y) = (cx as isize + dx, cy as isize + dy);
                    if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                        continue;
                    }
                    let r2 = (dx * dx + dy * dy) as f32;
                    px[y as usize * w + x as usize] += amp * (-r2 / 4.0).exp();
                }
            }
        };
        // A handful of round stars well away from the centre.
        splat(w / 4, h / 4, 30000.0, 8);
        splat(3 * w / 4, h / 3, 22000.0, 8);
        splat(w / 3, 3 * h / 4, 15000.0, 8);
        // A faint blob dead centre: exactly what a geometric-centre crop hits.
        splat(w / 2, h / 2, 700.0, 3);

        let mut out = Vec::with_capacity(w * h * 2);
        for v in px {
            let stored = (v - 32768.0).round().clamp(i16::MIN as f32, i16::MAX as f32);
            out.extend_from_slice(&(stored as i16).to_be_bytes());
        }
        out
    }

    fn ha_file(name: &str, w: usize, h: usize, seed: u64) -> std::path::PathBuf {
        let cards = vec![card("BZERO", "32768")];
        write_tmp(name, &build_fits(w, h, 16, &cards, &ha_frame(w, h, seed)))
    }

    #[test]
    fn stats_recover_sky_sigma_and_metric() {
        let f = ha_file("stats.fits", 512, 512, 7);
        let h = read_header(&f).unwrap();
        let s = stats_pass(&f, &h).unwrap();
        assert!((s.median - 310.0).abs() < 8.0, "median {}", s.median);
        assert!((s.sigma - 12.0).abs() < 3.0, "sigma {}", s.sigma);
        assert!((s.sky_over_sigma() - 26.0).abs() < 3.0, "metric {}", s.sky_over_sigma());
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn white_point_uses_percentile_guard_not_only_sigma() {
        // med+8*sigma is ~406 ADU on this shape but p99.99 is ~975: without the
        // max() faint stars would be clipped away.
        let f = ha_file("white.fits", 512, 512, 11);
        let h = read_header(&f).unwrap();
        let s = stats_pass(&f, &h).unwrap();
        let st = Stretch::new(&s);
        assert!(st.white > s.median + 8.0 * s.sigma, "white {}", st.white);
        assert!(st.white < s.max, "white {} should not reach the peak {}", st.white, s.max);
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn stretch_maps_sky_to_midtone_and_stars_to_white() {
        let f = ha_file("stretch.fits", 512, 512, 13);
        let h = read_header(&f).unwrap();
        let raw = read_pixels(&f, &h).unwrap();
        let s = stats_from_raw(&raw, &h);
        let st = Stretch::new(&s);

        let sky = st.map(s.median) as f32;
        assert!(sky > 40.0 && sky < 120.0, "sky mapped to {sky}");
        assert_eq!(st.map(s.max), 255, "brightest star should clip to white");
        assert!(st.map(s.median - 20.0 * s.sigma) < 10, "deep background should go near black");
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn anchor_picks_brightest_star_not_geometric_centre() {
        let (w, h) = (512usize, 512usize);
        let f = ha_file("anchor.fits", w, h, 17);
        let hd = read_header(&f).unwrap();
        let raw = read_pixels(&f, &hd).unwrap();
        let st = stats_from_raw(&raw, &hd);
        let a = find_anchor(&raw, &hd, &st).expect("anchor");
        // Nearest planted star is at (w/4, h/4); the centre blob is 700 ADU.
        let d_star = ((a.0 as f64 - h as f64 / 4.0).powi(2)
            + (a.1 as f64 - w as f64 / 4.0).powi(2))
        .sqrt();
        let d_centre = ((a.0 as f64 - h as f64 / 2.0).powi(2)
            + (a.1 as f64 - w as f64 / 2.0).powi(2))
        .sqrt();
        assert!(d_star < 20.0, "anchor {a:?} not on the star (d={d_star})");
        assert!(d_centre > 100.0, "anchor fell back to the centre");
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn anchor_skips_saturated_stars() {
        let (w, h) = (256usize, 256usize);
        let cards = vec![card("BZERO", "32768")];
        let mut px = vec![0i16; w * h];
        for v in px.iter_mut() {
            *v = (310i32 - 32768) as i16;
        }
        // Saturated star (clips at the 65535 ceiling) near the centre.
        for dy in -6..=6 {
            for dx in -6..=6 {
                px[((h / 2) as isize + dy) as usize * w + ((w / 2) as isize + dx) as usize] = i16::MAX;
            }
        }
        // Bright but unsaturated star in the corner.
        for dy in -6..=6 {
            for dx in -6..=6 {
                let at = ((h / 4) as isize + dy) as usize * w + ((w / 4) as isize + dx) as usize;
                px[at] = (30000i32 - 32768) as i16;
            }
        }
        let mut data = Vec::new();
        for v in px {
            data.extend_from_slice(&v.to_be_bytes());
        }
        let f = write_tmp("sat.fits", &build_fits(w, h, 16, &cards, &data));
        let hd = read_header(&f).unwrap();
        let raw = read_pixels(&f, &hd).unwrap();
        let st = stats_from_raw(&raw, &hd);
        let a = find_anchor(&raw, &hd, &st).expect("anchor");
        let d_sat = ((a.0 as f64 - h as f64 / 2.0).powi(2)
            + (a.1 as f64 - w as f64 / 2.0).powi(2))
        .sqrt();
        assert!(d_sat > 50.0, "anchor {a:?} sat on the clipped star");
        let d_ok = ((a.0 as f64 - h as f64 / 4.0).powi(2)
            + (a.1 as f64 - w as f64 / 4.0).powi(2))
        .sqrt();
        assert!(d_ok < 20.0, "anchor {a:?} should be on the unsaturated star");
        std::fs::remove_file(&f).ok();
    }

    /// Helper: flat sky frame with an arbitrary i16 pixel edit.
    fn sky_frame(name: &str, w: usize, h: usize) -> Vec<i16> {
        let mut px = vec![0i16; w * h];
        for v in px.iter_mut() {
            *v = (310i32 - 32768) as i16;
        }
        let _ = name;
        px
    }

    fn put(px: &mut [i16], w: usize, y: isize, x: isize, adu: i32) {
        px[(y as usize) * w + x as usize] = (adu - 32768) as i16;
    }

    fn finish(px: Vec<i16>) -> Vec<u8> {
        let mut data = Vec::with_capacity(px.len() * 2);
        for v in px {
            data.extend_from_slice(&v.to_be_bytes());
        }
        data
    }

    /// The failure seen on real cloud-affected 2x2-binned frames: a cosmic-ray
    /// track has a higher peak than any star in the frame but almost no light,
    /// and peak-ranking picked it every time.
    #[test]
    fn anchor_prefers_a_star_over_a_higher_peak_cosmic_ray() {
        let (w, h) = (256usize, 256usize);
        let mut px = sky_frame("cr.fits", w, h);
        // Scattered spikes with sky between them: high peaks, no flux, and each
        // spike has a bright neighbour so the shape test alone lets it through.
        for (dy, dx, v) in [
            (0isize, 0isize, 40000i32),
            (1, 2, 35000),
            (2, 1, 30000),
            (3, 3, 38000),
            (4, 2, 33000),
            (5, 4, 36000),
        ] {
            put(&mut px, w, 200 + dy, 200 + dx, v);
        }
        // A real star: lower peak, but ~80 pixels of light behind it.
        for dy in -6isize..=6 {
            for dx in -6isize..=6 {
                let adu = 20000 - 1200 * dy.abs() as i32 - 1200 * dx.abs() as i32;
                put(&mut px, w, 60 + dy, 60 + dx, adu);
            }
        }
        let f = write_tmp(
            "cr.fits",
            &build_fits(w, h, 16, &[card("BZERO", "32768")], &finish(px)),
        );
        let hd = read_header(&f).unwrap();
        let raw = read_pixels(&f, &hd).unwrap();
        let st = stats_from_raw(&raw, &hd);
        let a = find_anchor(&raw, &hd, &st).expect("anchor");
        let d_cr = ((a.0 as f64 - 202.0).powi(2) + (a.1 as f64 - 202.0).powi(2)).sqrt();
        let d_star = ((a.0 as f64 - 60.0).powi(2) + (a.1 as f64 - 60.0).powi(2)).sqrt();
        assert!(d_cr > 50.0, "anchor {a:?} landed on the cosmic ray");
        assert!(d_star < 8.0, "anchor {a:?} should be on the star (d={d_star})");
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn anchor_rejects_a_lone_hot_pixel() {
        // The old peak-only scan found this immediately: a single 50,000 ADU
        // pixel beats every real star. It is not a star and tells you nothing
        // about focus or tracking.
        let (w, h) = (256usize, 256usize);
        let mut px = sky_frame("hot1.fits", w, h);
        put(&mut px, w, 200, 200, 50000);
        for dy in -3isize..=3 {
            for dx in -3isize..=3 {
                put(&mut px, w, 60 + dy, 60 + dx, 20000);
            }
        }
        let f = write_tmp(
            "hot1.fits",
            &build_fits(w, h, 16, &[card("BZERO", "32768")], &finish(px)),
        );
        let hd = read_header(&f).unwrap();
        let raw = read_pixels(&f, &hd).unwrap();
        let st = stats_from_raw(&raw, &hd);
        let a = find_anchor(&raw, &hd, &st).expect("anchor");
        let d_hot = ((a.0 as f64 - 200.0).powi(2) + (a.1 as f64 - 200.0).powi(2)).sqrt();
        let d_star = ((a.0 as f64 - 60.0).powi(2) + (a.1 as f64 - 60.0).powi(2)).sqrt();
        assert!(d_hot > 50.0, "anchor {a:?} landed on the hot pixel");
        assert!(d_star < 5.0, "anchor {a:?} should be on the star (d={d_star})");
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn anchor_rejects_a_two_by_two_hot_cluster() {
        let (w, h) = (256usize, 256usize);
        let mut px = sky_frame("hot2.fits", w, h);
        for dy in 0..2 {
            for dx in 0..2 {
                put(&mut px, w, 200 + dy, 200 + dx, 50000);
            }
        }
        for dy in -3isize..=3 {
            for dx in -3isize..=3 {
                put(&mut px, w, 60 + dy, 60 + dx, 20000);
            }
        }
        let f = write_tmp(
            "hot2.fits",
            &build_fits(w, h, 16, &[card("BZERO", "32768")], &finish(px)),
        );
        let hd = read_header(&f).unwrap();
        let raw = read_pixels(&f, &hd).unwrap();
        let st = stats_from_raw(&raw, &hd);
        let a = find_anchor(&raw, &hd, &st).expect("anchor");
        let d_hot = ((a.0 as f64 - 200.0).powi(2) + (a.1 as f64 - 200.0).powi(2)).sqrt();
        assert!(d_hot > 50.0, "anchor {a:?} landed on a 2x2 spike");
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn anchor_returns_the_flux_centroid_not_the_highest_pixel() {
        // A gradient-weighted blob: its brightest pixel is at the top edge, but
        // the crop should be centred on the bulk of the light.
        let (w, h) = (256usize, 256usize);
        let mut px = sky_frame("cent.fits", w, h);
        for r in 0..9 {
            for c in 0..9 {
                put(&mut px, w, 96 + r, 96 + c, 30000 - 2000 * r as i32);
            }
        }
        let f = write_tmp(
            "cent.fits",
            &build_fits(w, h, 16, &[card("BZERO", "32768")], &finish(px)),
        );
        let hd = read_header(&f).unwrap();
        let raw = read_pixels(&f, &hd).unwrap();
        let st = stats_from_raw(&raw, &hd);
        let a = find_anchor(&raw, &hd, &st).expect("anchor");
        assert!(a.0 > 98, "anchor {a:?} should sit below the top-most pixel");
        assert!(a.0 < 104, "anchor {a:?} drifted off the blob");
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn anchor_keeps_an_unsaturated_star_sharing_a_block_with_a_saturated_one() {
        // Regression: the block-max was once used for both brightness and the
        // saturation test, so a block containing any clipped pixel was thrown
        // away entirely — which discarded every bright star and left the inset
        // anchored on a faint one.
        let (w, h) = (256usize, 256usize);
        let cards = vec![card("BZERO", "32768")];
        let mut px = vec![0i16; w * h];
        for v in px.iter_mut() {
            *v = (310i32 - 32768) as i16;
        }
        // Both stars inside the same 16x16 block (rows/cols 64..80).
        for dy in -2isize..=2 {
            for dx in -2isize..=2 {
                px[((66isize + dy) as usize) * w + (66isize + dx) as usize] = i16::MAX; // clipped core
                px[((72isize + dy) as usize) * w + (72isize + dx) as usize] =
                    (30000i32 - 32768) as i16;
            }
        }
        let mut data = Vec::new();
        for v in px {
            data.extend_from_slice(&v.to_be_bytes());
        }
        let f = write_tmp("sameblock.fits", &build_fits(w, h, 16, &cards, &data));
        let hd = read_header(&f).unwrap();
        let raw = read_pixels(&f, &hd).unwrap();
        let st = stats_from_raw(&raw, &hd);
        let a = find_anchor(&raw, &hd, &st).expect("anchor");
        // The blob is flat, so the scan lands on its first max pixel; assert
        // proximity rather than an exact coordinate.
        let d = ((a.0 as f64 - 72.0).powi(2) + (a.1 as f64 - 72.0).powi(2)).sqrt();
        assert!(d < 5.0, "anchor {a:?} should be on the unsaturated star, not the clipped one");
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn render_preview_writes_decodable_jpeg_with_inset() {
        let f = ha_file("render.fits", 640, 480, 23);
        let out = tmp("render.jpg");
        render_preview(&f, &out, &RenderOptions::default()).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!((img.width(), img.height()), (640, 480));
        // The inset is a native 1:1 patch pasted bottom-right; the host image is
        // downscaled, so the inset region must contain near-white star cores.
        let g = img.to_luma8();
        let (iw, ih) = (g.width() as usize, g.height() as usize);
        let mut maxv = 0u8;
        for y in ih - 150..ih - 5 {
            for x in iw - 150..iw - 5 {
                maxv = maxv.max(g.as_raw()[y * iw + x]);
            }
        }
        assert!(maxv > 200, "inset region has no bright pixels (max {maxv})");
        std::fs::remove_file(&f).ok();
        std::fs::remove_file(&out).ok();
    }

    #[test]
    fn float_bitpix_and_rgb_are_accepted() {
        // -32 with three planes -> an RGB cube.
        let mut hdr = String::new();
        hdr.push_str(&card("SIMPLE", "T"));
        hdr.push_str(&card("BITPIX", "-32"));
        hdr.push_str(&card("NAXIS", "3"));
        hdr.push_str(&card("NAXIS1", "16"));
        hdr.push_str(&card("NAXIS2", "16"));
        hdr.push_str(&card("NAXIS3", "3"));
        hdr.push_str(&format!("{:<80}", "END"));
        while hdr.len() % BLOCK != 0 {
            hdr.push_str(&format!("{:<80}", ""));
        }
        let mut bytes = hdr.into_bytes();
        for _ in 0..16 * 16 * 3 {
            bytes.extend_from_slice(&310.0f32.to_be_bytes());
        }
        while bytes.len() % BLOCK != 0 {
            bytes.extend_from_slice(&[0u8; 8]);
        }
        let f = write_tmp("rgb.fits", &bytes);
        let h = read_header(&f).unwrap();
        assert_eq!(h.bitpix, BitPix::F32);
        assert_eq!(h.planes, 3);
        assert_eq!(h.data_len(), 16 * 16 * 3 * 4);
        let out = tmp("rgb.jpg");
        render_preview(&f, &out, &RenderOptions::default()).unwrap();
        assert!(out.exists());
        std::fs::remove_file(&f).ok();
        std::fs::remove_file(&out).ok();
    }

    #[test]
    fn non_image_only_file_is_unsupported_or_notfits() {
        let mut hdr = String::new();
        hdr.push_str(&card("SIMPLE", "T"));
        hdr.push_str(&card("BITPIX", "8"));
        hdr.push_str(&card("NAXIS", "0"));
        hdr.push_str(&format!("{:<80}", "END"));
        while hdr.len() % BLOCK != 0 {
            hdr.push_str(&format!("{:<80}", ""));
        }
        let f = write_tmp("table.fits", hdr.as_bytes());
        let r = read_header(&f);
        assert!(matches!(r, Err(FitError::NotFits) | Err(FitError::Unsupported(_))), "{r:?}");
        std::fs::remove_file(&f).ok();
    }
}
