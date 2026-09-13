mod traverse;
mod transit;
mod location;
mod plan;
mod server;
mod render;

use clap::Parser;

#[derive(Parser)]
#[command(name = "astro_inventory", about = "Astronomy image inventory web server")]
struct Cli {
    /// Root directory of CCD data
    #[arg(long)]
    root: std::path::PathBuf,

    /// Rows per page; 0 disables paging
    #[arg(long, default_value_t = 20)]
    page_size: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Detect longitude (may touch network once; falls back to 0.0 on failure)
    let longitude = location::detect_longitude();

    let state = server::AppState::new(cli.root, cli.page_size, longitude);

    let app = server::build_app(state);

    let addr = "0.0.0.0:5000";
    println!("Listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
