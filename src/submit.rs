//! AstroBin submission bridge.
//!
//! AstroBin's API is **read-only** (their API page states this, and the app
//! already uses it in that mode for `/astrobin/{slug}`), so a submission has to
//! drive the web uploader. The engine is the `astrobin-upload` skill script: a
//! Node script that drives a logged-in Chrome over CDP and encodes AstroBin's
//! uploader pitfalls (the duplicated `#image-file-field` id, the five
//! `#save-button` copies with 0×0 rects, Angular needing real key events). This
//! module builds the command for that script, checks the engine
//! prerequisites, tracks the one in-flight job, and records what was submitted.
//!
//! The engine is deliberately not reimplemented in Rust: there is no CDP client
//! in this crate, and every DOM workaround here would have to be re-learned.

use std::collections::BTreeMap;
use std::fs;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::traverse::ObjectRecord;

/// Skill script, relative to the user's home.
pub const SKILL_SCRIPT: &str = ".agents/skills/astrobin-upload/scripts/upload.js";
/// `NODE_PATH` for the skill's `puppeteer-core`, relative to home.
pub const NODE_PATH_DIR: &str = ".cache/astrobin-upload/node_modules";
/// CDP port the skill connects to (matches the script's `--port` default).
pub const CDP_PORT: u16 = 9222;

/// Inventory telescope directory -> AstroBin "Your setups" name. Keys are the
/// directory names under the inventory root. Telescopes with no entry (e.g.
/// `6D`) get no setup applied, exactly as the skill does.
pub fn setup_by_telescope(telescope: &str) -> Option<&'static str> {
    match telescope {
        "135mm" => Some("135mm"),
        "Esprit100" => Some("Esprit"),
        "8RC" => Some("RC8"),
        "10RC" => Some("RC10"),
        _ => None,
    }
}

#[derive(Debug, Default, Clone)]
pub struct SubmitOptions {
    pub auto_description: bool,
    pub llm: bool,
    pub dry_run: bool,
    pub no_setup: bool,
    pub no_acquisition: bool,
    /// Draft to resume, for a run that aborted inside the wizard: the skill
    /// reuses the open tab instead of starting a new upload.
    pub resume: Option<String>,
    /// Curated description text (already reviewed/edited on the submit page).
    /// When set, the driver uploads it verbatim via `--description-file` and
    /// skips web retrieval and the LLM entirely.
    pub description_file: Option<PathBuf>,
}

/// The master image to upload: the largest `.jpg` in `master/`.
///
/// `final_images` holds the `master/` JPEGs the scanner already found. Small
/// preview JPEGs (`*_small`) are skipped: uploading a thumbnail is never the
/// intent, and picking by size makes the choice robust to naming.
pub fn master_file(record: &ObjectRecord) -> Option<PathBuf> {
    let master_dir = record.path.join("master");
    let mut best: Option<(u64, PathBuf)> = None;
    for name in &record.final_images {
        if name.to_lowercase().contains("small") {
            continue;
        }
        let p = master_dir.join(name);
        let len = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        if best.as_ref().map_or(true, |(b, _)| len > *b) {
            best = Some((len, p));
        }
    }
    best.map(|(_, p)| p)
}

/// Default title from the object directory name: `NGC_1333` -> `NGC 1333`.
/// The preview page lets the user edit it before confirming.
pub fn default_title(record: &ObjectRecord) -> String {
    let name = record.object.trim();
    if name.is_empty() {
        return record.telescope.clone();
    }
    let spaced = name.replace('_', " ");
    // `Bubble` alone is not a title; a nickname needs its object type.
    if regex::Regex::new(r"(?i)^(NGC|IC|Sh\s?2|LBN|Messier|M)\s?\d+").is_ok_and(|re| re.is_match(&spaced)) {
        spaced
    } else if regex::Regex::new(r"(?i)nebula|galaxy|cluster|remnant|asterism")
        .is_ok_and(|re| re.is_match(&spaced))
    {
        spaced
    } else {
        format!("{spaced} Nebula")
    }
}

/// Ahead-of-time object description shown (and editable) on the submit page.
/// Mirrors the `DESCRIBE_JSON` line the driver's `--describe-only` mode prints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectDescription {
    pub ok: bool,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub sources: Vec<String>,
}

/// Generate the object description without touching Chrome: runs the driver
/// with `--describe-only`, which does the web retrieval, the fact-guarded LLM
/// pass, and prints one `DESCRIBE_JSON {…}` line. Blocks (tens of seconds for
/// retrieval + inference); async callers must use `spawn_blocking`.
pub fn describe_object(record: &ObjectRecord) -> anyhow::Result<ObjectDescription> {
    let node = node_path().ok_or_else(|| anyhow::anyhow!("node is not on PATH"))?;
    let script = home_dir().join(SKILL_SCRIPT);
    if !script.is_file() {
        anyhow::bail!("skill script missing: {}", script.display());
    }
    let mut args: Vec<String> = vec![script.display().to_string()];
    if let Some(file) = master_file(record) {
        args.push(file.display().to_string());
    }
    args.push(default_title(record));
    args.push("--object-dir".to_string());
    args.push(record.path.display().to_string());
    args.push("--auto-description".to_string());
    args.push("--llm-description".to_string());
    args.push("--describe-only".to_string());
    let mut child = std::process::Command::new(&node)
        .args(&args)
        .env("NODE_PATH", home_dir().join(NODE_PATH_DIR))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("cannot run {}: {e}", script.display()))?;
    // Retrieval timeouts inside the driver stay well under this ceiling; the
    // guard is only against a wedged node process holding the page open.
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("describe timed out after 300 s");
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                let _ = child.wait();
                anyhow::bail!("wait failed: {e}");
            }
        }
    }
    let out = child.wait_with_output().map_err(|e| anyhow::anyhow!("capture failed: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("DESCRIBE_JSON "))
        .ok_or_else(|| {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail = err.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
            anyhow::anyhow!("driver printed no DESCRIBE_JSON line; stderr: {tail}")
        })?;
    let desc: ObjectDescription =
        serde_json::from_str(line).map_err(|e| anyhow::anyhow!("bad DESCRIBE_JSON: {e}"))?;
    if !desc.ok || desc.text.as_deref().unwrap_or("").trim().is_empty() {
        anyhow::bail!(
            "description generation found nothing (path: {})",
            desc.path.as_deref().unwrap_or("?")
        );
    }
    Ok(desc)
}

/// One acquisition session row, mirroring a row of the driver's `ACQ_JSON`
/// payload (`upload.js --acquisition-json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcquisitionRow {
    pub date: String,
    pub number: u64,
    pub duration: f64,
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub iso: Option<i64>,
}

/// Scanned acquisition data for one object. The driver computes it from file
/// headers — FITS `EXPTIME` for deep-sky rigs, EXIF `ExposureTime` for DSLR
/// (CR3) objects — which is exact, unlike the inventory scan that counts DSLR
/// frames but assumes 30 s subs to stay fast.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcquisitionInfo {
    pub frames: u64,
    pub seconds: f64,
    pub rows: Vec<AcquisitionRow>,
}

impl AcquisitionInfo {
    /// True when the scan actually found light frames.
    pub fn has_frames(&self) -> bool {
        self.frames > 0
    }
}

/// Scan the object's acquisition data by running the driver in
/// `--acquisition-json` mode: it reads the same headers the upload will
/// import, including per-frame EXIF for DSLR frames. Reading thousands of
/// EXIF headers takes seconds, so this only ever runs on a blocking thread,
/// on page load; the caller caches the result per object.
pub fn scan_acquisition(record: &ObjectRecord) -> anyhow::Result<AcquisitionInfo> {
    let node = node_path().ok_or_else(|| anyhow::anyhow!("node is not on PATH"))?;
    let script = home_dir().join(SKILL_SCRIPT);
    if !script.is_file() {
        anyhow::bail!("skill script missing: {}", script.display());
    }
    let args: Vec<String> = vec![
        script.display().to_string(),
        "--object-dir".to_string(),
        record.path.display().to_string(),
        "--acquisition-json".to_string(),
    ];
    let mut child = std::process::Command::new(&node)
        .args(&args)
        .env("NODE_PATH", home_dir().join(NODE_PATH_DIR))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("cannot run {}: {e}", script.display()))?;
    // EXIF/FITS header reads are milliseconds per batch; the ceiling only
    // guards against a wedged node holding the page open.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("acquisition scan timed out after 120 s");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let _ = child.wait();
                anyhow::bail!("wait failed: {e}");
            }
        }
    }
    let out = child.wait_with_output().map_err(|e| anyhow::anyhow!("capture failed: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    parse_acq_json(&stdout).ok_or_else(|| {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail = err.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        anyhow::anyhow!("driver printed no ACQ_JSON line; stderr: {tail}")
    })
}

/// Parse the driver's `ACQ_JSON` line out of its stdout (the driver prints
/// progress logs around it).
pub fn parse_acq_json(stdout: &str) -> Option<AcquisitionInfo> {
    let line = stdout.lines().rev().find_map(|l| l.trim().strip_prefix("ACQ_JSON "))?;
    serde_json::from_str(line).ok()
}

/// Full `node …/upload.js …` argv, excluding the interpreter.
pub fn build_argv(record: &ObjectRecord, opts: &SubmitOptions, title: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let script = home_dir().join(SKILL_SCRIPT);
    argv.push(script.display().to_string());
    if let Some(file) = master_file(record) {
        argv.push(file.display().to_string());
    }
    argv.push(title.to_string());
    if let Some(draft) = &opts.resume {
        argv.push("--resume".to_string());
        argv.push(draft.clone());
    }
    if let Some(setup) = setup_by_telescope(&record.telescope) {
        if !opts.no_setup {
            argv.push("--setup".to_string());
            argv.push(setup.to_string());
        }
    }
    argv.push("--object-dir".to_string());
    argv.push(record.path.display().to_string());
    match &opts.description_file {
        // The page supplied a reviewed description: it wins, and the driver
        // must not fetch or generate anything.
        Some(p) => {
            argv.push("--description-file".to_string());
            argv.push(p.display().to_string());
        }
        None => {
            if opts.auto_description {
                argv.push("--auto-description".to_string());
            }
            if opts.llm {
                argv.push("--llm-description".to_string());
            }
        }
    }
    if opts.no_setup {
        argv.push("--no-setup".to_string());
    }
    if opts.no_acquisition {
        argv.push("--no-acquisition".to_string());
    }
    if opts.dry_run {
        argv.push("--dry-run".to_string());
    }
    argv
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

// --- Preflight ---------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct Preflight {
    pub node: bool,
    pub puppeteer: bool,
    pub script: bool,
    pub cdp_open: bool,
    /// A Chrome process exists but may have no DevTools port; that changes what
    /// launching it would cost the user.
    pub chrome_running: bool,
    pub pi: bool,
    pub brave_key: bool,
}

/// Absolute path to `node`, resolved with `command -v`. Spawn sites must use
/// this: the server inherits the PATH of whatever launched it, and on this
/// machine `node` lives under mise shims that an interactive shell has and a
/// detached launcher typically does not.
pub fn node_path() -> Option<String> {
    lookup("node")
}

fn lookup(name: &str) -> Option<String> {
    let out = std::process::Command::new("sh")
        // The command string must carry the name: `sh -c 'command -v' node`
        // passes `node` as `$0`, so `command -v` runs with no argument.
        .args(["-c", &format!("command -v {name}")])
        .stdout(std::process::Stdio::piped())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn cdp_open(port: u16) -> bool {
    // A connect probe is the honest test: the skill connects to the DevTools
    // port, and a Chrome that refuses the port is the known failure mode.
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok()
}

pub fn preflight(port: u16) -> Preflight {
    Preflight {
        node: node_path().is_some(),
        puppeteer: home_dir().join(NODE_PATH_DIR).join("puppeteer-core").is_dir(),
        script: home_dir().join(SKILL_SCRIPT).is_file(),
        cdp_open: cdp_open(port),
        chrome_running: chrome_running(),
        pi: lookup("pi").is_some(),
        brave_key: std::env::var("BRAVE_API_KEY").is_ok(),
    }
}

// --- Chrome launcher ---------------------------------------------------------

/// Skill script that starts a visible, logged-in Chrome with the DevTools port
/// on a copy of the real profile. Reused rather than reimplemented: it encodes
/// the profile-copy exclusions and the anti-throttling flags the uploader needs
/// on this Wayland session.
pub const CHROME_SCRIPT: &str = ".agents/skills/astrobin-upload/scripts/start-chrome.sh";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeOutcome {
    AlreadyUp,
    Launched,
    Failed,
}

pub fn chrome_running() -> bool {
    std::process::Command::new("pgrep")
        .args(["-x", "chrome"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Make sure a CDP-enabled Chrome is listening, starting one when needed.
///
/// The debug Chrome runs on a copied profile, so it opens as a separate window
/// and coexists with the user's own Chrome; only a stale previous debug
/// instance blocks the launch (the script reports that and suggests --kill).
///
/// Blocks while the script runs: first use copies the browser profile (about a
/// minute), after that Chrome answers the port in a few seconds. Callers on an
/// async runtime must wrap this in `spawn_blocking`. Returns the script's own
/// log lines so the reason for a failure reaches the page.
pub fn ensure_chrome(port: u16) -> (ChromeOutcome, Vec<String>) {
    if cdp_open(port) {
        return (
            ChromeOutcome::AlreadyUp,
            vec![format!("CDP already listening on 127.0.0.1:{port}")],
        );
    }
    let script = home_dir().join(CHROME_SCRIPT);
    if !script.is_file() {
        return (
            ChromeOutcome::Failed,
            vec![format!("launcher script missing: {}", script.display())],
        );
    }
    let log = std::env::temp_dir().join("astro_inventory_chrome_launch.log");
    let out_file = match fs::File::create(&log) {
        Ok(f) => f,
        Err(e) => return (ChromeOutcome::Failed, vec![format!("cannot write {}: {e}", log.display())]),
    };
    let err_file = match out_file.try_clone() {
        Ok(f) => f,
        Err(e) => return (ChromeOutcome::Failed, vec![format!("cannot write {}: {e}", log.display())]),
    };
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&script)
        .stdin(std::process::Stdio::null())
        .stdout(out_file)
        .stderr(err_file);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (ChromeOutcome::Failed, vec![format!("cannot run {}: {e}", script.display())]);
        }
    };
    // 240 s covers a first-run profile copy; the script disowns Chrome, so the
    // browser outlives this helper either way.
    let started = std::time::Instant::now();
    let deadline = Duration::from_secs(240);
    let mut timed_out = false;
    // The exit status is not the verdict: the launcher can exit non-zero while
    // Chrome comes up fine, so the port probe decides.
    let _status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if started.elapsed() > deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(400));
            }
            Err(_) => {
                let _ = child.wait();
                break None;
            }
        }
    };
    let mut lines: Vec<String> = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .filter(|l| !l.trim().is_empty())
        .collect();
    if cdp_open(port) {
        (ChromeOutcome::Launched, lines)
    } else {
        if timed_out {
            lines.push(format!("launcher still running after {}s", deadline.as_secs()));
        }
        if lines.is_empty() {
            lines.push("launcher produced no output".to_string());
        }
        (ChromeOutcome::Failed, lines)
    }
}

// --- Job state ---------------------------------------------------------------

#[derive(Debug)]
pub struct JobState {
    pub lines: Vec<String>,
    pub status: String,
    pub draft_url: Option<String>,
    pub done: bool,
    pub ok: bool,
}

impl JobState {
    pub fn new(status: &str) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(JobState {
            lines: Vec::new(),
            status: status.to_string(),
            draft_url: None,
            done: false,
            ok: false,
        }))
    }
}

pub type Job = (String, Arc<Mutex<JobState>>);

/// Snapshot of a job for rendering: `(key, status, lines, done, ok, draft)`.
pub fn snapshot(job: &Job) -> (String, String, Vec<String>, bool, bool, Option<String>) {
    let st = job.1.lock().unwrap();
    (job.0.clone(), st.status.clone(), st.lines.clone(), st.done, st.ok, st.draft_url.clone())
}


pub fn draft_url_from(line: &str) -> Option<String> {
    line.split_once("Saved draft:").map(|(_, rest)| rest.trim().to_string())
}

/// Last non-empty line of a log, used as the failure reason in error messages:
/// the upload driver reports why it stopped on its last line.
pub fn last_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

// --- Submitted registry ------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submitted {
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub title: String,
    /// RFC3339 or epoch timestamp. Accepts a JSON string *or* number so a
    /// hand-edited registry never fails to parse (which would silently drop
    /// every entry).
    #[serde(default, deserialize_with = "de_loose_string")]
    pub ts: String,
}

fn de_loose_string<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        other => return Err(D::Error::custom(format!("expected string or number, got {other}"))),
    })
}

pub fn submitted_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("submitted.json")
}

pub fn load_submitted(cache_dir: &Path) -> BTreeMap<String, Submitted> {
    fs::read(submitted_path(cache_dir))
        .and_then(|b| serde_json::from_slice(&b).map_err(std::io::Error::other))
        .unwrap_or_default()
}

pub fn record_submitted(cache_dir: &Path, key: &str, rec: Submitted) -> std::io::Result<()> {
    let mut map = load_submitted(cache_dir);
    map.insert(key.to_string(), rec);
    let json = serde_json::to_string_pretty(&map).map_err(std::io::Error::other)?;
    fs::write(submitted_path(cache_dir), json)
}

/// Frontmatter key the submit bridge owns in `plan.md`.
pub const PLAN_URL_KEY: &str = "astrobin";

/// Write the AstroBin draft URL into the object's `plan.md` frontmatter.
///
/// Only `astrobin:` is touched: a hand-kept `link:` (an older published image,
/// a target list, whatever the owner put there) survives. Creating `plan.md`
/// when it is missing is deliberate — an object with a submitted draft but no
/// plan file should still show where the image went.
///
/// Returns `Ok(true)` when the file changed. Callers must not fail a finished
/// upload over a bookkeeping write, so they log instead.
pub fn record_plan_link(object_dir: &Path, url: &str) -> std::io::Result<bool> {
    if url.is_empty() {
        return Ok(false);
    }
    let path = object_dir.join("plan.md");
    let current = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let updated = crate::plan::set_meta(&current, PLAN_URL_KEY, url);
    if updated == current {
        return Ok(false);
    }
    fs::write(&path, updated)?;
    Ok(true)
}

/// AstroBin slug from a draft URL (`https://app.astrobin.com/i/pvc55t`).
pub fn slug_from_url(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traverse::ObjectRecord;
    use std::path::PathBuf;

    fn record(telescope: &str, object: &str, masters: Vec<&str>) -> ObjectRecord {
        ObjectRecord {
            telescope: telescope.to_string(),
            object: object.to_string(),
            path: PathBuf::from(format!("/data/Astro/CCD/{telescope}/{object}")),
            filters: Default::default(),
            cr2cr3_count: 0,
            latest: None,
            has_project: true,
            has_final: !masters.is_empty(),
            size_bytes: 0,
            final_images: masters.iter().map(|s| s.to_string()).collect(),
            plan: None,
            has_todo: false,
            plan_mtime: None,
            transit: None,
            transit_sort: 0.0,
        }
    }

    #[test]
    fn setup_map_matches_the_inventory_directories() {
        assert_eq!(setup_by_telescope("10RC"), Some("RC10"));
        assert_eq!(setup_by_telescope("8RC"), Some("RC8"));
        assert_eq!(setup_by_telescope("Esprit100"), Some("Esprit"));
        assert_eq!(setup_by_telescope("135mm"), Some("135mm"));
        assert_eq!(setup_by_telescope("6D"), None);
    }

    #[test]
    fn master_file_skips_small_previews_and_picks_the_largest() {
        let dir = std::env::temp_dir().join(format!("astro_submit_{}", std::process::id()));
        let master = dir.join("master");
        fs::create_dir_all(&master).unwrap();
        fs::write(master.join("Bubble10RC.jpg"), vec![b'1'; 100]).unwrap();
        fs::write(master.join("Bubble10RC_small.jpg"), vec![b'1'; 10]).unwrap();
        fs::write(master.join("BubbleHa.jpg"), vec![b'1'; 300]).unwrap();

        let mut rec = record("10RC", "Bubble", vec!["Bubble10RC.jpg", "Bubble10RC_small.jpg", "BubbleHa.jpg"]);
        rec.path = dir.clone();
        let picked = master_file(&rec).unwrap();
        assert_eq!(picked.file_name().unwrap().to_str().unwrap(), "BubbleHa.jpg");

        // A designation title stays as-is; a nickname gets its type.
        assert_eq!(default_title(&rec), "Bubble Nebula");
        let ngc = record("8RC", "NGC_1333", vec![]);
        assert_eq!(default_title(&ngc), "NGC 1333");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn argv_resumes_an_open_draft() {
        let rec = record("Esprit100", "LBN762", vec!["LBN762.jpg"]);
        let opts = SubmitOptions {
            auto_description: true,
            resume: Some("6g57uf".to_owned()),
            ..Default::default()
        };
        let argv = build_argv(&rec, &opts, "LBN 762");
        let joined = argv.join(" ");
        // The slug goes with its own flag, and the title stays where the
        // driver expects it (argv[2]).
        assert_eq!(argv[2], "LBN 762");
        assert!(joined.contains("--resume 6g57uf"));
        assert!(joined.contains("--setup Esprit"));
        assert!(joined.contains("--auto-description"));
    }

    #[test]
    fn argv_uses_description_file_instead_of_fetch_flags() {
        let rec = record("10RC", "Bubble", vec!["Bubble10RC.jpg"]);
        let opts = SubmitOptions {
            auto_description: true,
            llm: true,
            description_file: Some(PathBuf::from("/tmp/desc.txt")),
            ..Default::default()
        };
        let joined = build_argv(&rec, &opts, "Bubble Nebula").join(" ");
        assert!(joined.contains("--description-file /tmp/desc.txt"));
        assert!(!joined.contains("--auto-description"));
        assert!(!joined.contains("--llm-description"));
    }

    #[test]
    fn argv_carries_setup_object_dir_and_flags() {
        let rec = record("10RC", "Bubble", vec!["Bubble10RC.jpg"]);
        let opts = SubmitOptions {
            auto_description: true,
            llm: true,
            ..Default::default()
        };
        let argv = build_argv(&rec, &opts, "Bubble Nebula");
        assert!(argv[0].ends_with("astrobin-upload/scripts/upload.js"));
        assert_eq!(argv[2], "Bubble Nebula");
        let joined = argv.join(" ");
        assert!(joined.contains("--setup RC10"));
        assert!(joined.contains("--object-dir /data/Astro/CCD/10RC/Bubble"));
        assert!(joined.contains("--auto-description"));
        assert!(joined.contains("--llm-description"));
        // --no-setup suppresses the derived setup.
        let opts2 = SubmitOptions { no_setup: true, ..Default::default() };
        assert!(!build_argv(&rec, &opts2, "Bubble").join(" ").contains("--setup RC10"));
        // No mapping, no setup.
        let rec6 = record("6D", "CSSP-2025", vec!["x.jpg"]);
        assert!(!build_argv(&rec6, &SubmitOptions::default(), "x").join(" ").contains("--setup"));
    }

    #[test]
    fn milestones_and_slug_are_extracted() {
        let line = "Saved draft: https://app.astrobin.com/i/pvc55t";
        assert_eq!(draft_url_from(line), Some("https://app.astrobin.com/i/pvc55t".to_string()));
        assert_eq!(slug_from_url("https://app.astrobin.com/i/pvc55t/"), "pvc55t");
    }

    #[test]
    fn registry_tolerates_missing_and_numeric_fields() {
        let dir = std::env::temp_dir().join(format!("astro_submit_loose_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            submitted_path(&dir),
            r#"{"10RC/Bubble":{"slug":"yerut6","url":"https://app.astrobin.com/i/yerut6","ts":1795380437}}"#,
        )
        .unwrap();
        let map = load_submitted(&dir);
        let s = map.get("10RC/Bubble").expect("entry should load");
        assert_eq!(s.slug, "yerut6");
        assert_eq!(s.ts, "1795380437");
        assert_eq!(s.title, "");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn registry_roundtrips() {
        let dir = std::env::temp_dir().join(format!("astro_submit_reg_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        record_submitted(
            &dir,
            "10RC/Bubble",
            Submitted {
                slug: "pvc55t".into(),
                url: "https://app.astrobin.com/i/pvc55t".into(),
                file: "/data/Astro/CCD/10RC/Bubble/master/Bubble10RC.jpg".into(),
                title: "Bubble Nebula".into(),
                ts: "2026-11-22T12:00:00+01:00".into(),
            },
        )
        .unwrap();
        let map = load_submitted(&dir);
        assert_eq!(map.get("10RC/Bubble").unwrap().slug, "pvc55t");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensure_chrome_reuses_a_listening_cdp_port() {
        // Bind a throwaway port so `cdp_open` is true without touching Chrome:
        // the launcher must probe first and never start a second browser.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (outcome, lines) = ensure_chrome(port);
        assert_eq!(outcome, ChromeOutcome::AlreadyUp);
        assert!(lines.iter().any(|l| l.contains("already listening")));
    }

    #[test]
    fn plan_link_is_added_without_touching_a_user_link() {
        let dir = std::env::temp_dir().join(format!("astro_submit_plan_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("plan.md"),
            "---\nconstellation: Perseus\nlink: https://app.astrobin.com/i/olduser\n---\n\n## Plan\nnotes\n",
        )
        .unwrap();
        let url = "https://app.astrobin.com/i/05feat";
        assert!(record_plan_link(&dir, url).unwrap());
        assert!(!record_plan_link(&dir, url).unwrap(), "second write should be a no-op");
        let text = fs::read_to_string(dir.join("plan.md")).unwrap();
        assert!(text.contains("astrobin: https://app.astrobin.com/i/05feat"));
        assert!(text.contains("link: https://app.astrobin.com/i/olduser"));
        assert!(text.contains("notes"));
        // Missing plan.md gets created.
        let bare = dir.join("nope");
        fs::create_dir_all(&bare).unwrap();
        assert!(record_plan_link(&bare, url).unwrap());
        assert!(fs::read_to_string(bare.join("plan.md")).unwrap().contains(url));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parses_acq_json_line() {
        let stdout = "Inventory: 135mm/Orion\nAcquisition: 2 row(s)\nACQ_JSON {\"frames\":66,\"seconds\":7986,\"rows\":[{\"date\":\"2026-11-09\",\"number\":25,\"duration\":121,\"filter\":null,\"iso\":6400},{\"date\":\"2026-11-10\",\"number\":41,\"duration\":121.4,\"filter\":null,\"iso\":6400}]}\n";
        let a = parse_acq_json(stdout).expect("parses");
        assert_eq!(a.frames, 66);
        assert_eq!(a.rows.len(), 2);
        assert!((a.rows[1].duration - 121.4).abs() < 1e-9);
        assert_eq!(a.rows[0].iso, Some(6400));
        assert!(a.has_frames());
    }

    #[test]
    fn acq_json_filters_null_filter_and_missing_line() {
        // FITS rows carry filter letters; missing iso is null.
        let stdout = "ACQ_JSON {\"frames\":36,\"seconds\":10800,\"rows\":[{\"date\":\"2025-12-18\",\"number\":36,\"duration\":300,\"filter\":\"H\",\"iso\":null}]}";
        let a = parse_acq_json(stdout).expect("parses");
        assert_eq!(a.rows[0].filter.as_deref(), Some("H"));
        assert_eq!(a.rows[0].iso, None);
        assert_eq!(parse_acq_json("no machine line here"), None);
    }
}
