#[cfg(feature = "web")]
pub mod api;
pub mod app;
pub mod browser;
pub mod config;
pub mod db;
pub mod models;
pub mod network;
pub mod rrc;
pub mod service;

use std::sync::Arc;

use anyhow::Context;

use crate::app::AppState;
use crate::config::AppConfig;

/// Owns the shared application state and its background services.
///
/// Frontends are responsible for driving their own event loops and can use
/// [`state`](Self::state) to construct an adapter over the common core.
pub struct Runtime {
    state: Arc<AppState>,
    network_task: Option<tokio::task::JoinHandle<()>>,
}

impl Runtime {
    pub fn start(config: AppConfig) -> anyhow::Result<Self> {
        let database = db::Database::open(&config.database_path)
            .context("could not open application database")?;
        config::restrict_file_permissions(&config.database_path)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .min(i64::MAX as u64) as i64;
        database
            .maintain(now)
            .context("could not maintain application database")?;
        let state = Arc::new(AppState::new(config, database));
        let network_task = network::spawn(state.clone());
        Ok(Self {
            state,
            network_task,
        })
    }

    pub fn state(&self) -> Arc<AppState> {
        self.state.clone()
    }

    /// Returns the frontend-neutral application API.
    pub fn service(&self) -> service::AppService {
        service::AppService::new(self.state.clone())
    }

    pub async fn shutdown(self) {
        self.state.shutdown.trigger();
        if let Some(mut task) = self.network_task {
            if tokio::time::timeout(std::time::Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                tracing::warn!("network task did not stop in time; aborting it");
                task.abort();
                let _ = task.await;
            }
        }
    }
}
