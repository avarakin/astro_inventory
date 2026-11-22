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

/// Full `node …/upload.js …` argv, excluding the interpreter.
pub fn build_argv(record: &ObjectRecord, opts: &SubmitOptions, title: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let script = home_dir().join(SKILL_SCRIPT);
    argv.push(script.display().to_string());
    if let Some(file) = master_file(record) {
        argv.push(file.display().to_string());
    }
    argv.push(title.to_string());
    if let Some(setup) = setup_by_telescope(&record.telescope) {
        if !opts.no_setup {
            argv.push("--setup".to_string());
            argv.push(setup.to_string());
        }
    }
    argv.push("--object-dir".to_string());
    argv.push(record.path.display().to_string());
    if opts.auto_description {
        argv.push("--auto-description".to_string());
    }
    if opts.llm {
        argv.push("--llm-description".to_string());
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
        pi: lookup("pi").is_some(),
        brave_key: std::env::var("BRAVE_API_KEY").is_ok(),
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

/// A line worth surfacing in the progress panel: the script prints its own
/// milestones, so a substring test is enough and keeps the panel short.
pub fn is_milestone(line: &str) -> bool {
    line.contains("UPLOAD COMPLETE")
        || line.contains("Description path:")
        || line.contains("Saved draft:")
        || line.contains("In staging area")
        || line.contains("llm description rejected")
        || line.contains("llm call failed")
        || line.contains("Publish button present")
        || line.contains("Acquisition:")
        || line.contains("AstroBin setup:")
        || line.contains("Page errors:")
        || line.contains("warning:")
}

pub fn draft_url_from(line: &str) -> Option<String> {
    line.split_once("Saved draft:").map(|(_, rest)| rest.trim().to_string())
}

// --- Submitted registry ------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submitted {
    pub slug: String,
    pub url: String,
    pub file: String,
    pub title: String,
    pub ts: String,
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
        assert!(is_milestone(line));
        assert_eq!(draft_url_from(line), Some("https://app.astrobin.com/i/pvc55t".to_string()));
        assert_eq!(slug_from_url("https://app.astrobin.com/i/pvc55t/"), "pvc55t");
        assert!(!is_milestone("chunk 312 sent"));
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
}
