mod traverse;
mod transit;
mod location;
mod plan;
mod server;
mod render;
mod staging;

use std::path::PathBuf;

use clap::Parser;
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

    /// Staging directory reviewed at /staging (enables the feature)
    #[arg(long)]
    staging: Option<std::path::PathBuf>,

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
    let staging = cli.staging.or(cfg.staging);
    let cache_dir = pick(cli.cache, cfg.cache, default_cache_dir());
    let page_size = pick(cli.page_size, cfg.page_size, 20);
    let compendium_dir = pick(cli.compendium_dir, cfg.compendium_dir, PathBuf::from("data/compendium"));

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
        std::fs::create_dir_all(&cache_dir)?;
        println!("Staging review enabled: {} (preview cache: {})", s.display(), cache_dir.display());
    }

    let state = server::AppState::new(
        root,
        page_size,
        longitude,
        latitude,
        compendium_dir,
        staging,
        cache_dir,
    );

    let app = server::build_app(state);

    let addr = format!("0.0.0.0:{}", cli.port);
    println!("Listening on http://{addr}");
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok::<_, anyhow::Error>(())
    })
}

#[cfg(test)]
mod tests {
    use super::config_path_in;
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

    #[test]
    fn a_typo_in_explicit_config_is_a_hard_error_not_a_silent_default() {
        let d = tmpdir("typo");
        let missing = d.join("nope.json");
        let e = config_path_in(&d, Some(&missing)).unwrap_err().to_string();
        assert!(e.contains("does not exist"), "{e}");
        fs::remove_dir_all(&d).ok();
    }
}
