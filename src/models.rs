use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkState {
    Offline,
    Starting,
    Online,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkSnapshot {
    pub state: NetworkState,
    pub detail: String,
    pub destination_hash: Option<String>,
    pub interfaces: Vec<InterfaceSnapshot>,
}

impl NetworkSnapshot {
    pub fn offline() -> Self {
        Self {
            state: NetworkState::Offline,
            detail: "Offline mode".into(),
            destination_hash: None,
            interfaces: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InterfaceSnapshot {
    pub id: u64,
    pub name: String,
    pub online: bool,
    pub mode: String,
    pub role: String,
    pub bitrate: u64,
    pub mtu: u32,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_rate: u64,
    pub tx_rate: u64,
    pub held_announces: u64,
    pub tx_drops: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationSummary {
    pub destination_hash: String,
    pub display_name: Option<String>,
    pub last_message: Option<String>,
    pub last_activity: Option<i64>,
    pub unread: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageView {
    pub id: i64,
    pub destination_hash: String,
    pub source_hash: String,
    pub title: String,
    pub content: String,
    pub timestamp: i64,
    pub outbound: bool,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectoryEntry {
    pub destination_hash: String,
    pub identity_hash: Option<String>,
    pub delivery_hash: Option<String>,
    pub kind: String,
    pub display_name: Option<String>,
    pub hops: u8,
    pub last_seen: i64,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RrcHubView {
    pub destination_hash: String,
    pub local_identity: String,
    pub name: Option<String>,
    pub nick: Option<String>,
    pub version: Option<String>,
    pub supports_resources: bool,
    pub supports_actions: bool,
    pub supports_direct_notices: bool,
    pub max_message_bytes: Option<usize>,
    pub connected: bool,
    pub rooms: Vec<String>,
    pub room_states: Vec<RrcRoomStateView>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RrcRoomStateView {
    pub name: String,
    pub registered: bool,
    pub modes: String,
    pub topic: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RrcMessageView {
    pub hub_hash: String,
    pub room: Option<String>,
    pub source_hash: String,
    pub nick: Option<String>,
    pub body: String,
    pub timestamp_ms: u64,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RrcRoomView {
    pub name: String,
    pub topic: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RrcUserView {
    pub nick: Option<String>,
    pub identity: String,
    pub operator: bool,
    pub voiced: bool,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    pub destination_hash: String,
    #[serde(default)]
    pub title: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ServerEvent {
    Snapshot(NetworkSnapshot),
    NetworkChanged(NetworkSnapshot),
    MessageStored(MessageView),
    DirectoryChanged(DirectoryEntry),
    RrcHubChanged(RrcHubView),
    RrcMessage(RrcMessageView),
}
