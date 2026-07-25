use std::sync::Arc;

use tokio::sync::{Mutex, RwLock, broadcast, mpsc};

use crate::config::AppConfig;
use crate::db::Database;
use crate::models::{NetworkSnapshot, ServerEvent};
use crate::network::NetworkCommand;
use crate::rrc::RrcCommand;
use rns_runtime::lifecycle::ShutdownSignal;

pub struct AppState {
    pub config: AppConfig,
    pub database: Database,
    pub network: RwLock<NetworkSnapshot>,
    pub events: broadcast::Sender<ServerEvent>,
    pub network_commands: mpsc::Sender<NetworkCommand>,
    pub network_command_rx: Mutex<Option<mpsc::Receiver<NetworkCommand>>>,
    pub rrc_commands: mpsc::Sender<RrcCommand>,
    pub rrc_command_rx: Mutex<Option<mpsc::Receiver<RrcCommand>>>,
    pub shutdown: ShutdownSignal,
}

impl AppState {
    pub fn new(config: AppConfig, database: Database) -> Self {
        let (events, _) = broadcast::channel(256);
        let (network_commands, network_command_rx) = mpsc::channel(64);
        let (rrc_commands, rrc_command_rx) = mpsc::channel(64);
        Self {
            config,
            database,
            network: RwLock::new(NetworkSnapshot::offline()),
            events,
            network_commands,
            network_command_rx: Mutex::new(Some(network_command_rx)),
            rrc_commands,
            rrc_command_rx: Mutex::new(Some(rrc_command_rx)),
            shutdown: ShutdownSignal::new(),
        }
    }

    pub async fn set_network(self: &Arc<Self>, snapshot: NetworkSnapshot) {
        *self.network.write().await = snapshot.clone();
        let _ = self.events.send(ServerEvent::NetworkChanged(snapshot));
    }
}
