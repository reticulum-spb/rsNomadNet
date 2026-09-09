use super::*;
use std::collections::HashSet;

enum Completion {
    File,
    Sent(String, String, Result<(), String>),
    Hub(String, u64, Result<RrcHubView, String>),
    Browser,
}

fn load_history(service: &AppService, state: &mut UiState, destination: &str, limit: usize) {
    match service.recent_messages(destination, limit) {
        Ok(messages) => {
            if let Some(row) = state
                .conversations
                .iter_mut()
                .find(|row| row.destination_hash == destination)
            {
                row.messages = messages
                    .into_iter()
                    .map(|message| message_line(message.outbound, &message.state, &message.content))
                    .collect();
            }
        }
        Err(error) => {
            state.network.push(format!("History: {error}"));
        }
    }
}

fn refresh_conversations(service: &AppService, state: &mut UiState) {
    match service.conversations() {
        Ok(rows) => {
            let mut histories: HashMap<_, _> = state
                .conversations
                .drain(..)
                .map(|row| (row.destination_hash, row.messages))
                .collect();
            state.conversations = conversation_rows(service, rows);
            for row in &mut state.conversations {
                row.messages = histories.remove(&row.destination_hash).unwrap_or_default();
            }
        }
        Err(error) => state.network.push(format!("Conversations: {error}")),
    }
}

pub(super) async fn run(
    service: AppService,
    mut state: UiState,
    sender: UpdateSender,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<UiCommand>,
    mut events: tokio::sync::broadcast::Receiver<ServerEvent>,
) {
    let mut hubs: HashMap<String, RrcHubView> = HashMap::new();
    let mut versions: HashMap<String, u64> = HashMap::new();
    let mut opened: HashMap<String, usize> = HashMap::new();
    let mut in_flight = HashSet::new();
    // Dropping the bridge cancels its owned tasks as well.
    let mut jobs = tokio::task::JoinSet::new();
    let mut publish = tokio::time::interval(Duration::from_millis(100));
    publish.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    loop {
        tokio::select! {
            _ = publish.tick() => {
                let offers = service.pending_files();
                if offers != state.file_offers { state.file_offers = offers; dirty = true; }
                if dirty {
                    let _ = sender.send(state.clone());
                    state.send_results.clear();
                    dirty = false;
                }
            }
            _ = refresh.tick() => {
                state.network = network_lines(service.network_snapshot().await);
                dirty = true;
            }
            event = events.recv() => {
                match event {
                    Ok(ServerEvent::Snapshot(network) | ServerEvent::NetworkChanged(network)) => {
                        state.network = network_lines(network);
                    }
                    Ok(ServerEvent::DirectoryChanged(_)) => {
                        match service.directory() {
                            Ok(entries) => state.directory = directory_lines(entries),
                            Err(error) => state.network.push(format!("Directory: {error}")),
                        }
                        refresh_conversations(&service, &mut state);
                    }
                    Ok(ServerEvent::MessageStored(message)) => {
                        refresh_conversations(&service, &mut state);
                        if let Some(limit) = opened.get(&message.destination_hash) {
                            load_history(&service, &mut state, &message.destination_hash, *limit);
                        }
                    }
                    Ok(ServerEvent::RrcHubChanged(hub)) => {
                        *versions.entry(hub.destination_hash.clone()).or_default() += 1;
                        state.directory_views.insert(format!("rrc:{}", hub.destination_hash), rrc_hub_lines(&service, hub.clone()));
                        hubs.insert(hub.destination_hash.clone(), hub);
                    }
                    Ok(ServerEvent::RrcMessage(message)) => {
                        if let Some(hub) = hubs.get(&message.hub_hash) {
                            state.directory_views.insert(format!("rrc:{}", message.hub_hash), rrc_hub_lines(&service, hub.clone()));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        refresh_conversations(&service, &mut state);
                        for (destination, limit) in &opened { load_history(&service, &mut state, destination, *limit); }
                        if let Ok(entries) = service.directory() { state.directory = directory_lines(entries); }
                        // Do not pretend a general snapshot can recover live RRC state.
                        for hub in hubs.values_mut() {
                            hub.connected = false;
                            hub.detail = "RRC events lost; reopen hub to reconnect".into();
                            state.directory_views.insert(format!("rrc:{}", hub.destination_hash), rrc_hub_lines(&service, hub.clone()));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
                dirty = true;
            }
            completed = jobs.join_next(), if !jobs.is_empty() => {
                match completed {
                    Some(Ok(Completion::File)) => {}
                    Some(Ok(Completion::Sent(destination, content, result))) => {
                        in_flight.remove(&format!("send:{destination}"));
                        if result.is_ok() {
                            refresh_conversations(&service, &mut state);
                            load_history(&service, &mut state, &destination, opened.get(&destination).copied().unwrap_or(500));
                        }
                        state.send_results.insert(destination, SendResult { content, error: result.err() });
                    }
                    Some(Ok(Completion::Hub(destination, version, result))) => {
                        in_flight.remove(&format!("rrc:{destination}"));
                        // A connect reply describes the state BEFORE WELCOME.
                        // Never overwrite a subsequent hub event with that reply.
                        if versions.get(&destination).copied().unwrap_or_default() == version {
                            let lines = match result {
                                Ok(hub) => {
                                    hubs.insert(destination.clone(), hub.clone());
                                    rrc_hub_lines(&service, hub)
                                }
                                Err(error) => vec![format!("Connection failed: {error}")],
                            };
                            state.directory_views.insert(format!("rrc:{destination}"), lines);
                        }
                    }
                    Some(Ok(Completion::Browser)) => {}
                    Some(Err(error)) => state.network.push(format!("Background task failed: {error}")),
                    None => {}
                }
                dirty = true;
            }
            command = commands.recv() => {
                let Some(command) = command else { break };
                dirty = true;
                if let UiCommand::SendFile { destination_hash, path, reply } = command {
                    if jobs.len() >= 16 { let _ = reply.send(Err("Too many pending transfers".into())); }
                    else {
                        let service = service.clone();
                        jobs.spawn(async move {
                            let result = service.send_file(&destination_hash, path).await.map(|_| "File delivered".into()).map_err(|e| e.to_string());
                            let _ = reply.send(result);
                            Completion::File
                        });
                    }
                    continue;
                }
                if let UiCommand::DecideFile { id, accept, reply } = command {
                    if accept && jobs.len() >= 16 { let _ = reply.send(Err("Too many pending operations; try again".into())); }
                    else {
                        let service = service.clone();
                        jobs.spawn(async move {
                            let result = service.decide_file(id, accept).await.map(|path| path.map(|p| format!("Saved to {}", p.display())).unwrap_or_else(|| "File declined".into())).map_err(|e| e.to_string());
                            let _ = reply.send(result);
                            Completion::File
                        });
                    }
                    continue;
                }
                if let UiCommand::ClearConversation { destination_hash, reply } = command {
                    let result = if in_flight.contains(&format!("send:{destination_hash}")) {
                        Err("Cannot clear history while sending a message".into())
                    } else {
                        service.clear_conversation(&destination_hash).map(|_| ()).map_err(|e| e.to_string())
                    };
                    if result.is_ok() {
                        refresh_conversations(&service, &mut state);
                        if let Some(limit) = opened.get_mut(&destination_hash) {
                            *limit = 500;
                            load_history(&service, &mut state, &destination_hash, *limit);
                        }
                    }
                    let _ = reply.send(result);
                    continue;
                }
                if let UiCommand::BrowserFetch(request) = command {
                    if jobs.len() >= 16 { request.reject("Too many pending requests; try again shortly"); }
                    else {
                        let service = service.clone();
                        jobs.spawn(async move { request.execute(service).await; Completion::Browser });
                    }
                    continue;
                }
                if let UiCommand::LoadOlder { destination_hash } = command {
                    if let Some(limit) = opened.get_mut(&destination_hash) {
                        *limit = (*limit + 500).min(10_000);
                        load_history(&service, &mut state, &destination_hash, *limit);
                    }
                    continue;
                }
                if let UiCommand::CloseConversation { destination_hash } = command {
                    opened.remove(&destination_hash);
                    if let Some(row) = state.conversations.iter_mut().find(|row| row.destination_hash == destination_hash) {
                        row.messages.clear();
                    }
                    continue;
                }
                if let UiCommand::OpenConversation { destination_hash } = command {
                    opened.insert(destination_hash.clone(), 500);
                    if let Err(error) = service.mark_conversation_read(&destination_hash) {
                        state.network.push(format!("Mark read: {error}"));
                    }
                    refresh_conversations(&service, &mut state);
                    load_history(&service, &mut state, &destination_hash, 500);
                    continue;
                }
                let (key, destination) = match &command {
                    UiCommand::SendMessage { destination_hash, .. } => (format!("send:{destination_hash}"), destination_hash),
                    UiCommand::ConnectRrc { destination_hash } => (format!("rrc:{destination_hash}"), destination_hash),
                    UiCommand::BrowserFetch(_) | UiCommand::ClearConversation { .. } | UiCommand::SendFile { .. } | UiCommand::DecideFile { .. } => unreachable!(),
                    UiCommand::OpenConversation { .. } | UiCommand::CloseConversation { .. } | UiCommand::LoadOlder { .. } => unreachable!(),
                };
                if let UiCommand::ConnectRrc { .. } = &command {
                    if let Some(hub) = hubs.get(destination).filter(|hub| hub.connected) {
                        state.directory_views.insert(key, rrc_hub_lines(&service, hub.clone()));
                        continue;
                    }
                }
                if in_flight.contains(&key) { continue; }
                if jobs.len() >= 16 {
                    let error = "Too many pending operations; try again shortly".to_owned();
                    if let UiCommand::SendMessage { destination_hash, content } = command {
                        state.send_results.insert(destination_hash, SendResult { content, error: Some(error) });
                    } else {
                        state.directory_views.insert(key, vec![error]);
                    }
                    continue;
                }
                in_flight.insert(key.clone());
                let service = service.clone();
                match command {
                    UiCommand::SendMessage { destination_hash, content } => {
                        jobs.spawn(async move {
                            let result = service.send_message(SendMessage {
                                destination_hash: destination_hash.clone(), content: content.clone(),
                                title: String::new(), delivery_method: "automatic".into(), propagation_node: None,
                            }).await.map(|_| ()).map_err(|error| error.to_string());
                            Completion::Sent(destination_hash, content, result)
                        });
                    }
                    UiCommand::ConnectRrc { destination_hash } => {
                        state.directory_views.insert(key, vec!["Connecting…".into()]);
                        let version = versions.get(&destination_hash).copied().unwrap_or_default();
                        jobs.spawn(async move {
                            let result = tokio::time::timeout(Duration::from_secs(45), service.rrc_connect(&destination_hash, None))
                                .await.map_err(|_| "Timed out after 45 seconds".to_owned())
                                .and_then(|result| result.map_err(|error| error.to_string()));
                            Completion::Hub(destination_hash, version, result)
                        });
                    }
                    UiCommand::BrowserFetch(_) | UiCommand::ClearConversation { .. } | UiCommand::SendFile { .. } | UiCommand::DecideFile { .. } => unreachable!(),
                    UiCommand::OpenConversation { .. } | UiCommand::CloseConversation { .. } | UiCommand::LoadOlder { .. } => unreachable!(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnomadnet_core::{
        app::AppState, db::Database, models::NetworkState, network::NetworkCommand, rrc::RrcCommand,
    };
    use std::sync::Arc;

    fn service() -> (tempfile::TempDir, Arc<AppState>, AppService) {
        let directory = tempfile::tempdir().unwrap();
        let config = AppConfig::from_cli(Cli {
            listen: "127.0.0.1:8080".parse().unwrap(),
            allow_remote: false,
            auth_token_file: None,
            offline: true,
            rns_config: None,
            state_dir: Some(directory.path().into()),
        })
        .unwrap();
        let state = Arc::new(AppState::new(
            config,
            Database::open(std::path::Path::new(":memory:")).unwrap(),
        ));
        (directory, state.clone(), AppService::new(state))
    }

    async fn next_matching(
        updates: &UpdateReceiver,
        predicate: impl Fn(&UiState) -> bool,
    ) -> UiState {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(update) = updates.try_recv() {
                    if predicate(&update) {
                        return update;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("bridge update timed out")
    }

    #[tokio::test]
    async fn clear_history_deletes_only_target_and_publishes_empty_history() {
        let (_directory, core, service) = service();
        let target = "aa".repeat(16);
        let other = "bb".repeat(16);
        for destination in [&target, &other] {
            core.database
                .store_message(rsnomadnet_core::db::NewMessage {
                    destination_hash: destination,
                    source_hash: destination,
                    title: "",
                    content: "old message",
                    timestamp: 1,
                    outbound: false,
                    state: "delivered",
                    delivery_method: "incoming",
                    attempts: 0,
                    next_attempt: 0,
                    last_error: None,
                    message_hash: None,
                })
                .unwrap();
        }
        let (sender, updates) = update_channel();
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let bridge = tokio::spawn(run(
            service,
            UiState::default(),
            sender,
            receiver,
            core.events.subscribe(),
        ));
        commands
            .send(UiCommand::OpenConversation {
                destination_hash: target.clone(),
            })
            .unwrap();
        next_matching(&updates, |state| {
            state
                .conversations
                .iter()
                .any(|row| row.destination_hash == target && !row.messages.is_empty())
        })
        .await;
        let (reply, result) = tokio::sync::oneshot::channel();
        commands
            .send(UiCommand::ClearConversation {
                destination_hash: target.clone(),
                reply,
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), result)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        next_matching(&updates, |state| {
            !state
                .conversations
                .iter()
                .any(|row| row.destination_hash == target)
        })
        .await;
        assert!(core.database.messages(&target).unwrap().is_empty());
        assert_eq!(core.database.messages(&other).unwrap().len(), 1);
        drop(commands);
        tokio::time::timeout(Duration::from_secs(2), bridge)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn file_send_uses_direct_command_and_reports_failure_without_blocking_updates() {
        let (_directory, core, service) = service();
        core.network.write().await.state = NetworkState::Online;
        let mut network_commands = core.network_command_rx.lock().await.take().unwrap();
        let (sender, updates) = update_channel();
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let bridge = tokio::spawn(run(
            service,
            UiState::default(),
            sender,
            receiver,
            core.events.subscribe(),
        ));
        let (reply, result) = tokio::sync::oneshot::channel();
        commands
            .send(UiCommand::SendFile {
                destination_hash: "aa".repeat(16),
                path: PathBuf::from("/tmp/selected.txt"),
                reply,
            })
            .unwrap();
        let NetworkCommand::SendFile {
            destination_hash,
            path,
            response,
        } = network_commands.recv().await.unwrap()
        else {
            panic!("wrong network command")
        };
        assert_eq!(destination_hash, [0xaa; 16]);
        assert_eq!(path, PathBuf::from("/tmp/selected.txt"));
        let mut snapshot = core.network.read().await.clone();
        snapshot.detail = "Still processing events".into();
        core.events
            .send(ServerEvent::NetworkChanged(snapshot))
            .unwrap();
        next_matching(&updates, |state| {
            state
                .network
                .iter()
                .any(|line| line.contains("Still processing events"))
        })
        .await;
        response.send(Err("peer unavailable".into())).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), result)
                .await
                .unwrap()
                .unwrap(),
            Err("peer unavailable".into())
        );
        assert!(core.database.conversations().unwrap().is_empty());
        drop(commands);
        tokio::time::timeout(Duration::from_secs(2), bridge)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn stalled_send_does_not_block_events_and_is_cancelled_with_bridge() {
        let (_directory, core, service) = service();
        core.network.write().await.state = NetworkState::Online;
        let mut network_commands = core.network_command_rx.lock().await.take().unwrap();
        let (sender, updates) = update_channel();
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let bridge = tokio::spawn(run(
            service,
            UiState::default(),
            sender,
            receiver,
            core.events.subscribe(),
        ));
        commands
            .send(UiCommand::SendMessage {
                destination_hash: "00".repeat(16),
                content: "draft".into(),
            })
            .unwrap();
        let NetworkCommand::SendMessage { response, .. } = network_commands.recv().await.unwrap()
        else {
            panic!("wrong command")
        };
        let mut snapshot = core.network.read().await.clone();
        snapshot.detail = "Event while send is pending".into();
        core.set_network(snapshot).await;
        next_matching(&updates, |update| {
            update
                .network
                .iter()
                .any(|line| line == "Event while send is pending")
        })
        .await;
        bridge.abort();
        let _ = bridge.await;
        tokio::task::yield_now().await;
        assert!(response.is_closed());
    }

    fn hub(connected: bool) -> RrcHubView {
        RrcHubView {
            destination_hash: "00".repeat(16),
            local_identity: "11".repeat(16),
            name: None,
            nick: None,
            version: None,
            supports_resources: false,
            supports_actions: false,
            supports_direct_notices: false,
            supports_room_state: false,
            supports_user_list: false,
            max_message_bytes: None,
            connected,
            rooms: Vec::new(),
            room_states: Vec::new(),
            detail: if connected {
                "WELCOME"
            } else {
                "Waiting for WELCOME"
            }
            .into(),
        }
    }

    #[tokio::test]
    async fn late_connect_reply_does_not_overwrite_welcome() {
        let (_directory, core, service) = service();
        core.network.write().await.state = NetworkState::Online;
        let mut rrc_commands = core.rrc_command_rx.lock().await.take().unwrap();
        let (sender, updates) = update_channel();
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let bridge = tokio::spawn(run(
            service,
            UiState::default(),
            sender,
            receiver,
            core.events.subscribe(),
        ));
        commands
            .send(UiCommand::ConnectRrc {
                destination_hash: "00".repeat(16),
            })
            .unwrap();
        let RrcCommand::Connect { response, .. } = rrc_commands.recv().await.unwrap() else {
            panic!("wrong command")
        };
        core.events
            .send(ServerEvent::RrcHubChanged(hub(true)))
            .unwrap();
        let key = format!("rrc:{}", "00".repeat(16));
        next_matching(&updates, |update| {
            update
                .directory_views
                .get(&key)
                .is_some_and(|lines| lines.contains(&"WELCOME".into()))
        })
        .await;
        response.send(Ok(hub(false))).unwrap();
        let after_reply =
            next_matching(&updates, |update| update.directory_views.contains_key(&key)).await;
        assert!(after_reply.directory_views[&key].contains(&"State: connected".into()));
        commands
            .send(UiCommand::ConnectRrc {
                destination_hash: "00".repeat(16),
            })
            .unwrap();
        next_matching(&updates, |update| update.directory_views.contains_key(&key)).await;
        assert!(
            rrc_commands.try_recv().is_err(),
            "connected hub must be reused"
        );
        bridge.abort();
        let _ = bridge.await;
    }
}
