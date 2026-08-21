use anyhow::Context;
use clap::Parser;
use rsnomadnet_core::config::{AppConfig, Cli};
use rsnomadnet_core::{Runtime, api};
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
    let config = AppConfig::from_cli(cli)?;
    let runtime = Runtime::start(config.clone())?;
    let state = runtime.state();

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("could not bind web interface to {}", config.listen))?;
    tracing::info!(address = %config.listen, "rsNomadNet web interface ready");

    let server =
        axum::serve(listener, api::router(state)).with_graceful_shutdown(shutdown_signal());
    let result = server.await.context("web server failed");

    runtime.shutdown().await;
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
