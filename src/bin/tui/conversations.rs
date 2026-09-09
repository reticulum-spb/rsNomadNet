use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(hash: &str, name: Option<&str>) -> ConversationSummary {
        ConversationSummary {
            destination_hash: hash.into(),
            display_name: name.map(str::to_owned),
            last_message: Some("hello".into()),
            last_activity: Some(1),
            unread: 2,
        }
    }

    fn announce(hash: &str, name: &str) -> DirectoryEntry {
        DirectoryEntry {
            destination_hash: hash.into(),
            display_name: Some(name.into()),
            identity_hash: None,
            delivery_hash: None,
            kind: "peer".into(),
            hops: 1,
            last_seen: 1,
            active: true,
        }
    }

    #[test]
    fn names_use_full_directory_and_keep_contact_names_and_hash_fallback() {
        let mut directory: Vec<_> = (0..MAX_DIRECTORY_ROWS)
            .map(|n| announce(&format!("{n}"), "Other"))
            .collect();
        directory.push(announce("aa", " Alice "));
        directory.push(announce("bb", "Bob"));
        directory.push(announce("cc", "  "));
        let rows = rows_with_directory(
            vec![
                summary("aa", None),
                summary("bb", Some("My friend")),
                summary("cc", None),
                summary("dd", None),
            ],
            directory,
        );
        assert_eq!(rows[0].title, "Alice");
        assert_eq!(rows[0].label, "[2] Alice  hello");
        assert_eq!(rows[0].destination_hash, "aa");
        assert_eq!(rows[1].title, "My friend");
        assert_eq!(rows[2].title, "cc");
        assert_eq!(rows[3].title, "dd");
    }

    #[test]
    fn delivery_address_is_used_only_when_no_peer_name_is_known() {
        let mut node = announce("node", "Node owner");
        node.kind = "node".into();
        node.delivery_hash = Some("aa".into());
        assert_eq!(
            rows_with_directory(vec![summary("aa", None)], vec![node.clone()])[0].title,
            "Node owner"
        );
        assert_eq!(
            rows_with_directory(
                vec![summary("aa", None)],
                vec![node, announce("aa", "Alice")]
            )[0]
            .title,
            "Alice"
        );
    }

    #[test]
    fn renaming_and_reordering_preserve_selected_chat_identity() {
        let state = Rc::new(RefCell::new(UiState {
            conversations: rows_with_directory(
                vec![summary("aa", None), summary("bb", None)],
                vec![],
            ),
            ..Default::default()
        }));
        let mut list = StateList::new(Rect::new(0, 0, 40, 10), state.clone(), Pane::Conversations);
        crate::tests::with_context(|ctx| {
            let refresh = || Event::Broadcast {
                command: REFRESH,
                source: None,
            };
            list.handle_event(&mut refresh(), ctx);
            list.list.set_value_ctx(FieldValue::Int(1), ctx);
            state.borrow_mut().conversations = rows_with_directory(
                vec![summary("bb", None), summary("aa", None)],
                vec![announce("bb", "Bob")],
            );
            list.handle_event(&mut refresh(), ctx);
            assert_eq!(list.focused_destination().as_deref(), Some("bb"));
            assert_eq!(list.list.value(), Some(FieldValue::Int(0)));
            assert!(list.list.list()[0].contains("Bob"));
        });
    }
}

pub(super) fn window(mut window: Window, state: Shared) -> Window {
    let extent = window.state().get_extent();
    window.insert_child(Box::new(StateList::new(
        Rect::new(1, 1, extent.b.x - 1, extent.b.y - 1),
        state,
        Pane::Conversations,
    )));
    window
}

pub(super) fn rows(
    service: &AppService,
    conversations: Vec<ConversationSummary>,
) -> Vec<ConversationRow> {
    rows_with_directory(conversations, service.directory().unwrap_or_default())
}

fn rows_with_directory(
    conversations: Vec<ConversationSummary>,
    directory: Vec<DirectoryEntry>,
) -> Vec<ConversationRow> {
    // Use the complete directory, not the 200-row TUI Directory viewport.
    // Peer announces take precedence over names advertised by other services.
    let mut names = HashMap::new();
    for entry in directory.iter().filter(|entry| entry.kind == "peer") {
        if let Some(name) = entry
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            names.entry(entry.destination_hash.as_str()).or_insert(name);
        }
    }
    for entry in &directory {
        if let (Some(delivery), Some(name)) = (
            entry.delivery_hash.as_deref(),
            entry
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty()),
        ) {
            names.entry(delivery).or_insert(name);
        }
    }
    conversations
        .into_iter()
        .map(|conversation| {
            let title = conversation
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .or_else(|| names.get(conversation.destination_hash.as_str()).copied())
                .unwrap_or(&conversation.destination_hash)
                .to_owned();
            ConversationRow {
                label: format!(
                    "{}{}  {}",
                    if conversation.unread > 0 {
                        format!("[{}] ", conversation.unread)
                    } else {
                        String::new()
                    },
                    title,
                    conversation.last_message.as_deref().unwrap_or("")
                ),
                destination_hash: conversation.destination_hash,
                title,
                messages: Vec::new(),
            }
        })
        .collect()
}
