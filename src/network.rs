use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use bytes::Bytes;
use lxmf_core::application::DeliveryIdentity;
use lxmf_core::constants::{DeliveryMethod, UnverifiedReason};
use lxmf_core::message::LxMessage;
use lxmf_core::propagation_client::{PropagationClient, PropagationClientState};
use rns_crypto::ed25519::Ed25519PublicKey;
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use rns_runtime::application::announce_stream;
use rns_runtime::link_client::{LinkResponse, LinkSession};
use rns_runtime::link_manager::LinkManager;
use rns_runtime::reticulum;
use rns_transport::messages::{
    OutboundRequest, TransportMessage, TransportQuery, TransportQueryResponse,
};
use tokio::sync::oneshot;

use crate::app::AppState;
use crate::browser::{
    BrowserPage, DownloadedFile, NomadUrl, download_content_type, parse_page, safe_download_name,
};
use crate::db::NewMessage;
use crate::db::PendingMessage;
use crate::models::{
    DirectoryEntry, InterfaceSnapshot, MessageView, NetworkSnapshot, NetworkState, ServerEvent,
};

const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES: usize = 64 * 1024 * 1024;

pub enum NetworkCommand {
    SetAnnounceName {
        name: Option<String>,
        announce_now: bool,
        response: oneshot::Sender<Result<(), String>>,
    },
    SendMessage {
        destination_hash: [u8; 16],
        title: String,
        content: String,
        delivery_method: String,
        propagation_node: Option<[u8; 16]>,
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
    let announce_name = state
        .database
        .setting("announce_name")?
        .unwrap_or_else(|| "rsNomadNet".into());
    let mut delivery = DeliveryIdentity::new(
        identity,
        (!announce_name.is_empty()).then_some(announce_name),
        None,
    )?;
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
    let (raw_tx, mut raw_rx) = tokio::sync::mpsc::channel(256);
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
    link_manager.set_inbound_raw_channel(raw_tx);
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
    let browser_private_key = *rrc_private_key;
    let rrc_task = crate::rrc::spawn(
        state.clone(),
        runtime.clone(),
        *rrc_private_key,
        delivery.identity().hash,
    );
    let mut propagation_client = PropagationClient::new(
        runtime.transport_tx.clone(),
        Some(delivery.identity().get_public_key()),
        delivery.identity().get_signing_key(),
    );
    propagation_client.set_runtime(runtime.clone());
    let mut selected_propagation_node = None;
    let mut last_propagation_download = 0.0;
    let (outbound_result_tx, mut outbound_result_rx) = tokio::sync::mpsc::channel(16);
    let mut outbound_active = false;

    let mut command_rx = state
        .network_command_rx
        .lock()
        .await
        .take()
        .ok_or_else(|| anyhow::anyhow!("network command receiver already taken"))?;
    let recovered = state
        .database
        .recover_interrupted_outbound(now_f64() as i64)?;
    if recovered > 0 {
        tracing::info!(recovered, "recovered interrupted outbound LXMF deliveries");
    }
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
            raw = raw_rx.recv() => {
                if let Some(raw) = raw {
                    receive_opportunistic_message(&state, &runtime, &delivery, &raw).await;
                }
            }
            command = command_rx.recv() => {
                let Some(command) = command else { break };
                match command {
                    NetworkCommand::SetAnnounceName {
                        name,
                        announce_now,
                        response,
                    } => {
                        delivery.set_display_name(name);
                        let result = if announce_now {
                            announce(&runtime, &mut delivery)
                                .await
                                .map_err(|error| error.to_string())
                        } else {
                            Ok(())
                        };
                        let _ = response.send(result);
                    }
                    NetworkCommand::SendMessage {
                        destination_hash,
                        title,
                        content,
                        delivery_method,
                        propagation_node,
                        response,
                    } => {
                        let result = queue_outbound(
                            &state,
                            &delivery,
                            destination_hash,
                            &title,
                            &content,
                            &delivery_method,
                            propagation_node,
                        )
                        .map_err(|error| error.to_string());
                        let _ = response.send(result);
                    }
                    NetworkCommand::FetchPage { url, reload, fields, response } => {
                        let state = state.clone();
                        let runtime = runtime.clone();
                        tokio::spawn(async move {
                            let result = async {
                                let identity = Identity::from_private_key(&browser_private_key)?;
                                fetch_page(&state, &runtime, &identity, &url, reload, &fields).await
                            }
                            .await
                            .map_err(|error: anyhow::Error| error.to_string());
                            let _ = response.send(result);
                        });
                    }
                    NetworkCommand::FetchFile { url, response } => {
                        let runtime = runtime.clone();
                        tokio::spawn(async move {
                            let result = async {
                                let identity = Identity::from_private_key(&browser_private_key)?;
                                fetch_file(&runtime, &identity, &url).await
                            }
                            .await
                            .map_err(|error: anyhow::Error| error.to_string());
                            let _ = response.send(result);
                        });
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
            result = outbound_result_rx.recv() => {
                if let Some(result) = result {
                    outbound_active = false;
                    finish_outbound(&state, result);
                }
            }
            _ = interval.tick() => {
                refresh_interfaces(&state, &runtime, &destination_hash).await;
                propagation_client.tick();
                for payload in propagation_client.take_received_messages() {
                    receive_propagated_message(&state, &runtime, &delivery, &payload).await;
                }
                if let Some(node) = select_propagation_node(&state) {
                    if selected_propagation_node != Some(node) {
                        propagation_client.set_propagation_node(node);
                        selected_propagation_node = Some(node);
                        last_propagation_download = 0.0;
                    }
                    if now_f64() - last_propagation_download >= 90.0
                        && propagation_client.state == PropagationClientState::Idle
                    {
                        runtime.transport_tx.send(TransportMessage::RequestPath {
                            destination_hash: node,
                        }).await.ok();
                        if propagation_client.start_download() {
                            last_propagation_download = now_f64();
                        }
                    }
                }
                if !outbound_active
                    && let Some(pending) = state.database.pending_messages(now_f64() as i64, 1)?.into_iter().next()
                {
                    if let Ok(view) = state.database.update_message_delivery(
                        pending.id,
                        "sending",
                        &pending.delivery_method,
                        pending.attempts,
                        (now_f64() as i64).saturating_add(180),
                        None,
                    ) {
                        let _ = state.events.send(ServerEvent::MessageStored(view));
                    }
                    let state_for_task = state.clone();
                    let runtime_for_task = runtime.clone();
                    let private_key = delivery.identity().get_private_key()
                        .ok_or_else(|| anyhow::anyhow!("local identity has no private key"))?
                        .to_vec();
                    let tx = outbound_result_tx.clone();
                    outbound_active = true;
                    tokio::spawn(async move {
                        let result = attempt_outbound(
                            &state_for_task,
                            &runtime_for_task,
                            &private_key,
                            pending,
                        ).await;
                        let _ = tx.send(result).await;
                    });
                }
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
    identity: &Identity,
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
    let response = request_node(
        runtime,
        identity,
        url,
        request_data.as_deref(),
        MAX_PAGE_BYTES,
    )
    .await?;
    let page = parse_page(canonical.clone(), &response.data, false)?;
    if fields.is_empty() {
        state
            .database
            .cache_page(&canonical, &response.data, page.cache_seconds, now)?;
    }
    Ok(page)
}

async fn fetch_file(
    runtime: &reticulum::ReticulumHandle,
    identity: &Identity,
    url: &NomadUrl,
) -> anyhow::Result<DownloadedFile> {
    if !url.is_file() {
        anyhow::bail!("file request requires a /file/ path");
    }
    let response = request_node(runtime, identity, url, None, MAX_DOWNLOAD_BYTES).await?;
    if response.data.len() > MAX_DOWNLOAD_BYTES {
        anyhow::bail!("download exceeds the 64 MiB client limit");
    }
    let metadata_name = response
        .metadata
        .as_deref()
        .and_then(filename_from_resource_metadata);
    let filename = safe_download_name(
        metadata_name
            .as_deref()
            .unwrap_or_else(|| url.path.rsplit('/').next().unwrap_or("download.bin")),
    );
    let content_type = download_content_type(&filename).to_string();
    Ok(DownloadedFile {
        filename,
        content_type,
        bytes: response.data,
    })
}

fn filename_from_resource_metadata(metadata: &[u8]) -> Option<String> {
    let value = rmpv::decode::read_value(&mut std::io::Cursor::new(metadata)).ok()?;
    let rmpv::Value::Map(entries) = value else {
        return None;
    };
    entries.into_iter().find_map(|(key, value)| {
        let is_name = key.as_str() == Some("name")
            || matches!(&key, rmpv::Value::Binary(bytes) if bytes == b"name");
        if !is_name {
            return None;
        }
        match value {
            rmpv::Value::String(value) => value.as_str().map(str::to_string),
            rmpv::Value::Binary(value) => String::from_utf8(value).ok(),
            _ => None,
        }
    })
}

async fn request_node(
    runtime: &reticulum::ReticulumHandle,
    identity: &Identity,
    url: &NomadUrl,
    request_data: Option<&[u8]>,
    max_response_bytes: usize,
) -> anyhow::Result<LinkResponse> {
    runtime
        .transport_tx
        .send(TransportMessage::RequestPath {
            destination_hash: url.destination_hash,
        })
        .await?;
    runtime
        .await_path(url.destination_hash, Duration::from_secs(30))
        .await?;
    let mut link = LinkSession::open(
        runtime,
        identity.clone(),
        url.destination_hash,
        1,
        Duration::from_secs(30),
    )
    .await?;
    let response = link
        .request_with_metadata_limit(
            &url.path,
            request_data,
            Duration::from_secs(120),
            max_response_bytes,
        )
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

struct OutboundAttemptResult {
    message: PendingMessage,
    method: String,
    result: anyhow::Result<()>,
}

#[derive(Debug, thiserror::Error)]
enum OpportunisticSendError {
    #[error("opportunistic message exceeds Reticulum MTU")]
    MtuExceeded,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
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
        let message_hash = message.hash.or(message.message_id).map(hex::encode);
        if message_hash
            .as_deref()
            .is_some_and(|hash| state.database.message_hash_exists(hash).unwrap_or(false))
        {
            return anyhow::Ok(());
        }
        let source_hash = hex::encode(message.source_hash);
        let stored = state.database.store_message(NewMessage {
            destination_hash: &source_hash,
            source_hash: &source_hash,
            title: &message.title,
            content: &message.content,
            timestamp: message.timestamp as i64,
            outbound: false,
            state: verification_state,
            delivery_method: "incoming",
            attempts: 0,
            next_attempt: 0,
            last_error: None,
            message_hash: message_hash.as_deref(),
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
    let Some(signing_bytes) = public_key.get(32..64) else {
        message.unverified_reason = Some(UnverifiedReason::SignatureInvalid);
        return;
    };
    let Ok(signing_bytes) = signing_bytes.try_into() else {
        message.unverified_reason = Some(UnverifiedReason::SignatureInvalid);
        return;
    };
    match Ed25519PublicKey::from_bytes(signing_bytes) {
        Ok(key) if message.verify(&key) => {}
        _ => message.unverified_reason = Some(UnverifiedReason::SignatureInvalid),
    }
}

fn queue_outbound(
    state: &Arc<AppState>,
    delivery: &DeliveryIdentity,
    recipient: [u8; 16],
    title: &str,
    content: &str,
    requested_method: &str,
    propagation_node: Option<[u8; 16]>,
) -> anyhow::Result<MessageView> {
    let method = match requested_method.trim().to_ascii_lowercase().as_str() {
        "" | "auto" | "automatic" => "automatic",
        "opportunistic" => "opportunistic",
        "direct" => "direct",
        "propagated" => "propagated",
        _ => anyhow::bail!("unknown LXMF delivery method"),
    };
    if method != "propagated" && propagation_node.is_some() {
        anyhow::bail!("propagation_node is only valid for propagated delivery");
    }
    let recipient_hash = hex::encode(recipient);
    let source_hash = hex::encode(delivery.destination_hash());
    state.database.queue_message(
        NewMessage {
            destination_hash: &recipient_hash,
            source_hash: &source_hash,
            title,
            content,
            timestamp: now_f64() as i64,
            outbound: true,
            state: "queued",
            delivery_method: method,
            attempts: 0,
            next_attempt: 0,
            last_error: None,
            message_hash: None,
        },
        propagation_node.as_ref().map(hex::encode).as_deref(),
    )
}

async fn attempt_outbound(
    state: &Arc<AppState>,
    runtime: &reticulum::ReticulumHandle,
    private_key: &[u8],
    message: PendingMessage,
) -> OutboundAttemptResult {
    let result = async {
        let recipient = parse_hash(&message.destination_hash)?;
        let identity = Identity::from_private_key(private_key)?;
        let delivery = DeliveryIdentity::new(identity, Some("rsNomadNet".into()), None)?;
        match message.delivery_method.as_str() {
            "automatic" => {
                match send_opportunistic(
                    runtime,
                    &delivery,
                    recipient,
                    &message.title,
                    &message.content,
                )
                .await
                {
                    Ok(()) => Ok("opportunistic"),
                    Err(OpportunisticSendError::MtuExceeded) => {
                        send_direct(
                            runtime,
                            &delivery,
                            recipient,
                            &message.title,
                            &message.content,
                        )
                        .await?;
                        Ok("direct")
                    }
                    Err(error) => Err(error.into()),
                }
            }
            "opportunistic" => {
                send_opportunistic(
                    runtime,
                    &delivery,
                    recipient,
                    &message.title,
                    &message.content,
                )
                .await?;
                Ok("opportunistic")
            }
            "direct" => {
                send_direct(
                    runtime,
                    &delivery,
                    recipient,
                    &message.title,
                    &message.content,
                )
                .await?;
                Ok("direct")
            }
            "propagated" => {
                let node = match message.propagation_node.as_deref() {
                    Some(hash) => parse_hash(hash)?,
                    None => select_propagation_node(state)
                        .ok_or_else(|| anyhow::anyhow!("no active propagation node is known"))?,
                };
                let stamp_cost = state
                    .database
                    .destination_app_data(&hex::encode(node))?
                    .as_deref()
                    .and_then(lxmf_core::handlers::parse_pn_announce_data)
                    .map(|data| data.stamp_cost)
                    .unwrap_or(0);
                send_propagated(
                    runtime,
                    &delivery,
                    recipient,
                    node,
                    &message.title,
                    &message.content,
                    stamp_cost,
                )
                .await?;
                Ok("propagated")
            }
            other => anyhow::bail!("unsupported queued delivery method {other}"),
        }
    }
    .await;
    match result {
        Ok(method) => OutboundAttemptResult {
            message,
            method: method.into(),
            result: Ok(()),
        },
        Err(error) => OutboundAttemptResult {
            method: message.delivery_method.clone(),
            message,
            result: Err(error),
        },
    }
}

async fn destination_identity(
    runtime: &reticulum::ReticulumHandle,
    destination: [u8; 16],
) -> anyhow::Result<Identity> {
    runtime
        .transport_tx
        .send(TransportMessage::RequestPath {
            destination_hash: destination,
        })
        .await?;
    runtime
        .await_path(destination, Duration::from_secs(30))
        .await?;
    let public_key = match runtime
        .query_control(TransportQuery::Recall {
            destination_hash: destination,
        })
        .await
    {
        Some(TransportQueryResponse::Announce(Some(entry))) => entry.public_key,
        _ => None,
    }
    .ok_or_else(|| anyhow::anyhow!("destination identity is not known"))?;
    Ok(Identity::from_public_key(&public_key)?)
}

async fn send_opportunistic(
    runtime: &reticulum::ReticulumHandle,
    delivery: &DeliveryIdentity,
    recipient: [u8; 16],
    title: &str,
    content: &str,
) -> Result<(), OpportunisticSendError> {
    let remote = destination_identity(runtime, recipient).await?;
    let message = delivery
        .message(recipient, title, content, DeliveryMethod::Opportunistic)
        .map_err(anyhow::Error::from)?;
    let payload = message
        .pack_opportunistic_encrypted(|plaintext| {
            remote
                .encrypt(plaintext, None)
                .map_err(|error| lxmf_core::message::MessageError::PackFailed(error.to_string()))
        })
        .map_err(anyhow::Error::from)?;
    match rns_runtime::application::send_pre_encrypted_packet_with_receipt(
        runtime,
        recipient,
        &payload,
        Duration::from_secs(15),
    )
    .await
    {
        Ok(_) => {}
        Err(rns_runtime::application::ApplicationError::MtuExceeded { .. }) => {
            return Err(OpportunisticSendError::MtuExceeded);
        }
        Err(error) => return Err(OpportunisticSendError::Other(error.into())),
    }
    Ok(())
}

async fn send_direct(
    runtime: &reticulum::ReticulumHandle,
    delivery: &DeliveryIdentity,
    recipient: [u8; 16],
    title: &str,
    content: &str,
) -> anyhow::Result<()> {
    let remote = destination_identity(runtime, recipient).await?;
    let public_key = remote.get_public_key();

    let message = delivery.message(recipient, title, content, DeliveryMethod::Direct)?;
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
    Ok(())
}

async fn send_propagated(
    runtime: &reticulum::ReticulumHandle,
    delivery: &DeliveryIdentity,
    recipient: [u8; 16],
    propagation_node: [u8; 16],
    title: &str,
    content: &str,
    stamp_cost: u8,
) -> anyhow::Result<()> {
    let remote = destination_identity(runtime, recipient).await?;
    let node = destination_identity(runtime, propagation_node).await?;
    let mut message = delivery.message(recipient, title, content, DeliveryMethod::Propagated)?;
    let (payload, _, _) = message.pack_propagated_encrypted_with_stamp(
        |plaintext| {
            remote
                .encrypt(plaintext, None)
                .map_err(|error| lxmf_core::message::MessageError::PackFailed(error.to_string()))
        },
        stamp_cost,
    )?;
    let private_key = delivery
        .identity()
        .get_private_key()
        .ok_or_else(|| anyhow::anyhow!("local identity has no private key"))?;
    let link_identity = Identity::from_private_key(private_key.as_ref())?;
    let mut link = LinkSession::open_with_public_key(
        runtime,
        link_identity,
        propagation_node,
        node.get_public_key(),
        1,
        Duration::from_secs(30),
    )
    .await?;
    link.identify().await?;
    link.send_payload(payload, false, Duration::from_secs(120))
        .await?;
    link.close().await?;
    Ok(())
}

fn select_propagation_node(state: &Arc<AppState>) -> Option<[u8; 16]> {
    state
        .database
        .best_propagation_node()
        .ok()
        .flatten()
        .and_then(|hash| parse_hash(&hash).ok())
}

fn finish_outbound(state: &Arc<AppState>, result: OutboundAttemptResult) {
    let attempts = result.message.attempts.saturating_add(1);
    let (status, next_attempt, error) = match result.result {
        Ok(()) => {
            let status = if result.method == "propagated" {
                "stored_on_node"
            } else {
                "delivered"
            };
            (status, 0, None)
        }
        Err(error) => {
            let text = error.to_string();
            if attempts >= 5 || text.contains("exceeds Reticulum MTU") {
                ("failed", 0, Some(text))
            } else {
                let delay = 5_i64.saturating_mul(1_i64 << attempts.min(6));
                (
                    "retrying",
                    (now_f64() as i64).saturating_add(delay.min(300)),
                    Some(text),
                )
            }
        }
    };
    match state.database.update_message_delivery(
        result.message.id,
        status,
        &result.method,
        attempts,
        next_attempt,
        error.as_deref(),
    ) {
        Ok(message) => {
            let _ = state.events.send(ServerEvent::MessageStored(message));
        }
        Err(error) => tracing::warn!(%error, "could not update outbound LXMF state"),
    }
}

async fn receive_propagated_message(
    state: &Arc<AppState>,
    runtime: &reticulum::ReticulumHandle,
    delivery: &DeliveryIdentity,
    payload: &[u8],
) {
    if payload.len() < 16 || payload[..16] != delivery.destination_hash() {
        tracing::warn!("discarding propagation payload for another destination");
        return;
    }
    let plaintext = match delivery.identity().decrypt(&payload[16..], None, false) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "could not decrypt propagated LXMF message");
            return;
        }
    };
    let unpacked = if plaintext.len() >= 16 && plaintext[..16] == delivery.destination_hash() {
        plaintext
    } else {
        let mut value = delivery.destination_hash().to_vec();
        value.extend_from_slice(&plaintext);
        value
    };
    receive_message(state, runtime, &unpacked).await;
}

async fn receive_opportunistic_message(
    state: &Arc<AppState>,
    runtime: &reticulum::ReticulumHandle,
    delivery: &DeliveryIdentity,
    raw: &[u8],
) {
    let (header, offset) = match rns_wire::header::PacketHeader::unpack(raw) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "could not parse opportunistic LXMF packet");
            return;
        }
    };
    if header.destination_hash != delivery.destination_hash() || raw.len() <= offset {
        return;
    }
    let plaintext = match delivery.identity().decrypt(&raw[offset..], None, false) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "could not decrypt opportunistic LXMF packet");
            return;
        }
    };
    let unpacked = if plaintext.len() >= 16 && plaintext[..16] == delivery.destination_hash() {
        plaintext
    } else {
        let mut value = delivery.destination_hash().to_vec();
        value.extend_from_slice(&plaintext);
        value
    };
    if LxMessage::unpack(&unpacked).is_err() {
        tracing::warn!("discarding invalid opportunistic LXMF payload");
        return;
    }
    if let Some(proof) = delivery_proof(delivery.identity(), raw, header.flags.header_type) {
        let destination_hash = rns_wire::hash::truncated_packet_hash(raw, header.flags.header_type);
        let _ = runtime
            .transport_tx
            .send(TransportMessage::Outbound(OutboundRequest {
                raw: Bytes::from(proof),
                destination_hash,
            }))
            .await;
    }
    receive_message(state, runtime, &unpacked).await;
}

fn delivery_proof(
    identity: &Identity,
    raw: &[u8],
    header_type: rns_wire::flags::HeaderType,
) -> Option<Vec<u8>> {
    let full_hash = rns_wire::hash::packet_hash(raw, header_type);
    let destination_hash = rns_wire::hash::truncated_packet_hash(raw, header_type);
    let signature = identity.sign(&full_hash)?;
    let mut proof = rns_wire::header::PacketHeader {
        flags: rns_wire::flags::PacketFlags {
            header_type: rns_wire::flags::HeaderType::Header1,
            context_flag: false,
            transport_type: rns_wire::flags::TransportType::Broadcast,
            destination_type: rns_wire::flags::DestinationType::Single,
            packet_type: rns_wire::flags::PacketType::Proof,
        },
        hops: 0,
        transport_id: None,
        destination_hash,
        context: rns_wire::context::PacketContext::None,
    }
    .pack();
    proof.extend_from_slice(&signature);
    Some(proof)
}

fn parse_hash(value: &str) -> anyhow::Result<[u8; 16]> {
    let bytes = hex::decode(value)?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("destination hash must contain 32 hexadecimal characters"))
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

    #[test]
    fn reads_python_nomadnet_resource_filename_metadata() {
        let mut metadata = Vec::new();
        rmpv::encode::write_value(
            &mut metadata,
            &rmpv::Value::Map(vec![(
                rmpv::Value::String("name".into()),
                rmpv::Value::Binary(b"docs/report.txt".to_vec()),
            )]),
        )
        .unwrap();
        assert_eq!(
            filename_from_resource_metadata(&metadata).as_deref(),
            Some("docs/report.txt")
        );
        assert!(filename_from_resource_metadata(b"not messagepack").is_none());
    }
}
