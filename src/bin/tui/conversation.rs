use super::*;
use tv::InputLine;

pub(super) const SEND_MESSAGE: Command = Command::custom("rsnomadnet.send_message");
pub(super) const LOAD_OLDER: Command = Command::custom("rsnomadnet.load_older");
pub(super) const CLEAR_HISTORY: Command = Command::custom("rsnomadnet.clear_history");
pub(super) const HELP: tv::help::HelpCtx = tv::help::HelpCtx::custom("nomadnet.conversation");

type SharedText = Rc<RefCell<String>>;
pub(super) type ActiveComposer = Rc<RefCell<Option<(String, SharedText)>>>;

struct ComposerInput {
    input: InputLine,
    content: SharedText,
}

impl ComposerInput {
    fn new(bounds: Rect, content: SharedText) -> Self {
        Self::with_limit(bounds, content, 4096)
    }

    fn with_limit(bounds: Rect, content: SharedText, limit: i32) -> Self {
        Self {
            input: InputLine::with_limit(bounds, limit),
            content,
        }
    }
}

#[delegate(to = input)]
impl View for ComposerInput {
    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }

    fn draw(&mut self, context: &mut DrawCtx) {
        let content = self.content.borrow().clone();
        if self.input.value() != Some(FieldValue::Text(content.clone())) {
            self.input.set_value(FieldValue::Text(content));
        }
        self.input.draw(context);
    }

    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
        if matches!(event, Event::KeyDown(key) if key.key == Key::Enter) {
            context.put_event(Event::Command(SEND_MESSAGE));
            event.clear();
            return;
        }
        let content = self.content.borrow().clone();
        if self.input.value() != Some(FieldValue::Text(content.clone())) {
            self.input.set_value_ctx(FieldValue::Text(content), context);
        }
        self.input.handle_event(event, context);
        if let Some(FieldValue::Text(content)) = self.input.value() {
            *self.content.borrow_mut() = content;
        }
    }
}

struct ConversationHistory {
    list: ListBox,
    state: Shared,
    destination_hash: String,
    seeded: bool,
}

impl ConversationHistory {
    fn new(bounds: Rect, state: Shared, destination_hash: String, scroll_bar: ViewId) -> Self {
        Self {
            list: ListBox::new(bounds, 1, None, Some(scroll_bar)),
            state,
            destination_hash,
            seeded: false,
        }
    }

    fn lines(&self) -> Vec<String> {
        let state = self.state.borrow();
        let mut lines = state
            .conversations
            .iter()
            .find(|conversation| conversation.destination_hash == self.destination_hash)
            .map(|conversation| conversation.messages.clone())
            .filter(|messages| !messages.is_empty())
            .unwrap_or_else(|| vec!["No messages".into()]);
        if let Some(error) = state.send_errors.get(&self.destination_hash) {
            lines.push(format!("[!] {error}"));
        }
        if state.pending_sends.contains_key(&self.destination_hash) {
            lines.push("[~] Waiting for message to be saved…".into());
        }
        lines
    }
}

#[delegate(to = list)]
impl View for ConversationHistory {
    fn apply_scroll_sync(&mut self, h: Option<i32>, v: Option<i32>, ctx: &mut Context) {
        self.list.apply_scroll_sync(h, v, ctx);
        // Finish the deferred scrollbar write-back before the next mouse event.
        ctx.put_event(Event::Nothing);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }

    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
        if matches!(event, Event::KeyDown(key) if key.key == Key::PageUp)
            && self.list.value() == Some(FieldValue::Int(0))
        {
            context.put_event(Event::Command(LOAD_OLDER));
            event.clear();
        }
        let lines = self.lines();
        if !self.seeded || self.list.list() != lines {
            let mut selected = self.list.value();
            let follow = !self.seeded
                || matches!(selected, Some(FieldValue::Int(index))
                if index as usize >= self.list.list().len().saturating_sub(1));
            if lines.len() > self.list.list().len() && lines.ends_with(self.list.list()) {
                if let Some(FieldValue::Int(index)) = &mut selected {
                    *index += (lines.len() - self.list.list().len()) as i32;
                }
            }
            self.seeded = true;
            self.list.new_list(lines.clone(), context);
            tv::widgets::list_viewer::update_steps(&self.list, context);
            if follow && !lines.is_empty() {
                self.list.set_value_ctx(
                    FieldValue::Int(lines.len().saturating_sub(1) as i32),
                    context,
                );
            } else if let Some(selected) = selected {
                self.list.set_value_ctx(selected, context);
            }
        }
        self.list.handle_event(event, context);
    }
}

pub(super) struct ConversationWindow {
    window: Window,
    destination_hash: String,
    content: SharedText,
    active_composer: ActiveComposer,
    shared: Shared,
    input: ViewId,
    seeded: bool,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    clearing: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
}

impl ConversationWindow {
    fn clear_history(&mut self) {
        if self.clearing.is_some() {
            return;
        }
        if self
            .shared
            .borrow()
            .pending_sends
            .contains_key(&self.destination_hash)
        {
            self.shared.borrow_mut().send_errors.insert(
                self.destination_hash.clone(),
                "Cannot clear history while sending a message".into(),
            );
            return;
        }
        let (reply, result) = tokio::sync::oneshot::channel();
        if self
            .commands
            .send(UiCommand::ClearConversation {
                destination_hash: self.destination_hash.clone(),
                reply,
            })
            .is_ok()
        {
            self.clearing = Some(result);
        } else {
            self.shared.borrow_mut().send_errors.insert(
                self.destination_hash.clone(),
                "Clear history: application worker stopped".into(),
            );
        }
    }
}

impl Drop for ConversationWindow {
    fn drop(&mut self) {
        self.shared
            .borrow_mut()
            .drafts
            .insert(self.destination_hash.clone(), self.content.borrow().clone());
    }
}

#[delegate(to = window)]
impl View for ConversationWindow {
    fn get_help_ctx(&self) -> tv::help::HelpCtx {
        HELP
    }
    fn settle_currency(&mut self, ctx: &mut Context) {
        self.window.settle_currency(ctx);
        if !self.seeded {
            self.seeded = true;
            self.window.focus_descendant(self.input, ctx);
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }

    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
        if let Some(result) =
            self.clearing
                .as_mut()
                .and_then(|receiver| match receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                    Err(_) => Some(Err("Application worker stopped".into())),
                })
        {
            self.clearing = None;
            let mut state = self.shared.borrow_mut();
            match result {
                Ok(()) => {
                    state.send_errors.remove(&self.destination_hash);
                }
                Err(error) => {
                    state.send_errors.insert(
                        self.destination_hash.clone(),
                        format!("Clear history: {error}"),
                    );
                }
            }
        }
        let clear = matches!(event, Event::Command(command) if *command == CLEAR_HISTORY)
            || matches!(event, Event::KeyDown(key) if key.modifiers.ctrl && !key.modifiers.alt
                && matches!(key.key, Key::Char('l' | 'L')));
        if clear && self.window.state().state.active {
            self.clear_history();
            event.clear();
        }
        if matches!(event, Event::KeyDown(key) if key.modifiers.ctrl && !key.modifiers.alt && matches!(key.key, Key::Char('a' | 'A')))
        {
            context.put_event(Event::Command(files::SEND));
            event.clear();
        }
        if matches!(event, Event::MouseWheel(_)) && !self.window.state().state.active {
            return;
        }
        if let Some(result) = self
            .shared
            .borrow_mut()
            .send_results
            .remove(&self.destination_hash)
        {
            if result.error.is_none() && self.content.borrow().trim() == result.content {
                self.content.borrow_mut().clear();
            }
        }
        if self.window.state().state.focused {
            *self.active_composer.borrow_mut() =
                Some((self.destination_hash.clone(), self.content.clone()));
        }
        self.window.handle_event(event, context);
    }
}

pub(super) fn window(
    desktop: Rect,
    state: Shared,
    destination_hash: String,
    active_composer: ActiveComposer,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
) -> ConversationWindow {
    let borrowed = state.borrow();
    let conversation = borrowed
        .conversations
        .iter()
        .find(|conversation| conversation.destination_hash == destination_hash);
    let title = conversation
        .map(|conversation| conversation.title.as_str())
        .or_else(|| {
            borrowed
                .directory
                .iter()
                .find(|entry| entry.destination_hash == destination_hash)
                .map(|entry| entry.title.as_str())
        })
        .unwrap_or(&destination_hash)
        .to_owned();
    drop(borrowed);

    let width = 78.min(desktop.b.x - desktop.a.x - 2).max(40);
    let height = 25.min(desktop.b.y - desktop.a.y - 2).max(12);
    let left = desktop.a.x + ((desktop.b.x - desktop.a.x - width) / 2).max(0);
    let top = desktop.a.y + ((desktop.b.y - desktop.a.y - height) / 2).max(0);
    let mut window = Window::new(
        Rect::new(left, top, left + width, top + height),
        Some(format!("LXMF Conversation — {title}")),
        0,
    );
    window.set_palette(WindowPalette::Blue);
    window.set_min_size(tv::Point::new(40, 12));
    window.set_flags(WindowFlags {
        r#move: true,
        grow: true,
        close: true,
        zoom: true,
    });
    let extent = window.state().get_extent();
    let content = Rc::new(RefCell::new(
        state
            .borrow()
            .drafts
            .get(&destination_hash)
            .cloned()
            .unwrap_or_default(),
    ));
    *active_composer.borrow_mut() = Some((destination_hash.clone(), content.clone()));
    let interior = Rect::new(1, 1, extent.b.x - 1, extent.b.y - 1);
    let mut panels = tv::Splitter::rows().joined();
    panels.state_mut().options.first_click = true;
    let width = extent.b.x - 2;
    let height = extent.b.y - 4;
    let mut history_pane = tv::Group::new(Rect::new(0, 0, width, height));
    history_pane.state_mut().options.selectable = true;
    history_pane.state_mut().options.first_click = true;
    let mut history_scroll = ScrollBar::new(Rect::new(width - 1, 0, width, height));
    history_scroll.state_mut().grow_mode = GrowMode {
        lo_x: true,
        hi_x: true,
        hi_y: true,
        ..Default::default()
    };
    let history_scroll = history_pane.insert(Box::new(history_scroll));
    let mut history = ConversationHistory::new(
        Rect::new(0, 0, width - 1, height),
        state.clone(),
        destination_hash.clone(),
        history_scroll,
    );
    history.state_mut().grow_mode = GrowMode {
        hi_x: true,
        hi_y: true,
        ..Default::default()
    };
    history_pane.insert(Box::new(history));
    panels.insert(Box::new(history_pane), tv::Constraints::flex().min(1));
    let input = panels.insert(
        Box::new(ComposerInput::new(
            Rect::new(0, 0, width, 1),
            content.clone(),
        )),
        tv::Constraints::fixed(1),
    );
    panels.change_bounds(interior);
    window.insert_child(Box::new(panels));
    ConversationWindow {
        window,
        destination_hash,
        content,
        active_composer,
        shared: state,
        input,
        seeded: false,
        commands,
        clearing: None,
    }
}

pub(super) fn handle_command(
    command: Command,
    state: &Shared,
    active_composer: &ActiveComposer,
    commands: &tokio::sync::mpsc::UnboundedSender<UiCommand>,
) {
    if command == LOAD_OLDER {
        if let Some((destination_hash, _)) = active_composer.borrow().as_ref() {
            let _ = commands.send(UiCommand::LoadOlder {
                destination_hash: destination_hash.clone(),
            });
        }
    } else if command == SEND_MESSAGE {
        let active = active_composer.borrow().clone();
        if let Some((destination_hash, content)) = active {
            let message = content.borrow().trim().to_owned();
            if !message.is_empty() && !state.borrow().pending_sends.contains_key(&destination_hash)
            {
                let sent = commands.send(UiCommand::SendMessage {
                    destination_hash: destination_hash.clone(),
                    content: message.clone(),
                });
                if sent.is_ok() {
                    state
                        .borrow_mut()
                        .pending_sends
                        .insert(destination_hash, message);
                } else {
                    state
                        .borrow_mut()
                        .send_errors
                        .insert(destination_hash, "Application worker stopped".into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_context;

    #[test]
    fn ctrl_l_clears_only_this_peer_and_reports_failure_without_losing_draft() {
        let state = Rc::new(RefCell::new(UiState::default()));
        state.borrow_mut().selected_destination_hash = Some("another-peer".into());
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut view = window(
            Rect::new(0, 0, 100, 30),
            state.clone(),
            "aa".into(),
            Rc::new(RefCell::new(None)),
            commands,
        );
        view.state_mut().state.active = true;
        *view.content.borrow_mut() = "draft".into();
        with_context(|ctx| {
            let mut event = Event::KeyDown(window_key(Key::Char('l'), true, false, false));
            view.handle_event(&mut event, ctx);
            assert!(matches!(event, Event::Nothing));
            let UiCommand::ClearConversation {
                destination_hash,
                reply,
            } = receiver.try_recv().unwrap()
            else {
                panic!("wrong command");
            };
            assert_eq!(destination_hash, "aa");
            view.handle_event(&mut Event::Command(CLEAR_HISTORY), ctx);
            assert!(
                receiver.try_recv().is_err(),
                "duplicate clear while pending"
            );
            reply.send(Err("awaiting delivery".into())).unwrap();
            view.handle_event(&mut Event::Nothing, ctx);
            assert!(state.borrow().send_errors["aa"].contains("awaiting delivery"));
            assert_eq!(&*view.content.borrow(), "draft");
            view.handle_event(&mut Event::Command(CLEAR_HISTORY), ctx);
            let UiCommand::ClearConversation { reply, .. } = receiver.try_recv().unwrap() else {
                panic!("wrong command")
            };
            reply.send(Ok(())).unwrap();
            view.handle_event(&mut Event::Nothing, ctx);
            assert!(!state.borrow().send_errors.contains_key("aa"));
            assert_eq!(&*view.content.borrow(), "draft");
        });
    }

    #[test]
    fn panels_resize_with_window_and_keep_composer_and_scrolling_working() {
        let (backend, screen) = tv::HeadlessBackend::new(100, 30);
        let state = Rc::new(RefCell::new(UiState {
            conversations: vec![ConversationRow {
                destination_hash: "aa".into(),
                title: "Alice".into(),
                label: "Alice".into(),
                messages: (0..60).map(|n| format!("Message {n:02}")).collect(),
            }],
            ..UiState::default()
        }));
        let (_sender, updates) = update_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);
        let bounds = app.program.desktop_rect();
        let view = window(
            bounds,
            state.clone(),
            "aa".into(),
            Rc::new(RefCell::new(None)),
            tokio::sync::mpsc::unbounded_channel().0,
        );
        let content = view.content.clone();
        app.program
            .desktop_insert(Box::new(layout::TrackedWindow::new(
                Box::new(view),
                "lxmf:aa".into(),
                app.layout.clone(),
                None,
                bounds,
            )));
        screen.push_paste("draft");
        for _ in 0..40 {
            app.program.pump_once();
        }
        assert_eq!(&*content.borrow(), "draft");
        assert!(screen.snapshot().contains("Ctrl-L"));
        assert!(screen.snapshot().contains("Clear history"));
        let check = |app: &TuiApp| {
            let store = app.layout.borrow();
            let w = store.windows.iter().find(|w| w.key == "lxmf:aa").unwrap();
            let buffer = screen.buffer();
            let x = w.x as u16;
            // Desktop's origin is one row below the menu.
            let y = (w.y + w.height - 2) as u16;
            assert_eq!(buffer.get(x, y).symbol(), "╟");
            assert_eq!(buffer.get(x + w.width as u16 - 1, y).symbol(), "╢");
            for column in 1..w.width as u16 - 1 {
                assert_eq!(buffer.get(x + column, y).symbol(), "─");
            }
            assert!(matches!(
                buffer.get(x + w.width as u16 - 2, y - 1).symbol(),
                "▼" | "↓"
            ));
            let input: String = (1..w.width as u16 - 1)
                .map(|column| buffer.get(x + column, y + 1).symbol())
                .collect();
            assert!(input.contains("draft"), "{input}");
            // Only the frame follows the input; there is no unused blank row.
            assert_eq!(buffer.get(x + 2, y + 2).symbol(), "═");
        };
        check(&app);
        screen.push_event(Event::Command(Command::RESIZE));
        screen.push_key(
            Key::Left,
            KeyModifiers {
                shift: true,
                ..Default::default()
            },
        );
        screen.push_key(
            Key::Up,
            KeyModifiers {
                shift: true,
                ..Default::default()
            },
        );
        screen.push_key(Key::Enter, KeyModifiers::default());
        for _ in 0..30 {
            app.program.pump_once();
        }
        check(&app);
        screen.push_event(Event::Command(Command::ZOOM));
        for _ in 0..30 {
            app.program.pump_once();
        }
        check(&app);
        for (width, height) in [(120, 40), (80, 24), (100, 30)] {
            screen.resize(width, height);
            for _ in 0..30 {
                app.program.pump_once();
            }
            check(&app);
        }
        screen.push_key(Key::Tab, KeyModifiers::default());
        for _ in 0..12 {
            screen.push_event(Event::MouseWheel(tv::event::MouseEvent {
                wheel: tv::event::MouseWheel::Up,
                ..Default::default()
            }));
        }
        for _ in 0..100 {
            app.program.pump_once();
        }
        assert!(!screen.snapshot().contains("Message 59"));
        assert_eq!(&*content.borrow(), "draft");
        screen.push_key(Key::Tab, KeyModifiers::default());
        screen.push_key(Key::End, KeyModifiers::default());
        screen.push_paste("!");
        for _ in 0..30 {
            app.program.pump_once();
        }
        assert_eq!(&*content.borrow(), "draft!");
        screen.push_event(Event::Command(Command::CLOSE));
        for _ in 0..30 {
            app.program.pump_once();
        }
        assert_eq!(
            state.borrow().drafts.get("aa").map(String::as_str),
            Some("draft!")
        );
        assert!(!screen.snapshot().contains("Clear history"));
    }

    #[test]
    fn lxmf_keeps_reading_position_and_only_follows_at_the_end() {
        let state = Rc::new(RefCell::new(UiState {
            conversations: vec![ConversationRow {
                destination_hash: "aa".into(),
                title: "Alice".into(),
                label: "Alice".into(),
                messages: (0..50).map(|n| format!("Message {n}")).collect(),
            }],
            ..UiState::default()
        }));
        let mut window = Dialog::new(Rect::new(0, 0, 40, 12), None);
        let scroll = window.insert_child(Box::new(ScrollBar::new(Rect::new(38, 1, 39, 10))));
        let mut history =
            ConversationHistory::new(Rect::new(1, 1, 38, 10), state.clone(), "aa".into(), scroll);
        with_context(|ctx| {
            let mut refresh = Event::Broadcast {
                command: REFRESH,
                source: None,
            };
            history.handle_event(&mut refresh, ctx);
            assert_eq!(history.list.value(), Some(FieldValue::Int(49)));
            history.list.set_value_ctx(FieldValue::Int(10), ctx);
            history.handle_event(&mut refresh, ctx);
            assert_eq!(history.list.value(), Some(FieldValue::Int(10)));
            state.borrow_mut().conversations[0]
                .messages
                .push("New message".into());
            history.handle_event(&mut refresh, ctx);
            assert_eq!(history.list.value(), Some(FieldValue::Int(10)));
            history.list.set_value_ctx(FieldValue::Int(50), ctx);
            state.borrow_mut().conversations[0]
                .messages
                .push("Another new message".into());
            history.handle_event(&mut refresh, ctx);
            assert_eq!(history.list.value(), Some(FieldValue::Int(51)));
            history.list.set_value_ctx(FieldValue::Int(0), ctx);
            state.borrow_mut().conversations[0]
                .messages
                .insert(0, "Older message".into());
            history.handle_event(&mut refresh, ctx);
            assert_eq!(history.list.value(), Some(FieldValue::Int(1)));
        });
    }

    #[test]
    fn send_acknowledgement_keeps_failed_or_newly_edited_text() {
        let state = Rc::new(RefCell::new(UiState::default()));
        let mut window = window(
            Rect::new(0, 0, 100, 30),
            state.clone(),
            "aa".into(),
            Rc::new(RefCell::new(None)),
            tokio::sync::mpsc::unbounded_channel().0,
        );
        *window.content.borrow_mut() = "draft".into();
        with_context(|ctx| {
            for error in [Some("offline".into()), None] {
                state.borrow_mut().send_results.insert(
                    "aa".into(),
                    SendResult {
                        content: "draft".into(),
                        error: error.clone(),
                    },
                );
                window.handle_event(
                    &mut Event::Broadcast {
                        command: REFRESH,
                        source: None,
                    },
                    ctx,
                );
                assert_eq!(
                    &*window.content.borrow(),
                    if error.is_some() { "draft" } else { "" }
                );
            }
            *window.content.borrow_mut() = "edited while sending".into();
            state.borrow_mut().send_results.insert(
                "aa".into(),
                SendResult {
                    content: "draft".into(),
                    error: None,
                },
            );
            window.handle_event(
                &mut Event::Broadcast {
                    command: REFRESH,
                    source: None,
                },
                ctx,
            );
            assert_eq!(&*window.content.borrow(), "edited while sending");
        });
    }

    #[test]
    fn conversation_and_composer_bind_selected_peer() {
        let destination_hash = "aabbccddeeff00112233445566778899";
        let state = Rc::new(RefCell::new(UiState {
            conversations: vec![ConversationRow {
                destination_hash: destination_hash.into(),
                title: "Alice".into(),
                label: "Alice".into(),
                messages: vec!["[<] hello".into()],
            }],
            selected_destination_hash: Some(destination_hash.into()),
            ..UiState::default()
        }));

        assert_eq!(
            selected_destination(&state.borrow()).as_deref(),
            Some(destination_hash)
        );
        let active_composer = Rc::new(RefCell::new(None));
        let conversation = window(
            Rect::new(0, 0, 100, 28),
            state,
            destination_hash.into(),
            active_composer.clone(),
            tokio::sync::mpsc::unbounded_channel().0,
        );
        assert!(conversation.window.flags().grow);
        assert_eq!(conversation.window.palette(), WindowPalette::Blue);
        assert!(conversation.window.flags().zoom);
        assert!(conversation.content.borrow().is_empty());
        assert_eq!(
            active_composer
                .borrow()
                .as_ref()
                .map(|(destination, _)| destination.as_str()),
            Some(destination_hash)
        );
    }
}
