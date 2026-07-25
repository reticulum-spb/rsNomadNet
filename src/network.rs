use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use bytes::Bytes;
use lxmf_core::application::DeliveryIdentity;
use lxmf_core::constants::{DeliveryMethod, UnverifiedReason};
use lxmf_core::message::LxMessage;
use rns_crypto::ed25519::Ed25519PublicKey;
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use rns_runtime::application::announce_stream;
use rns_runtime::link_client::LinkSession;
use rns_runtime::link_manager::LinkManager;
use rns_runtime::reticulum;
use rns_transport::messages::{
    OutboundRequest, TransportMessage, TransportQuery, TransportQueryResponse,
};
use tokio::sync::oneshot;

use crate::app::AppState;
use crate::browser::{BrowserPage, DownloadedFile, NomadUrl, parse_page};
use crate::db::NewMessage;
use crate::models::{
    DirectoryEntry, InterfaceSnapshot, MessageView, NetworkSnapshot, NetworkState, ServerEvent,
};

pub enum NetworkCommand {
    SendMessage {
        destination_hash: [u8; 16],
        title: String,
        content: String,
        response: oneshot::Sender<Result<MessageView, String>>,
    },
    FetchPage {
        url: NomadUrl,
        reload: bool,
        fields: BTreeMap<String, String>,
        response: oneshot::Sender<Result<BrowserPage, String>>,
    },
    FetchFile {
        url: NomadUrl,
        response: oneshot::Sender<Result<DownloadedFile, String>>,
    },
}

pub fn spawn(state: Arc<AppState>) -> Option<tokio::task::JoinHandle<()>> {
    if state.config.offline {
        return None;
    }
    Some(tokio::spawn(async move {
        if let Err(error) = run(state.clone()).await {
            tracing::error!(%error, "Reticulum service stopped");
            state
                .set_network(NetworkSnapshot {
                    state: NetworkState::Failed,
                    detail: error.to_string(),
                    destination_hash: None,
                    interfaces: Vec::new(),
                })
                .await;
        }
    }))
}

async fn run(state: Arc<AppState>) -> anyhow::Result<()> {
    state
        .set_network(NetworkSnapshot {
            state: NetworkState::Starting,
            detail: "Starting Reticulum".into(),
            destination_hash: None,
            interfaces: Vec::new(),
        })
        .await;

    let identity = load_or_create_identity(&state.config.identity_path)?;
    let mut delivery = DeliveryIdentity::new(identity, Some("rsNomadNet".into()), None)?;
    let destination_hash = hex::encode(delivery.destination_hash());
    let config = state
        .config
        .rns_config
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    let runtime = reticulum::init(
        config.as_deref(),
        None,
        state.shutdown.clone(),
        Arc::new(AtomicBool::new(true)),
    )
    .await?;

    let (delivery_tx, delivery_rx) = tokio::sync::mpsc::channel(256);
    let (packet_tx, mut packet_rx) = tokio::sync::mpsc::channel(256);
    let (resource_tx, mut resource_rx) = tokio::sync::mpsc::channel(64);
    runtime
        .transport_tx
        .send(TransportMessage::RegisterDestination {
            hash: delivery.destination_hash(),
            app_name: lxmf_core::application::DELIVERY_APP_NAME.to_string(),
            delivery_tx: Some(delivery_tx),
        })
        .await?;

    let mut link_manager = LinkManager::with_destination(
        runtime.transport_tx.clone(),
        delivery_rx,
        delivery.identity(),
        lxmf_core::application::DELIVERY_APP_NAME,
        delivery.identity().get_signing_key(),
    );
    link_manager.set_link_packet_channel(packet_tx);
    link_manager.set_resource_completed_channel(resource_tx);
    let link_manager_task = tokio::spawn(link_manager.run());
    announce(&runtime, &mut delivery).await?;
    let mut delivery_announces = announce_stream(&runtime, Some("lxmf.delivery")).await?;
    let mut node_announces = announce_stream(&runtime, Some("nomadnetwork.node")).await?;
    let mut propagation_announces = announce_stream(&runtime, Some("lxmf.propagation")).await?;
    seed_cached_announces(&state, &runtime).await;
    let rrc_private_key = delivery
        .identity()
        .get_private_key()
        .ok_or_else(|| anyhow::anyhow!("local identity has no private key"))?;
    let rrc_task = crate::rrc::spawn(
        state.clone(),
        runtime.clone(),
        *rrc_private_key,
        delivery.identity().hash,
    );

    let mut command_rx = state
        .network_command_rx
        .lock()
        .await
        .take()
        .ok_or_else(|| anyhow::anyhow!("network command receiver already taken"))?;
    refresh_interfaces(&state, &runtime, &destination_hash).await;
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = state.shutdown.wait() => break,
            packet = packet_rx.recv() => {
                if let Some((payload, _link_id)) = packet {
                    receive_message(&state, &runtime, &payload).await;
                }
            }
            resource = resource_rx.recv() => {
                if let Some((payload, _link_id)) = resource {
                    receive_message(&state, &runtime, &payload).await;
                }
            }
            command = command_rx.recv() => {
                let Some(command) = command else { break };
                match command {
                    NetworkCommand::SendMessage { destination_hash, title, content, response } => {
                        let result = send_direct(&state, &runtime, &delivery, destination_hash, &title, &content)
                            .await
                            .map_err(|error| error.to_string());
                        let _ = response.send(result);
                    }
                    NetworkCommand::FetchPage { url, reload, fields, response } => {
                        let result = fetch_page(&state, &runtime, &delivery, &url, reload, &fields)
                            .await
                            .map_err(|error| error.to_string());
                        let _ = response.send(result);
                    }
                    NetworkCommand::FetchFile { url, response } => {
                        let result = fetch_file(&runtime, &delivery, &url)
                            .await
                            .map_err(|error| error.to_string());
                        let _ = response.send(result);
                    }
                }
            }
            announce = delivery_announces.recv() => {
                if let Some(announce) = announce {
                    process_announce(&state, announce, DirectoryKind::Peer);
                }
            }
            announce = node_announces.recv() => {
                if let Some(announce) = announce {
                    process_announce(&state, announce, DirectoryKind::Node);
                }
            }
            announce = propagation_announces.recv() => {
                if let Some(announce) = announce {
                    process_announce(&state, announce, DirectoryKind::Propagation);
                }
            }
            _ = interval.tick() => {
                refresh_interfaces(&state, &runtime, &destination_hash).await;
            }
        }
    }
    link_manager_task.abort();
    let _ = rrc_task.await;
    Ok(())
}

async fn fetch_page(
    state: &Arc<AppState>,
    runtime: &reticulum::ReticulumHandle,
    delivery: &DeliveryIdentity,
    url: &NomadUrl,
    reload: bool,
    fields: &BTreeMap<String, String>,
) -> anyhow::Result<BrowserPage> {
    if !url.is_page() {
        anyhow::bail!("browser page request requires a /page/ path");
    }
    let canonical = url.canonical();
    let now = now_f64() as i64;
    if fields.is_empty()
        && !reload
        && let Some(bytes) = state.database.cached_page(&canonical, now)?
    {
        return Ok(parse_page(canonical, &bytes, true)?);
    }

    let request_data = if fields.is_empty() {
        None
    } else {
        Some(encode_form_fields(fields)?)
    };
    let response = request_node(runtime, delivery, url, request_data.as_deref()).await?;
    let page = parse_page(canonical.clone(), &response, false)?;
    if fields.is_empty() {
        state
            .database
            .cache_page(&canonical, &response, page.cache_seconds, now)?;
    }
    Ok(page)
}

async fn fetch_file(
    runtime: &reticulum::ReticulumHandle,
    delivery: &DeliveryIdentity,
    url: &NomadUrl,
) -> anyhow::Result<DownloadedFile> {
    if !url.is_file() {
        anyhow::bail!("file request requires a /file/ path");
    }
    let bytes = request_node(runtime, delivery, url, None).await?;
    if bytes.len() > 64 * 1024 * 1024 {
        anyhow::bail!("download exceeds the 64 MiB client limit");
    }
    let filename = url
        .path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("download.bin")
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(180)
        .collect();
    Ok(DownloadedFile { filename, bytes })
}

async fn request_node(
    runtime: &reticulum::ReticulumHandle,
    delivery: &DeliveryIdentity,
    url: &NomadUrl,
    request_data: Option<&[u8]>,
) -> anyhow::Result<Vec<u8>> {
    runtime
        .transport_tx
        .send(TransportMessage::RequestPath {
            destination_hash: url.destination_hash,
        })
        .await?;
    runtime
        .await_path(url.destination_hash, Duration::from_secs(30))
        .await?;
    let private_key = delivery
        .identity()
        .get_private_key()
        .ok_or_else(|| anyhow::anyhow!("local identity has no private key"))?;
    let link_identity = Identity::from_private_key(private_key.as_ref())?;
    let mut link = LinkSession::open(
        runtime,
        link_identity,
        url.destination_hash,
        1,
        Duration::from_secs(30),
    )
    .await?;
    let response = link
        .request(&url.path, request_data, Duration::from_secs(120))
        .await?;
    link.close().await?;
    Ok(response)
}

fn encode_form_fields(fields: &BTreeMap<String, String>) -> anyhow::Result<Vec<u8>> {
    if fields.len() > 256 {
        anyhow::bail!("too many form fields");
    }
    let value = rmpv::Value::Map(
        fields
            .iter()
            .map(|(key, value)| -> anyhow::Result<_> {
                if !(key.starts_with("field_") || key.starts_with("var_"))
                    || key.len() > 134
                    || value.len() > 1024 * 1024
                {
                    anyhow::bail!("invalid form field");
                }
                Ok((
                    rmpv::Value::String(key.clone().into()),
                    rmpv::Value::String(value.clone().into()),
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
    );
    let mut encoded = Vec::new();
    rmpv::encode::write_value(&mut encoded, &value)?;
    Ok(encoded)
}

async fn seed_cached_announces(state: &Arc<AppState>, runtime: &reticulum::ReticulumHandle) {
    let Some(TransportQueryResponse::Announces(entries)) = runtime
        .query_control(TransportQuery::GetRecentAnnounces)
        .await
    else {
        return;
    };
    let delivery_name = rns_identity::name_hash::name_hash("lxmf.delivery");
    let node_name = rns_identity::name_hash::name_hash("nomadnetwork.node");
    let propagation_name = rns_identity::name_hash::name_hash("lxmf.propagation");
    for entry in entries {
        let kind = if entry.name_hash == delivery_name {
            DirectoryKind::Peer
        } else if entry.name_hash == node_name {
            DirectoryKind::Node
        } else if entry.name_hash == propagation_name {
            DirectoryKind::Propagation
        } else {
            continue;
        };
        let identity_hash = entry
            .public_key
            .and_then(|key| Identity::from_public_key(&key).ok())
            .map(|identity| identity.hash);
        process_announce(
            state,
            rns_transport::messages::AnnounceHandlerEvent {
                destination_hash: entry.dest_hash,
                identity_hash,
                announce_packet_hash: [0; 32],
                is_path_response: entry.is_path_response,
                hops: entry.hops,
                app_data: entry.app_data,
                public_key: entry.public_key,
                ratchet: entry.ratchet,
                name_hash: entry.name_hash,
            },
            kind,
        );
    }
}

#[derive(Clone, Copy)]
enum DirectoryKind {
    Peer,
    Node,
    Propagation,
}

fn process_announce(
    state: &Arc<AppState>,
    announce: rns_transport::messages::AnnounceHandlerEvent,
    kind: DirectoryKind,
) {
    let identity_hash = announce.identity_hash;
    let delivery_hash = identity_hash
        .map(|identity| Destination::hash_from_name_and_identity("lxmf.delivery", Some(&identity)));
    let display_name = match kind {
        DirectoryKind::Peer => announce
            .app_data
            .as_deref()
            .and_then(lxmf_core::handlers::display_name_from_app_data),
        DirectoryKind::Node => announce
            .app_data
            .as_deref()
            .and_then(|data| std::str::from_utf8(data).ok())
            .map(sanitise_name),
        DirectoryKind::Propagation => announce
            .app_data
            .as_deref()
            .and_then(lxmf_core::handlers::pn_name_from_app_data),
    };
    let active = match kind {
        DirectoryKind::Propagation => announce
            .app_data
            .as_deref()
            .and_then(lxmf_core::handlers::parse_pn_announce_data)
            .is_some_and(|data| data.node_state),
        _ => true,
    };
    let entry = DirectoryEntry {
        destination_hash: hex::encode(announce.destination_hash),
        identity_hash: identity_hash.map(hex::encode),
        delivery_hash: delivery_hash.map(hex::encode),
        kind: match kind {
            DirectoryKind::Peer => "peer",
            DirectoryKind::Node => "node",
            DirectoryKind::Propagation => "propagation",
        }
        .into(),
        display_name,
        hops: announce.hops,
        last_seen: now_f64() as i64,
        active,
    };
    if let Err(error) = state
        .database
        .upsert_directory(&entry, announce.app_data.as_deref())
    {
        tracing::warn!(%error, "could not persist announce");
        return;
    }
    let _ = state.events.send(ServerEvent::DirectoryChanged(entry));
}

fn sanitise_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(128)
        .collect::<String>()
        .trim()
        .to_string()
}

async fn announce(
    runtime: &reticulum::ReticulumHandle,
    delivery: &mut DeliveryIdentity,
) -> anyhow::Result<()> {
    let destination_hash = delivery.destination_hash();
    let raw = delivery.announce_packet(now_f64())?;
    runtime
        .transport_tx
        .send(TransportMessage::Outbound(OutboundRequest {
            raw: Bytes::from(raw),
            destination_hash,
        }))
        .await?;
    Ok(())
}

async fn receive_message(
    state: &Arc<AppState>,
    runtime: &reticulum::ReticulumHandle,
    payload: &[u8],
) {
    let result = async {
        let mut message = LxMessage::unpack(payload)?;
        verify_message(runtime, &mut message).await;
        let verification_state = if message.signature_validated {
            "delivered"
        } else {
            match message.unverified_reason {
                Some(UnverifiedReason::SignatureInvalid) => "invalid_signature",
                _ => "source_unknown",
            }
        };
        let source_hash = hex::encode(message.source_hash);
        let stored = state.database.store_message(NewMessage {
            destination_hash: &source_hash,
            source_hash: &source_hash,
            title: &message.title,
            content: &message.content,
            timestamp: message.timestamp as i64,
            outbound: false,
            state: verification_state,
        })?;
        let _ = state.events.send(ServerEvent::MessageStored(stored));
        anyhow::Ok(())
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "could not process inbound LXMF message");
    }
}

async fn verify_message(runtime: &reticulum::ReticulumHandle, message: &mut LxMessage) {
    let public_key = match runtime
        .query_control(TransportQuery::Recall {
            destination_hash: message.source_hash,
        })
        .await
    {
        Some(TransportQueryResponse::Announce(Some(entry))) => entry.public_key,
        _ => None,
    };
    let Some(public_key) = public_key else {
        message.unverified_reason = Some(UnverifiedReason::SourceUnknown);
        return;
    };
    let Ok(signing_bytes) = public_key[32..].try_into() else {
        message.unverified_reason = Some(UnverifiedReason::SignatureInvalid);
        return;
    };
    match Ed25519PublicKey::from_bytes(signing_bytes) {
        Ok(key) if message.verify(&key) => {}
        _ => message.unverified_reason = Some(UnverifiedReason::SignatureInvalid),
    }
}

async fn send_direct(
    state: &Arc<AppState>,
    runtime: &reticulum::ReticulumHandle,
    delivery: &DeliveryIdentity,
    recipient: [u8; 16],
    title: &str,
    content: &str,
) -> anyhow::Result<MessageView> {
    runtime
        .transport_tx
        .send(TransportMessage::RequestPath {
            destination_hash: recipient,
        })
        .await?;
    runtime
        .await_path(recipient, Duration::from_secs(30))
        .await?;
    let public_key = match runtime
        .query_control(TransportQuery::Recall {
            destination_hash: recipient,
        })
        .await
    {
        Some(TransportQueryResponse::Announce(Some(entry))) => entry.public_key,
        _ => None,
    }
    .ok_or_else(|| anyhow::anyhow!("destination identity is not known"))?;

    let message = delivery.message(recipient, title, content, DeliveryMethod::Direct)?;
    let timestamp = message.timestamp as i64;
    let payload = message.pack()?;
    let private_key = delivery
        .identity()
        .get_private_key()
        .ok_or_else(|| anyhow::anyhow!("local identity has no private key"))?;
    let link_identity = Identity::from_private_key(private_key.as_ref())?;
    let mut link = LinkSession::open_with_public_key(
        runtime,
        link_identity,
        recipient,
        public_key,
        1,
        Duration::from_secs(30),
    )
    .await?;
    link.identify().await?;
    link.send_payload(payload, true, Duration::from_secs(120))
        .await?;
    link.close().await?;

    let recipient_hash = hex::encode(recipient);
    let source_hash = hex::encode(delivery.destination_hash());
    let stored = state.database.store_message(NewMessage {
        destination_hash: &recipient_hash,
        source_hash: &source_hash,
        title,
        content,
        timestamp,
        outbound: true,
        state: "delivered",
    })?;
    let _ = state
        .events
        .send(ServerEvent::MessageStored(stored.clone()));
    Ok(stored)
}

fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn load_or_create_identity(path: &std::path::Path) -> anyhow::Result<Identity> {
    if path.exists() {
        return Ok(Identity::from_file(path)?);
    }
    let identity = Identity::new();
    identity.to_file(path)?;
    Ok(identity)
}

async fn refresh_interfaces(
    state: &Arc<AppState>,
    runtime: &reticulum::ReticulumHandle,
    destination_hash: &str,
) {
    let interfaces = match runtime
        .query_control(TransportQuery::GetInterfaceStats)
        .await
    {
        Some(TransportQueryResponse::InterfaceStats(entries)) => entries
            .into_iter()
            .map(|entry| InterfaceSnapshot {
                id: entry.id,
                name: entry.name,
                online: entry.online,
                mode: entry.mode,
                role: entry.role,
                bitrate: entry.bitrate,
                mtu: entry.mtu,
                rx_bytes: entry.rx_bytes,
                tx_bytes: entry.tx_bytes,
                rx_rate: entry.rx_rate,
                tx_rate: entry.tx_rate,
                held_announces: entry.held_announces,
                tx_drops: entry.tx_drops,
            })
            .collect(),
        _ => Vec::new(),
    };
    state
        .set_network(NetworkSnapshot {
            state: NetworkState::Online,
            detail: format!("{:?} instance", runtime.instance_mode),
            destination_hash: Some(destination_hash.into()),
            interfaces,
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_fields_use_python_nomadnet_request_keys() {
        let fields = BTreeMap::from([
            ("field_name".to_string(), "Alice".to_string()),
            ("var_mode".to_string(), "short".to_string()),
        ]);
        let encoded = encode_form_fields(&fields).unwrap();
        let value = rmpv::decode::read_value(&mut encoded.as_slice()).unwrap();
        let map = value.as_map().unwrap();
        assert!(map.iter().any(|(key, value)| {
            key.as_str() == Some("field_name") && value.as_str() == Some("Alice")
        }));
        assert!(map.iter().any(|(key, value)| {
            key.as_str() == Some("var_mode") && value.as_str() == Some("short")
        }));
    }

    #[test]
    fn form_fields_reject_unscoped_keys() {
        let fields = BTreeMap::from([("name".to_string(), "Alice".to_string())]);
        assert!(encode_form_fields(&fields).is_err());
    }
}
