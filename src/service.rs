use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{broadcast, oneshot};

use crate::app::AppState;
use crate::browser::{BrowserPage, NomadUrl};
use crate::models::{
    ConversationSummary, DirectoryEntry, MessageView, NetworkSnapshot, NetworkState, ServerEvent,
};
use crate::network::NetworkCommand;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    TooLarge(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Remote(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Clone, Serialize)]
pub struct IdentitySettings {
    pub destination_hash: Option<String>,
    pub name: String,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct IdentityUpdate {
    pub name: String,
    pub announced: bool,
}

#[derive(Debug, Clone)]
pub struct SendMessage {
    pub destination_hash: String,
    pub title: String,
    pub content: String,
    pub delivery_method: String,
    pub propagation_node: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FetchPage {
    pub url: String,
    pub reload: bool,
    pub fields: BTreeMap<String, String>,
}

/// UI-independent facade over the rsNomadNet application core.
///
/// HTTP, terminal and future frontends should call this type instead of
/// reaching into the database or runtime command channels directly.
#[derive(Clone)]
pub struct AppService {
    state: Arc<AppState>,
}

impl AppService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    pub async fn network_snapshot(&self) -> NetworkSnapshot {
        self.state.network.read().await.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.state.events.subscribe()
    }

    pub async fn identity_settings(&self) -> AppResult<IdentitySettings> {
        let network = self.state.network.read().await;
        let name = self
            .state
            .database
            .setting("announce_name")?
            .unwrap_or_else(|| "rsNomadNet".into());
        Ok(IdentitySettings {
            destination_hash: network.destination_hash.clone(),
            name,
            online: matches!(network.state, NetworkState::Online),
        })
    }

    pub async fn update_identity(
        &self,
        name: String,
        announce_now: bool,
    ) -> AppResult<IdentityUpdate> {
        let name = name
            .chars()
            .filter(|character| !character.is_control())
            .take(128)
            .collect::<String>()
            .trim()
            .to_string();
        self.state.database.set_setting("announce_name", &name)?;
        let online = matches!(self.state.network.read().await.state, NetworkState::Online);
        if !online {
            if announce_now {
                return Err(AppError::Unavailable("Reticulum is not online".into()));
            }
            return Ok(IdentityUpdate {
                name,
                announced: false,
            });
        }
        let (response, receiver) = oneshot::channel();
        self.state
            .network_commands
            .send(NetworkCommand::SetAnnounceName {
                name: (!name.is_empty()).then_some(name.clone()),
                announce_now,
                response,
            })
            .await
            .map_err(|_| AppError::Unavailable("network service is unavailable".into()))?;
        receiver
            .await
            .map_err(|_| AppError::Unavailable("network service stopped".into()))?
            .map_err(AppError::Remote)?;
        Ok(IdentityUpdate {
            name,
            announced: announce_now,
        })
    }

    pub fn conversations(&self) -> AppResult<Vec<ConversationSummary>> {
        Ok(self.state.database.conversations()?)
    }

    pub fn directory(&self) -> AppResult<Vec<DirectoryEntry>> {
        Ok(self.state.database.directory()?)
    }

    pub fn messages(
        &self,
        destination_hash: &str,
        query: Option<&str>,
    ) -> AppResult<Vec<MessageView>> {
        let destination_hash = canonical_hash(destination_hash, "destination hash")?;
        let query = query.map(str::trim).filter(|value| !value.is_empty());
        Ok(match query {
            Some(query) => self
                .state
                .database
                .search_messages(&destination_hash, query)?,
            None => self.state.database.messages(&destination_hash)?,
        })
    }

    pub fn mark_conversation_read(&self, destination_hash: &str) -> AppResult<()> {
        let destination_hash = canonical_hash(destination_hash, "destination hash")?;
        Ok(self
            .state
            .database
            .mark_conversation_read(&destination_hash)?)
    }

    pub fn clear_conversation(&self, destination_hash: &str) -> AppResult<usize> {
        let destination_hash = canonical_hash(destination_hash, "destination hash")?;
        if self
            .state
            .database
            .conversation_has_pending(&destination_hash)?
        {
            return Err(AppError::Conflict(
                "conversation has messages awaiting delivery".into(),
            ));
        }
        Ok(self.state.database.clear_conversation(&destination_hash)?)
    }

    pub fn draft(&self, scope: &str, target: &str) -> AppResult<String> {
        validate_draft_target(scope, target)?;
        Ok(self
            .state
            .database
            .draft(scope, target)?
            .unwrap_or_default())
    }

    pub fn save_draft(&self, scope: &str, target: &str, content: &str) -> AppResult<()> {
        validate_draft_target(scope, target)?;
        if content.len() > 1024 * 1024 {
            return Err(AppError::TooLarge(
                "draft exceeds local safety limit".into(),
            ));
        }
        Ok(self
            .state
            .database
            .save_draft(scope, target, content, unix_seconds())?)
    }

    pub async fn send_message(&self, request: SendMessage) -> AppResult<MessageView> {
        let destination_hash = parse_hash(&request.destination_hash, "destination hash")?;
        if request.content.trim().is_empty() {
            return Err(AppError::Invalid("message content cannot be empty".into()));
        }
        if request.title.len() > 1024 || request.content.len() > 1024 * 1024 {
            return Err(AppError::TooLarge(
                "message exceeds local safety limits".into(),
            ));
        }
        let delivery_method = request.delivery_method.trim().to_ascii_lowercase();
        if !matches!(
            delivery_method.as_str(),
            "" | "auto" | "automatic" | "opportunistic" | "direct" | "propagated"
        ) {
            return Err(AppError::Invalid("unknown LXMF delivery method".into()));
        }
        if !matches!(self.state.network.read().await.state, NetworkState::Online) {
            return Err(AppError::Unavailable("Reticulum is not online".into()));
        }
        let propagation_node = request
            .propagation_node
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| parse_hash(value, "propagation node hash"))
            .transpose()?;
        if propagation_node.is_some() && delivery_method != "propagated" {
            return Err(AppError::Invalid(
                "propagation_node is only valid for propagated delivery".into(),
            ));
        }
        let (response, receiver) = oneshot::channel();
        self.state
            .network_commands
            .send(NetworkCommand::SendMessage {
                destination_hash,
                title: request.title,
                content: request.content,
                delivery_method,
                propagation_node,
                response,
            })
            .await
            .map_err(|_| AppError::Unavailable("network service is unavailable".into()))?;
        receiver
            .await
            .map_err(|_| AppError::Unavailable("network service stopped".into()))?
            .map_err(AppError::Remote)
    }

    pub async fn fetch_page(&self, request: FetchPage) -> AppResult<BrowserPage> {
        let url =
            NomadUrl::parse(&request.url).map_err(|error| AppError::Invalid(error.to_string()))?;
        if !url.is_page() {
            return Err(AppError::Invalid("page fetch requires a /page/ URL".into()));
        }
        if !matches!(self.state.network.read().await.state, NetworkState::Online) {
            return Err(AppError::Unavailable("Reticulum is not online".into()));
        }
        let (response, receiver) = oneshot::channel();
        self.state
            .network_commands
            .send(NetworkCommand::FetchPage {
                url,
                reload: request.reload,
                fields: request.fields,
                response,
            })
            .await
            .map_err(|_| AppError::Unavailable("network service is unavailable".into()))?;
        receiver
            .await
            .map_err(|_| AppError::Unavailable("network service stopped".into()))?
            .map_err(AppError::Remote)
    }
}

fn validate_draft_target(scope: &str, target: &str) -> AppResult<()> {
    if matches!(scope, "lxmf" | "rrc")
        && !target.is_empty()
        && target.len() <= 256
        && !target.chars().any(char::is_control)
    {
        Ok(())
    } else {
        Err(AppError::Invalid("invalid draft target".into()))
    }
}

fn canonical_hash(value: &str, field: &str) -> AppResult<String> {
    parse_hash(value, field)?;
    Ok(value.to_ascii_lowercase())
}

fn parse_hash(value: &str, field: &str) -> AppResult<[u8; 16]> {
    let mut output = [0_u8; 16];
    hex::decode_to_slice(value, &mut output).map_err(|_| {
        AppError::Invalid(format!("{field} must contain 32 hexadecimal characters"))
    })?;
    Ok(output)
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_validated_and_canonicalised() {
        assert_eq!(
            canonical_hash("AABBCCDDEEFF00112233445566778899", "hash").unwrap(),
            "aabbccddeeff00112233445566778899"
        );
        assert!(canonical_hash("not-a-hash", "hash").is_err());
    }

    #[test]
    fn draft_targets_are_bounded() {
        assert!(validate_draft_target("lxmf", "peer").is_ok());
        assert!(validate_draft_target("unknown", "peer").is_err());
        assert!(validate_draft_target("rrc", "bad\nroom").is_err());
    }
}
