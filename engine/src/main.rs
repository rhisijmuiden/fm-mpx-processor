//! PILOT engine binary.
//!
//! `pilot serve`  – run the engine and the management API (default port 8080).
//! `pilot render` – process an audio file offline into a 192 kHz MPX WAV (for testing).

use clap::{Parser, Subcommand};
use parking_lot::Mutex;
use pilot::api::{router, AppState};
use pilot::config::Config;
use pilot::db::Db;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "pilot", version, about = "PILOT FM MPX / RDS audio processor")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the engine and the management API.
    Serve {
        #[arg(long, env = "PILOT_LISTEN", default_value = "0.0.0.0:8080")]
        listen: String,
        #[arg(long, env = "PILOT_DB", default_value = "/var/lib/pilot/pilot.db")]
        db: PathBuf,
        /// Release channel shown in /api/info (production or test).
        #[arg(long, env = "PILOT_CHANNEL", default_value = "test")]
        channel: String,
    },
    /// Process an audio file (any format ffmpeg reads) into an MPX WAV file.
    Render {
        input: PathBuf,
        output: PathBuf,
        /// Optional configuration JSON (defaults otherwise).
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, default_value_t = 30.0)]
        seconds: f32,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Serve {
        listen: "0.0.0.0:8080".into(),
        db: "/var/lib/pilot/pilot.db".into(),
        channel: "test".into(),
    }) {
        Cmd::Serve { listen, db, channel } => serve(listen, db, channel),
        Cmd::Render { input, output, config, seconds } => pilot::render::render(&input, &output, config.as_deref(), seconds),
    }
}

fn serve(listen: String, db_path: PathBuf, channel: String) -> anyhow::Result<()> {
    let db = Arc::new(Db::open(&db_path)?);
    db.seed_presets()?;
    let cfg = db.load_config()?;
    db.save_config(&cfg)?;
    db.log("engine.start", &format!("v{}", pilot::api::version()));
    tracing::info!(version = pilot::api::version(), %listen, db = %db_path.display(), "starting PILOT engine");
    let engine = pilot::engine::start(cfg.clone());
    let token = std::env::var("PILOT_API_TOKEN").ok().filter(|t| !t.is_empty());
    let state = AppState { db, engine, cfg: Arc::new(Mutex::new(cfg)), token, channel };
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    rt.block_on(async move {
        let app = router(state).layer(tower_http::cors::CorsLayer::permissive());
        let listener = tokio::net::TcpListener::bind(&listen).await?;
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        anyhow::Ok(())
    })?;
    let _ = Config::default();
    Ok(())
}
