mod app;
mod codex;
mod codex_history;
mod commands;
mod config;
mod limits;
mod models;
mod render;
mod store;
mod telegram;
mod transcribe;

use anyhow::Result;
use tokio::time::{Duration, sleep};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("telecodex=info,reqwest=warn")),
        )
        .with_target(false)
        .compact()
        .init();

    if let Some(delay_ms) = restart_delay_ms_from_env() {
        sleep(Duration::from_millis(delay_ms)).await;
    }

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "telecodex.toml".to_string());
    let config = config::Config::load(config_path.into())?;
    let app = app::App::bootstrap(config).await?;
    app.run().await
}

fn restart_delay_ms_from_env() -> Option<u64> {
    std::env::var("TELECODEX_RESTART_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
}
