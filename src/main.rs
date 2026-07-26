mod api;
mod app;
mod browser;
mod config;
mod db;
mod models;
mod network;
mod rrc;

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use config::Cli;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("rs_nomadnet=info,tower_http=info")),
        )
        .init();

    let cli = Cli::parse();
    let config = config::AppConfig::from_cli(cli)?;
    let database =
        db::Database::open(&config.database_path).context("could not open application database")?;
    config::restrict_file_permissions(&config.database_path)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64;
    database
        .maintain(now)
        .context("could not maintain application database")?;
    let state = Arc::new(app::AppState::new(config.clone(), database));

    let network_task = network::spawn(state.clone());
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("could not bind web interface to {}", config.listen))?;
    tracing::info!(address = %config.listen, "rsNomadNet web interface ready");

    let server =
        axum::serve(listener, api::router(state.clone())).with_graceful_shutdown(shutdown_signal());
    let result = server.await.context("web server failed");

    state.shutdown.trigger();
    if let Some(task) = network_task {
        let _ = task.await;
    }
    result
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
