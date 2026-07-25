use rns_identity::identity::Identity;
use rns_runtime::reticulum::ReticulumHandle;
use rs_rrc_client::{Event, Hub, Message, MessageKind, RrcClient};
use tokio::sync::oneshot;

use crate::app::AppState;
use crate::db::Database;
use crate::models::{
    RrcHubView, RrcMessageView, RrcRoomStateView, RrcRoomView, RrcUserView, ServerEvent,
};

pub enum RrcCommand {
    Connect {
        destination_hash: [u8; 16],
        nick: Option<String>,
        response: oneshot::Sender<Result<RrcHubView, String>>,
    },
    Join {
        destination_hash: [u8; 16],
        room: String,
        key: Option<String>,
        response: oneshot::Sender<Result<(), String>>,
    },
    Part {
        destination_hash: [u8; 16],
        room: String,
        response: oneshot::Sender<Result<(), String>>,
    },
    Disconnect {
        destination_hash: [u8; 16],
        response: oneshot::Sender<Result<(), String>>,
    },
    SetNick {
        destination_hash: [u8; 16],
        nick: String,
        response: oneshot::Sender<Result<RrcHubView, String>>,
    },
    ListRooms {
        destination_hash: [u8; 16],
        response: oneshot::Sender<Result<Vec<RrcRoomView>, String>>,
    },
    ListUsers {
        destination_hash: [u8; 16],
        room: String,
        response: oneshot::Sender<Result<Vec<RrcUserView>, String>>,
    },
    Ping {
        destination_hash: [u8; 16],
        response: oneshot::Sender<Result<u64, String>>,
    },
    Send {
        destination_hash: [u8; 16],
        room: Option<String>,
        body: String,
        action: bool,
        response: oneshot::Sender<Result<(), String>>,
    },
}

pub fn spawn(
    state: std::sync::Arc<AppState>,
    runtime: ReticulumHandle,
    private_key: [u8; 64],
    source: [u8; 16],
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let identity = match Identity::from_private_key(&private_key) {
            Ok(identity) => identity,
            Err(error) => {
                tracing::error!(%error, "could not initialize RRC identity");
                return;
            }
        };
        let client = RrcClient::new(runtime, identity);
        let mut events = client.subscribe();
        let Some(mut commands) = state.rrc_command_rx.lock().await.take() else {
            return;
        };
        restore_saved_sessions(&state, &client);
        loop {
            tokio::select! {
                _ = state.shutdown.wait() => {
                    let _ = client.shutdown().await;
                    break;
                }
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    handle_command(&client, &state.database, source, command).await;
                }
                event = events.recv() => {
                    match event {
                        Ok(event) => forward_event(&state, source, event),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "RRC event adapter lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    })
}

async fn handle_command(
    client: &RrcClient,
    database: &Database,
    source: [u8; 16],
    command: RrcCommand,
) {
    match command {
        RrcCommand::Connect {
            destination_hash,
            nick,
            response,
        } => {
            let result = match client.connect(destination_hash, nick.as_deref()).await {
                Ok(hub) => {
                    let view = hub_view(hub, source);
                    if let Err(error) = database.save_rrc_hub(
                        &view.destination_hash,
                        view.name.as_deref(),
                        nick.as_deref(),
                        unix_seconds(),
                    ) {
                        tracing::warn!(%error, "could not save RRC hub");
                    }
                    Ok(view)
                }
                Err(error) => Err(error.to_string()),
            };
            let _ = response.send(result);
        }
        RrcCommand::Join {
            destination_hash,
            room,
            key,
            response,
        } => {
            let result = client
                .join(destination_hash, &room, key.as_deref())
                .await
                .map_err(|error| error.to_string());
            if result.is_ok()
                && let Err(error) = database.save_rrc_room(
                    &hex::encode(destination_hash),
                    &normalize_room(&room),
                    None,
                )
            {
                tracing::warn!(%error, "could not save RRC room");
            }
            let _ = response.send(result);
        }
        RrcCommand::Send {
            destination_hash,
            room,
            body,
            action,
            response,
        } => {
            let result = if body.starts_with('/') && !action {
                client
                    .send_command(destination_hash, room.as_deref(), &body)
                    .await
            } else if action {
                match room.as_deref() {
                    Some(room) => client.send_action(destination_hash, room, &body).await,
                    None => Err(rs_rrc_client::Error::InvalidRoom),
                }
            } else {
                match room.as_deref() {
                    Some(room) => client.send_message(destination_hash, room, &body).await,
                    None => Err(rs_rrc_client::Error::InvalidRoom),
                }
            }
            .map_err(|error| error.to_string());
            let _ = response.send(result);
        }
        RrcCommand::Part {
            destination_hash,
            room,
            response,
        } => {
            let result = client
                .part(destination_hash, &room)
                .await
                .map_err(|error| error.to_string());
            if result.is_ok()
                && let Err(error) =
                    database.remove_rrc_room(&hex::encode(destination_hash), &normalize_room(&room))
            {
                tracing::warn!(%error, "could not remove saved RRC room");
            }
            let _ = response.send(result);
        }
        RrcCommand::Disconnect {
            destination_hash,
            response,
        } => {
            let result = client
                .disconnect(destination_hash)
                .await
                .map_err(|error| error.to_string());
            if result.is_ok()
                && let Err(error) = database.remove_rrc_hub(&hex::encode(destination_hash))
            {
                tracing::warn!(%error, "could not remove saved RRC hub");
            }
            let _ = response.send(result);
        }
        RrcCommand::SetNick {
            destination_hash,
            nick,
            response,
        } => {
            let result = client
                .set_nick(destination_hash, &nick)
                .await
                .map(|hub| hub_view(hub, source))
                .map_err(|error| error.to_string());
            if let Ok(view) = &result
                && let Err(error) = database.save_rrc_hub(
                    &view.destination_hash,
                    view.name.as_deref(),
                    view.nick.as_deref(),
                    unix_seconds(),
                )
            {
                tracing::warn!(%error, "could not persist RRC nick");
            }
            let _ = response.send(result);
        }
        RrcCommand::ListRooms {
            destination_hash,
            response,
        } => {
            let result = client
                .list_rooms(destination_hash, std::time::Duration::from_secs(30))
                .await
                .map(|rooms| {
                    rooms
                        .into_iter()
                        .map(|room| RrcRoomView {
                            name: room.name,
                            topic: room.topic,
                        })
                        .collect()
                })
                .map_err(|error| error.to_string());
            let _ = response.send(result);
        }
        RrcCommand::ListUsers {
            destination_hash,
            room,
            response,
        } => {
            let result = client
                .list_users(destination_hash, &room, std::time::Duration::from_secs(30))
                .await
                .map(|users| {
                    users
                        .into_iter()
                        .map(|user| RrcUserView {
                            nick: user.nick,
                            identity: user.identity,
                            operator: user.operator,
                            voiced: user.voiced,
                        })
                        .collect()
                })
                .map_err(|error| error.to_string());
            let _ = response.send(result);
        }
        RrcCommand::Ping {
            destination_hash,
            response,
        } => {
            let result = client
                .ping(destination_hash, std::time::Duration::from_secs(30))
                .await
                .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
                .map_err(|error| error.to_string());
            let _ = response.send(result);
        }
    }
}

fn forward_event(state: &std::sync::Arc<AppState>, source: [u8; 16], event: Event) {
    let event = match event {
        Event::HubChanged(hub) => {
            let view = hub_view(hub, source);
            if !view.connected
                && matches!(
                    state.database.is_rrc_hub_saved(&view.destination_hash),
                    Ok(false)
                )
            {
                None
            } else {
                Some(ServerEvent::RrcHubChanged(view))
            }
        }
        Event::Message(message) => {
            let message = message_view(message);
            if let Err(error) = state.database.store_rrc_message(&message) {
                tracing::warn!(%error, "could not store RRC message");
            }
            Some(ServerEvent::RrcMessage(message))
        }
        Event::Envelope { .. } => None,
        Event::Resource(_) => None,
        Event::InvalidEnvelope { hub, error } => {
            tracing::warn!(hub = %hex::encode(hub), %error, "invalid RRC envelope");
            None
        }
    };
    if let Some(event) = event {
        let _ = state.events.send(event);
    }
}

fn hub_view(hub: Hub, source: [u8; 16]) -> RrcHubView {
    let welcome = hub.welcome.as_ref();
    RrcHubView {
        destination_hash: hex::encode(hub.destination_hash),
        local_identity: hex::encode(source),
        name: hub.name,
        nick: hub.nick,
        version: welcome.and_then(|value| value.version.clone()),
        supports_resources: welcome.is_some_and(|value| value.capabilities.resource_envelope),
        supports_actions: welcome.is_some_and(|value| value.capabilities.action),
        supports_direct_notices: welcome.is_some_and(|value| value.capabilities.direct_notice),
        max_message_bytes: welcome.and_then(|value| value.limits.max_message_bytes),
        connected: hub.connected && hub.welcome.is_some(),
        rooms: hub.rooms,
        room_states: hub
            .room_states
            .into_iter()
            .map(|(name, state)| RrcRoomStateView {
                name,
                registered: state.registered,
                modes: state.modes,
                topic: state.topic,
            })
            .collect(),
        detail: hub.detail,
    }
}

fn message_view(message: Message) -> RrcMessageView {
    RrcMessageView {
        hub_hash: hex::encode(message.hub),
        room: message.room,
        source_hash: message.source.map(hex::encode).unwrap_or_default(),
        nick: message.nick,
        body: message.body,
        timestamp_ms: message.timestamp_ms,
        kind: match message.kind {
            MessageKind::Message => "message",
            MessageKind::Notice => "notice",
            MessageKind::Action => "action",
            MessageKind::Error => "error",
        }
        .into(),
    }
}

fn restore_saved_sessions(state: &std::sync::Arc<AppState>, client: &RrcClient) {
    let saved = match state.database.saved_rrc_hubs() {
        Ok(saved) => saved,
        Err(error) => {
            tracing::warn!(%error, "could not load saved RRC hubs");
            return;
        }
    };
    for hub in saved {
        let Ok(bytes) = hex::decode(&hub.destination_hash) else {
            tracing::warn!(hub = hub.destination_hash, "ignoring invalid saved RRC hub");
            continue;
        };
        let Ok(destination) = <[u8; 16]>::try_from(bytes.as_slice()) else {
            tracing::warn!(hub = hub.destination_hash, "ignoring invalid saved RRC hub");
            continue;
        };
        let client = client.clone();
        tokio::spawn(async move {
            match client.connect(destination, hub.nick.as_deref()).await {
                Ok(_) => {
                    if let Err(error) = client
                        .wait_until_connected(destination, std::time::Duration::from_secs(30))
                        .await
                    {
                        tracing::warn!(
                            hub = %hex::encode(destination),
                            %error,
                            "RRC hub did not become ready"
                        );
                        return;
                    }
                    for (room, key) in hub.rooms {
                        if let Err(error) = client.join(destination, &room, key.as_deref()).await {
                            tracing::warn!(
                                hub = %hex::encode(destination),
                                %room,
                                %error,
                                "could not restore RRC room"
                            );
                        }
                    }
                }
                Err(error) => tracing::warn!(
                    hub = %hex::encode(destination),
                    %error,
                    "could not restore RRC hub"
                ),
            }
        });
    }
}

fn normalize_room(room: &str) -> String {
    room.trim().trim_start_matches('#').to_ascii_lowercase()
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}
