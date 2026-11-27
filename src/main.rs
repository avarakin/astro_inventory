mod traverse;
mod transit;
mod location;
mod plan;
mod server;
mod render;
mod review;
mod submit;

use std::path::PathBuf;

use clap::Parser;
use clap::Subcommand;
use serde::Deserialize;

#[derive(Parser)]
#[command(name = "astro_inventory", about = "Astronomy image inventory web server")]
struct Cli {
    /// JSON config file (see config.example.json); CLI flags override it.
    /// When omitted, ./config.json is used if present.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Root directory of CCD data
    #[arg(long)]
    root: Option<std::path::PathBuf>,

    /// Staging directory, listed as a launcher on /staging
    #[arg(long)]
    staging: Option<std::path::PathBuf>,

    /// Extra directories the generic review screen may open (in addition to
    /// the CCD root and the staging dir)
    #[arg(long, value_delimiter = ',')]
    review_roots: Vec<std::path::PathBuf>,

    /// Autorepeat throttle for the review screen's arrow keys, in ms
    #[arg(long)]
    nav_repeat_ms: Option<u64>,

    /// Minutes between staging preview sweeps; 0 disables the sweep
    #[arg(long)]
    preview_sweep_min: Option<u64>,

    /// Where rendered preview JPEGs are cached. Must live OUTSIDE the staging
    /// tree: every staging telescope dir is a Syncthing folder, so anything
    /// written inside it replicates to the capture machines.
    #[arg(long)]
    cache: Option<std::path::PathBuf>,

    /// Rows per page; 0 disables paging
    #[arg(long)]
    page_size: Option<usize>,

    /// Directory with extracted Compendium data (see `make compendium`)
    #[arg(long)]
    compendium_dir: Option<std::path::PathBuf>,

    /// Override observer latitude (deg N); default: GeoIP
    #[arg(long)]
    latitude: Option<f64>,

    /// Override observer longitude (deg E); default: GeoIP
    #[arg(long)]
    longitude: Option<f64>,

    /// TCP port to bind
    #[arg(long, default_value_t = 5000)]
    port: u16,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Submit an inventory object to AstroBin (unlisted draft, never published)
    Submit {
        /// Object as `telescope/name`, e.g. `10RC/Bubble`
        target: String,
        /// Title override (default: derived from the object directory name)
        #[arg(long)]
        title: Option<String>,
        /// LLM phrasing layer over the scraped sources (fact-guarded)
        #[arg(long)]
        llm: bool,
        /// Skip the auto description
        #[arg(long)]
        no_auto: bool,
        /// Skip the AstroBin setup assignment
        #[arg(long)]
        no_setup: bool,
        /// Skip the acquisition CSV
        #[arg(long)]
        no_acquisition: bool,
        /// Show what would be uploaded; no browser, no upload
        #[arg(long)]
        dry_run: bool,
        /// Finish an interrupted draft (`upload.js --resume <slug>`)
        #[arg(long)]
        resume: Option<String>,
    },
    /// Write recorded AstroBin draft links into each object's plan.md
    LinkPlans,
}

#[derive(Deserialize, Default)]
struct Config {
    root: Option<PathBuf>,
    staging: Option<PathBuf>,
    cache: Option<PathBuf>,
    page_size: Option<usize>,
    compendium_dir: Option<PathBuf>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    review_roots: Option<Vec<PathBuf>>,
    nav_repeat_ms: Option<u64>,
    preview_sweep_min: Option<u64>,
    /// Staging telescope name -> inventory telescope name. Staging folders are
    /// named after the mount (Pier, RC); the CCD tree names them per instrument
    /// (10RC, Esprit100). The map lets the move screen scope and preselect the
    /// right destination telescope despite the mismatch.
    staging_tel_map: Option<std::collections::HashMap<String, String>>,
}

/// Precedence: CLI flag > config file > built-in default.
fn pick<T>(cli: Option<T>, cfg: Option<T>, default: T) -> T {
    cli.or(cfg).unwrap_or(default)
}

fn default_cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("astro_inventory")
}

/// Decide which config file to read: an explicit `--config`, otherwise
/// `./config.json` when it exists.
///
/// Auto-discovery matters because a missing `--config` is otherwise silent: the
/// server comes up looking perfectly healthy with staging simply absent.
/// `start_server.sh` cds to the project directory, so a relative default lands
/// on the same file for both the script and a bare binary invocation.
fn config_path(explicit: Option<&std::path::Path>) -> anyhow::Result<Option<PathBuf>> {
    config_path_in(std::path::Path::new("."), explicit)
}

fn config_path_in(
    dir: &std::path::Path,
    explicit: Option<&std::path::Path>,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(p) = explicit {
        if !p.is_file() {
            anyhow::bail!("config file does not exist: {}", p.display());
        }
        return Ok(Some(p.to_path_buf()));
    }
    let fallback = dir.join("config.json");
    Ok(fallback.is_file().then_some(fallback))
}

/// CLI path for `astro_inventory submit telescope/name …`: shares the argv
/// builder with the web route, so the CLI and the page produce one command.
fn run_submit(
    root: &std::path::Path,
    cache_dir: &std::path::Path,
    target: &str,
    title: Option<&str>,
    llm: bool,
    no_auto: bool,
    no_setup: bool,
    no_acquisition: bool,
    dry_run: bool,
    resume: Option<&str>,
) -> anyhow::Result<()> {
    let (tel, name) = target
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("target must be telescope/name, e.g. 10RC/Bubble"))?;
    let rec = traverse::traverse(root, 0.0)
        .into_iter()
        .find(|r| r.telescope == tel && r.object == name)
        .ok_or_else(|| anyhow::anyhow!("{tel}/{name} is not in the inventory"))?;
    let object_dir = rec.path.clone();
    let opts = submit::SubmitOptions {
        auto_description: !no_auto,
        llm,
        dry_run,
        no_setup,
        no_acquisition,
        resume: resume.map(str::to_owned),
        // The CLI has no reviewed-description input; the driver fetches its own.
        description_file: None,
    };
    let derived = submit::default_title(&rec);
    let title = title.unwrap_or(&derived);
    let argv = submit::build_argv(&rec, &opts, title);
    if submit::master_file(&rec).is_none() {
        anyhow::bail!("no master JPEG in {}/master", rec.path.display());
    }
    let pf = submit::preflight(submit::CDP_PORT);
    eprintln!(
        "preflight: node={} puppeteer={} script={} cdp={} chrome={} pi={} brave={}",
        pf.node, pf.puppeteer, pf.script, pf.cdp_open, pf.chrome_running, pf.pi, pf.brave_key
    );
    if !pf.script {
        anyhow::bail!("skill script missing: {}", submit::home_dir().join(submit::SKILL_SCRIPT).display());
    }
    if !pf.node {
        anyhow::bail!("node is not on PATH");
    }
    if !dry_run {
        // Same auto-launch the web path does, so the CLI works after a reboot
        // without a separate Chrome ritual: the debug Chrome runs on a copied
        // profile and coexists with any Chrome the user has open.
        let (outcome, lines) = submit::ensure_chrome(submit::CDP_PORT);
        for l in &lines {
            println!("[chrome] {l}");
        }
        match outcome {
            submit::ChromeOutcome::AlreadyUp | submit::ChromeOutcome::Launched => {}
            submit::ChromeOutcome::Failed => {
                anyhow::bail!(
                    "could not start Chrome: {}",
                    lines.last().cloned().unwrap_or_else(|| "no output".to_string())
                );
            }
        }
    }
    let home = submit::home_dir();
    let node = submit::node_path().ok_or_else(|| anyhow::anyhow!("node is not on PATH"))?;
    let mut child = std::process::Command::new(node)
        .args(&argv)
        .env("NODE_PATH", home.join(submit::NODE_PATH_DIR))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    use std::io::Read;
    // Stream both pipes so a long upload shows progress live, and keep stderr:
    // the driver reports its failures there, and losing it hides the reason.
    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut pipe) = out_pipe {
            let _ = pipe.read_to_string(&mut buf);
        }
        print!("{buf}");
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut pipe) = err_pipe {
            let _ = pipe.read_to_string(&mut buf);
        }
        eprint!("{buf}");
        buf
    });
    let status = child.wait()?;
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    eprintln!("node exited: {:?}", status.code());
    if !status.success() {
        let reason = submit::last_line(&stderr).or_else(|| submit::last_line(&stdout));
        anyhow::bail!(
            "upload failed (exit {:?}){}",
            status.code(),
            match reason {
                Some(r) => format!(": {r}"),
                None => String::new(),
            }
        );
    }
    if !dry_run {
        let url = stdout
            .lines()
            .filter_map(submit::draft_url_from)
            .last()
            .unwrap_or_default();
        let slug = url.rsplit('/').next().unwrap_or("").to_owned();
        let rec = submit::Submitted {
            slug,
            url,
            file: submit::master_file(&rec).map(|p| p.display().to_string()).unwrap_or_default(),
            title: title.to_owned(),
            ts: chrono::Utc::now().to_rfc3339(),
        };
        let _ = submit::record_submitted(cache_dir, target, rec.clone());
        // Keep the plan file pointed at the draft. Bookkeeping only: a failure
        // here must not turn a successful upload into an error.
        match submit::record_plan_link(&object_dir, &rec.url) {
            Ok(true) => println!("plan.md updated: {}", object_dir.join("plan.md").display()),
            Ok(false) => println!("plan.md already current ({})", object_dir.display()),
            Err(e) => eprintln!("warning: could not update plan.md in {}: {e}", object_dir.display()),
        }
    }
    Ok(())
}

/// Backfill: write every recorded draft URL into its object's `plan.md`.
///
/// Objects submitted before this existed have a registry entry and no plan link,
/// so the listing shows no AstroBin chip for them.
fn link_plans(root: &std::path::Path, cache_dir: &std::path::Path) -> anyhow::Result<()> {
    let map = submit::load_submitted(cache_dir);
    if map.is_empty() {
        println!("nothing in {}", submit::submitted_path(cache_dir).display());
        return Ok(());
    }
    let (mut wrote, mut current, mut missing) = (0usize, 0usize, 0usize);
    for (key, rec) in &map {
        let Some((tel, name)) = key.split_once('/') else {
            println!("  {key}: malformed key, skipped");
            missing += 1;
            continue;
        };
        let dir = root.join(tel).join(name);
        if !dir.is_dir() {
            println!("  {key}: no such object directory {}", dir.display());
            missing += 1;
            continue;
        }
        match submit::record_plan_link(&dir, &rec.url) {
            Ok(true) => {
                println!("  {key}: {} -> {}", rec.url, dir.join("plan.md").display());
                wrote += 1;
            }
            Ok(false) => {
                println!("  {key}: already linked");
                current += 1;
            }
            Err(e) => {
                println!("  {key}: {e}");
                missing += 1;
            }
        }
    }
    println!(
        "{} entry(ies): {} updated, {} already linked, {} problem(s)",
        map.len(),
        wrote,
        current,
        missing
    );
    if missing > 0 {
        anyhow::bail!("{missing} object(s) could not be updated");
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let cfg = match config_path(cli.config.as_deref())? {
        Some(p) => {
            println!("Using config: {}", p.display());
            serde_json::from_str::<Config>(&std::fs::read_to_string(&p)?)
                .map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?
        }
        None => Config::default(),
    };

    let root = pick(cli.root, cfg.root, PathBuf::from("/data/Astro/CCD"));
    let cache_dir = pick(cli.cache, cfg.cache, default_cache_dir());
    if let Some(Command::Submit {
        target,
        title,
        llm,
        no_auto,
        no_setup,
        no_acquisition,
        dry_run,
        resume,
    }) = &cli.command
    {
        return run_submit(
            &root,
            &cache_dir,
            target,
            title.as_deref(),
            *llm,
            *no_auto,
            *no_setup,
            *no_acquisition,
            *dry_run,
            resume.as_deref(),
        );
    }
    if matches!(cli.command, Some(Command::LinkPlans)) {
        return link_plans(&root, &cache_dir);
    }
    let staging = cli.staging.or(cfg.staging);
    let page_size = pick(cli.page_size, cfg.page_size, 20);
    let compendium_dir = pick(cli.compendium_dir, cfg.compendium_dir, PathBuf::from("data/compendium"));
    // 500 ms is the default: a held arrow scrolls at two frames per second,
    // which stays under the ~270 ms cold preview render.
    let nav_repeat_ms = pick(cli.nav_repeat_ms, cfg.nav_repeat_ms, 500);
    // 5 min: a full pass over 1,283 staging files renders the ~450 missing
    // previews in ~2 minutes, so a pass is cheap and the interval is not.
    let preview_sweep_min = pick(cli.preview_sweep_min, cfg.preview_sweep_min, 5u64);
    let tel_map = cfg.staging_tel_map.unwrap_or_default();
    // The CCD root and staging are always reviewable; config/CLI add more.
    let mut review_roots: Vec<PathBuf> = cli.review_roots;
    for r in cfg.review_roots.unwrap_or_default() {
        if !review_roots.contains(&r) {
            review_roots.push(r);
        }
    }
    if !review_roots.contains(&root) {
        review_roots.push(root.clone());
    }
    if let Some(s) = &staging {
        if !review_roots.contains(s) {
            review_roots.push(s.clone());
        }
    }

    // Detect location BEFORE starting the tokio runtime (reqwest::blocking
    // panics when called from inside an async context).
    let (det_lat, det_lon) = location::detect_location();
    let latitude = cli.latitude.or(cfg.latitude).unwrap_or(det_lat);
    let longitude = cli.longitude.or(cfg.longitude).unwrap_or(det_lon);
    println!("Observer location: lat {latitude:.4}, lon {longitude:.4}");

    if let Some(s) = &staging {
        if !s.is_dir() {
            anyhow::bail!("staging directory does not exist: {}", s.display());
        }
    }
    let missing: Vec<PathBuf> = review_roots.iter().filter(|r| !r.is_dir()).cloned().collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "review root(s) do not exist: {}",
            missing.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        );
    }
    std::fs::create_dir_all(&cache_dir)?;
    println!(
        "Review roots: {} (autorepeat {} ms, preview cache: {})",
        review_roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "),
        nav_repeat_ms,
        cache_dir.display()
    );

    let state = server::AppState::new(
        root,
        page_size,
        longitude,
        latitude,
        compendium_dir,
        staging,
        cache_dir,
        review_roots,
        nav_repeat_ms,
        tel_map,
    );

    let sweep_state = state.clone();
    let app = server::build_app(state);

    let addr = format!("0.0.0.0:{}", cli.port);
    println!("Listening on http://{addr}");
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        if preview_sweep_min > 0 {
            println!(
                "Preview sweep: staging tree every {preview_sweep_min} min (first pass now)"
            );
            server::spawn_preview_sweep(sweep_state, std::time::Duration::from_secs(preview_sweep_min * 60));
        }
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok::<_, anyhow::Error>(())
    })
}

#[cfg(test)]
mod tests {
    use super::{config_path_in, Config};
    use std::fs;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("astro_cfg_{}_{}", tag, std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn config_is_discovered_next_to_the_binary_cwd() {
        let d = tmpdir("discover");
        fs::write(d.join("config.json"), "{}").unwrap();
        let found = config_path_in(&d, None).unwrap();
        assert_eq!(found, Some(d.join("config.json")));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn absent_config_yields_the_builtin_defaults() {
        let d = tmpdir("absent");
        assert_eq!(config_path_in(&d, None).unwrap(), None);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn explicit_config_is_used_verbatim() {
        let d = tmpdir("explicit");
        let p = d.join("other.json");
        fs::write(&p, "{}").unwrap();
        assert_eq!(config_path_in(&d, Some(&p)).unwrap(), Some(p));
        fs::remove_dir_all(&d).ok();
    }

    /// Autorepeat is config-driven: 500 ms is the built-in default, a config
    /// value overrides it, and a CLI flag overrides the config.
    #[test]
    fn nav_repeat_ms_is_cli_over_config_over_default() {
        use super::pick;
        assert_eq!(pick(None, None, 500u64), 500);
        assert_eq!(pick(None, Some(120), 500), 120);
        assert_eq!(pick(Some(800), Some(120), 500), 800);
    }

    /// The sweep interval follows the same precedence as every other knob, and
    /// `0` disables the sweep.
    #[test]
    fn preview_sweep_interval_is_cli_over_config_over_default() {
        use super::pick;
        assert_eq!(pick(None, None, 5u64), 5);
        assert_eq!(pick(None, Some(10), 5), 10);
        assert_eq!(pick(Some(0), Some(10), 5), 0);
    }

    #[test]
    fn a_typo_in_explicit_config_is_a_hard_error_not_a_silent_default() {
        let d = tmpdir("typo");
        let missing = d.join("nope.json");
        let e = config_path_in(&d, Some(&missing)).unwrap_err().to_string();
        assert!(e.contains("does not exist"), "{e}");
        fs::remove_dir_all(&d).ok();
    }

    /// The staging->inventory telescope mapping parses out of the config file;
    /// absent it, the map is empty and hints fall back to name equality.
    #[test]
    fn staging_tel_map_is_parsed_from_config() {
        let cfg: Config =
            serde_json::from_str(r#"{"staging_tel_map": {"Pier": "10RC", "RC": "8RC"}}"#)
                .unwrap();
        let m = cfg.staging_tel_map.unwrap();
        assert_eq!(m.get("Pier").map(String::as_str), Some("10RC"));
        assert_eq!(m.get("RC").map(String::as_str), Some("8RC"));
        let empty: Config = serde_json::from_str("{}").unwrap();
        assert!(empty.staging_tel_map.unwrap_or_default().is_empty());
    }
}
