use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use clap::Parser;
use rsnomadnet_core::Runtime;
use rsnomadnet_core::config::{AppConfig, Cli};
use rsnomadnet_core::models::{ConversationSummary, DirectoryEntry, NetworkSnapshot};
use rsnomadnet_core::service::{AppService, SendMessage};
use tv::{
    Backend, Button, ButtonFlags, Command, Context, CrosstermBackend, Desktop, Dialog, DrawCtx,
    Event, FieldValue, GrowMode, InputLine, Key, KeyEvent, ListBox, Menu, MenuBar, Program, Rect,
    ScrollBar, StaticText, StatusDef, StatusLine, SystemClock, Theme, View, ViewId, ViewState,
    Window, WindowFlags, alt, delegate,
};
use tvision_rs as tv;

const REFRESH: Command = Command::custom("rsnomadnet.refresh");
const OPEN_CONVERSATION: Command = Command::custom("rsnomadnet.open_conversation");
const COMPOSE_MESSAGE: Command = Command::custom("rsnomadnet.compose_message");
const SEND_MESSAGE: Command = Command::custom("rsnomadnet.send_message");

#[derive(Debug, Parser)]
#[command(version, about = "Terminal frontend for rsNomadNet")]
struct TuiCli {
    #[arg(long)]
    offline: bool,
    #[arg(long)]
    rns_config: Option<PathBuf>,
    #[arg(long)]
    state_dir: Option<PathBuf>,
}

#[derive(Default)]
struct UiState {
    network: Vec<String>,
    conversations: Vec<ConversationRow>,
    directory: Vec<DirectoryRow>,
    selected_destination_hash: Option<String>,
}

struct ConversationRow {
    destination_hash: String,
    title: String,
    label: String,
    messages: Vec<String>,
}

struct DirectoryRow {
    destination_hash: String,
    title: String,
    label: String,
}

enum UiCommand {
    SendMessage {
        destination_hash: String,
        content: String,
    },
}

type Shared = Rc<RefCell<UiState>>;
type SharedText = Rc<RefCell<String>>;
type ActiveComposer = Rc<RefCell<Option<(String, SharedText)>>>;

#[derive(Clone, Copy)]
enum Pane {
    Network,
    Conversations,
    Directory,
}

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
        self.state
            .borrow()
            .conversations
            .iter()
            .find(|conversation| conversation.destination_hash == self.destination_hash)
            .map(|conversation| conversation.messages.clone())
            .filter(|messages| !messages.is_empty())
            .unwrap_or_else(|| vec!["No messages".into()])
    }
}

#[delegate(to = list)]
impl View for ConversationHistory {
    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }

    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
        let refresh = matches!(
            event,
            Event::Broadcast { command, .. } if *command == REFRESH
        );
        let lines = self.lines();
        if !self.seeded || refresh || self.list.list() != lines {
            self.seeded = true;
            self.list.new_list(lines.clone(), context);
            tv::widgets::list_viewer::update_steps(&self.list, context);
            if !lines.is_empty() {
                self.list.set_value_ctx(
                    FieldValue::Int(lines.len().saturating_sub(1) as i32),
                    context,
                );
            }
        }
        self.list.handle_event(event, context);
    }
}

struct ConversationWindow {
    window: Dialog,
    destination_hash: String,
    content: SharedText,
    active_composer: ActiveComposer,
}

#[delegate(to = window)]
impl View for ConversationWindow {
    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }

    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
        if self.window.state().state.focused {
            *self.active_composer.borrow_mut() =
                Some((self.destination_hash.clone(), self.content.clone()));
        }
        self.window.handle_event(event, context);
    }
}

struct StateList {
    list: ListBox,
    state: Shared,
    pane: Pane,
    seeded: bool,
}

impl StateList {
    fn new(bounds: Rect, state: Shared, pane: Pane) -> Self {
        Self {
            list: ListBox::new(bounds, 1, None, None),
            state,
            pane,
            seeded: false,
        }
    }

    fn lines(&self) -> Vec<String> {
        let state = self.state.borrow();
        match self.pane {
            Pane::Network => state.network.clone(),
            Pane::Conversations => state
                .conversations
                .iter()
                .map(|conversation| conversation.label.clone())
                .collect(),
            Pane::Directory => state
                .directory
                .iter()
                .map(|entry| entry.label.clone())
                .collect(),
        }
    }

    fn focused_destination(&self) -> Option<String> {
        let FieldValue::Int(index) = self.list.value()? else {
            return None;
        };
        let state = self.state.borrow();
        match self.pane {
            Pane::Conversations => state
                .conversations
                .get(index as usize)
                .map(|conversation| conversation.destination_hash.clone()),
            Pane::Directory => state
                .directory
                .get(index as usize)
                .map(|entry| entry.destination_hash.clone()),
            Pane::Network => None,
        }
    }

    fn selected_index(&self) -> Option<i32> {
        let state = self.state.borrow();
        let selected = state.selected_destination_hash.as_deref()?;
        match self.pane {
            Pane::Conversations => state
                .conversations
                .iter()
                .position(|row| row.destination_hash == selected),
            Pane::Directory => state
                .directory
                .iter()
                .position(|row| row.destination_hash == selected),
            Pane::Network => None,
        }
        .map(|index| index as i32)
    }
}

#[delegate(to = list)]
impl View for StateList {
    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }

    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
        let open = matches!(event, Event::KeyDown(key) if key.key == Key::Enter)
            && matches!(self.pane, Pane::Conversations);
        let refresh = matches!(
            event,
            Event::Broadcast { command, .. } if *command == REFRESH
        );
        if !self.seeded || refresh {
            self.seeded = true;
            let lines = self.lines();
            self.list.new_list(lines, context);
            if let Some(index) = self.selected_index() {
                self.list.set_value_ctx(FieldValue::Int(index), context);
            }
        }
        self.list.handle_event(event, context);
        // A refresh rebuilds every list. Do not let an unfocused pane's
        // temporary first row replace the peer selected in another pane.
        if !refresh
            && self.list.state().state.focused
            && let Some(destination_hash) = self.focused_destination()
        {
            self.state.borrow_mut().selected_destination_hash = Some(destination_hash);
        }
        if open && self.state.borrow().selected_destination_hash.is_some() {
            context.put_event(Event::Command(OPEN_CONVERSATION));
            event.clear();
        }
    }
}

struct PumpView {
    state: ViewState,
    shared: Shared,
    updates: mpsc::Receiver<UiState>,
    armed: bool,
}

impl PumpView {
    fn new(shared: Shared, updates: mpsc::Receiver<UiState>) -> Self {
        let mut state = ViewState::new(Rect::new(0, 0, 0, 0));
        state.options.pre_process = true;
        Self {
            state,
            shared,
            updates,
            armed: false,
        }
    }
}

impl View for PumpView {
    fn state(&self) -> &ViewState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.state
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }

    fn draw(&mut self, _context: &mut DrawCtx) {}

    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
        if !self.armed {
            self.armed = true;
            context.set_timer(Duration::from_millis(100), Some(Duration::from_millis(100)));
        }
        if !matches!(event, Event::Timer(_)) {
            return;
        }
        let mut latest = None;
        while let Ok(update) = self.updates.try_recv() {
            latest = Some(update);
        }
        if let Some(update) = latest {
            let selected = self.shared.borrow().selected_destination_hash.clone();
            *self.shared.borrow_mut() = UiState {
                selected_destination_hash: selected,
                ..update
            };
            context.broadcast(REFRESH, None);
        }
    }
}

struct TuiApp {
    program: Program,
}

impl TuiApp {
    fn new(backend: Box<dyn Backend>, state: Shared, updates: mpsc::Receiver<UiState>) -> Self {
        let program = Program::new(
            backend,
            Box::new(SystemClock::new()),
            Theme::classic_blue(),
            move |bounds| Self::desktop(bounds, state.clone(), updates),
            Self::status_line,
            Self::menu_bar,
        );
        Self { program }
    }

    fn desktop(
        mut bounds: Rect,
        state: Shared,
        updates: mpsc::Receiver<UiState>,
    ) -> Option<Box<dyn View>> {
        bounds.a.y += 1;
        bounds.b.y -= 1;
        let mut desktop = Desktop::new(bounds, |rect| Some(Desktop::init_background(rect)));
        let width = bounds.b.x - bounds.a.x;
        let height = bounds.b.y - bounds.a.y;
        let left = bounds.a.x + 1;
        let top = bounds.a.y + 1;
        let middle = left + width / 2;
        let bottom = bounds.b.y - 1;

        let mut conversations = Window::new(
            Rect::new(left, top, middle, bottom),
            Some("LXMF Conversations".into()),
            1,
        );
        let extent = conversations.state().get_extent();
        conversations.insert_child(Box::new(StateList::new(
            Rect::new(1, 1, extent.b.x - 1, extent.b.y - 1),
            state.clone(),
            Pane::Conversations,
        )));
        // Keep the update pump in the initially focused window so it receives
        // the first event and arms its periodic timer deterministically.
        conversations.insert_child(Box::new(PumpView::new(state.clone(), updates)));

        let split = top + height / 2;
        let mut network = Window::new(
            Rect::new(middle, top, bounds.b.x - 1, split),
            Some("Network".into()),
            2,
        );
        let extent = network.state().get_extent();
        network.insert_child(Box::new(StateList::new(
            Rect::new(1, 1, extent.b.x - 1, extent.b.y - 1),
            state.clone(),
            Pane::Network,
        )));
        let mut directory = Window::new(
            Rect::new(middle, split, bounds.b.x - 1, bottom),
            Some("Directory".into()),
            3,
        );
        let extent = directory.state().get_extent();
        directory.insert_child(Box::new(StateList::new(
            Rect::new(1, 1, extent.b.x - 1, extent.b.y - 1),
            state,
            Pane::Directory,
        )));

        desktop.insert_view(Box::new(network));
        desktop.insert_view(Box::new(directory));
        // Insert conversations last so it is the initially focused window.
        desktop.insert_view(Box::new(conversations));
        Some(Box::new(desktop))
    }

    fn status_line(mut bounds: Rect) -> Option<Box<dyn View>> {
        bounds.a.y = bounds.b.y - 1;
        let definitions = StatusDef::list()
            .def_all(|definition| {
                definition
                    .item(
                        "~F4~ Conversation",
                        KeyEvent::from(Key::F(4)),
                        COMPOSE_MESSAGE,
                    )
                    .item("~Alt-X~ Exit", alt('x'), Command::QUIT)
            })
            .build();
        Some(Box::new(StatusLine::new(bounds, definitions)))
    }

    fn menu_bar(mut bounds: Rect) -> Option<Box<dyn View>> {
        bounds.b.y = bounds.a.y + 1;
        let menu = Menu::builder()
            .submenu("~M~essage", alt('m'), |menu| {
                menu.command_key(
                    "~C~onversation",
                    COMPOSE_MESSAGE,
                    KeyEvent::from(Key::F(4)),
                    "F4",
                )
            })
            .submenu("~F~ile", alt('f'), |menu| {
                menu.command_key("E~x~it", Command::QUIT, alt('x'), "Alt-X")
            })
            .build();
        Some(Box::new(MenuBar::new(bounds, menu)))
    }

    fn run(
        &mut self,
        state: Shared,
        commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    ) -> Command {
        let active_composer: ActiveComposer = Rc::new(RefCell::new(None));
        self.program.run_app(move |program, command| {
            if command == SEND_MESSAGE {
                let active = active_composer.borrow().clone();
                if let Some((destination_hash, content)) = active {
                    let message = content.borrow().trim().to_owned();
                    if !message.is_empty() {
                        let _ = commands.send(UiCommand::SendMessage {
                            destination_hash: destination_hash.clone(),
                            content: message.clone(),
                        });
                        content.borrow_mut().clear();
                    }
                }
            } else if matches!(command, OPEN_CONVERSATION | COMPOSE_MESSAGE) {
                let Some(destination_hash) = selected_destination(&state.borrow()) else {
                    program.exec_view(Box::new(message_dialog(
                        "LXMF conversation",
                        "Select an LXMF peer in Conversations or Directory first.",
                    )));
                    return;
                };
                let bounds = program.desktop_rect();
                program.desktop_insert(Box::new(conversation_window(
                    bounds,
                    state.clone(),
                    destination_hash,
                    active_composer.clone(),
                )));
            }
        })
    }
}

fn message_dialog(title: &str, message: &str) -> Dialog {
    let mut dialog = Dialog::new(Rect::new(0, 0, 54, 10), Some(title.into()));
    dialog.state_mut().options.center_x = true;
    dialog.state_mut().options.center_y = true;
    dialog.insert_child(Box::new(StaticText::new(Rect::new(3, 2, 51, 6), message)));
    dialog.insert_child(Box::new(Button::new(
        Rect::new(21, 6, 33, 8),
        "~O~K",
        Command::OK,
        ButtonFlags {
            default: true,
            ..ButtonFlags::default()
        },
    )));
    dialog
}

fn selected_destination(state: &UiState) -> Option<String> {
    state.selected_destination_hash.clone().or_else(|| {
        state
            .conversations
            .first()
            .map(|conversation| conversation.destination_hash.clone())
    })
}

fn conversation_window(
    desktop: Rect,
    state: Shared,
    destination_hash: String,
    active_composer: ActiveComposer,
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
    // A Dialog inserted directly into the Desktop remains non-modal while
    // retaining the gray dialog palette.
    let mut window = Dialog::new(
        Rect::new(left, top, left + width, top + height),
        Some(format!("LXMF Conversation — {title}")),
    );
    window.set_flags(WindowFlags {
        r#move: true,
        grow: true,
        close: true,
        zoom: true,
    });
    let extent = window.state().get_extent();
    let content = Rc::new(RefCell::new(String::new()));
    *active_composer.borrow_mut() = Some((destination_hash.clone(), content.clone()));
    let mut history_scroll =
        ScrollBar::new(Rect::new(extent.b.x - 2, 1, extent.b.x - 1, extent.b.y - 2));
    history_scroll.state_mut().grow_mode = GrowMode {
        lo_x: true,
        hi_x: true,
        hi_y: true,
        ..Default::default()
    };
    let history_scroll = window.insert_child(Box::new(history_scroll));
    let mut history = ConversationHistory::new(
        Rect::new(1, 1, extent.b.x - 2, extent.b.y - 2),
        state.clone(),
        destination_hash.clone(),
        history_scroll,
    );
    history.state_mut().grow_mode = GrowMode {
        hi_x: true,
        hi_y: true,
        ..Default::default()
    };
    window.insert_child(Box::new(history));
    // The last selectable child becomes current when the window is built.
    let mut input = ComposerInput::new(
        Rect::new(1, extent.b.y - 2, extent.b.x - 1, extent.b.y - 1),
        content.clone(),
    );
    input.state_mut().grow_mode = GrowMode {
        hi_x: true,
        lo_y: true,
        hi_y: true,
        ..Default::default()
    };
    window.insert_child(Box::new(input));
    ConversationWindow {
        window,
        destination_hash,
        content,
        active_composer,
    }
}

fn message_line(outbound: bool, state: &str, content: &str) -> String {
    let content = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let marker = if !outbound {
        '<'
    } else {
        match state {
            "delivered" | "stored_on_node" => '>',
            "failed" => '!',
            _ => '~',
        }
    };
    format!("[{marker}] {content}")
}

fn append_send_error(state: &mut UiState, destination_hash: &str, error: &str) {
    let message = format!("[!] Send failed: {error}");
    if let Some(conversation) = state
        .conversations
        .iter_mut()
        .find(|conversation| conversation.destination_hash == destination_hash)
    {
        conversation.messages.push(message);
        return;
    }
    let title = state
        .directory
        .iter()
        .find(|entry| entry.destination_hash == destination_hash)
        .map(|entry| entry.title.clone())
        .unwrap_or_else(|| destination_hash.to_owned());
    state.conversations.push(ConversationRow {
        destination_hash: destination_hash.to_owned(),
        title: title.clone(),
        label: title,
        messages: vec![message],
    });
}

async fn snapshot(service: &AppService) -> UiState {
    let network = service.network_snapshot().await;
    let conversations = service.conversations().unwrap_or_default();
    let directory = service.directory().unwrap_or_default();
    UiState {
        network: network_lines(network),
        conversations: conversation_rows(service, conversations),
        directory: directory_lines(directory),
        selected_destination_hash: None,
    }
}

fn network_lines(network: NetworkSnapshot) -> Vec<String> {
    let mut lines = vec![
        format!("State: {:?}", network.state),
        network.detail,
        format!(
            "Destination: {}",
            network.destination_hash.as_deref().unwrap_or("-")
        ),
    ];
    lines.extend(network.interfaces.into_iter().map(|interface| {
        format!(
            "{}  {}  RX {}  TX {}",
            if interface.online { "+" } else { "-" },
            interface.name,
            interface.rx_bytes,
            interface.tx_bytes
        )
    }));
    lines
}

fn conversation_rows(
    service: &AppService,
    conversations: Vec<ConversationSummary>,
) -> Vec<ConversationRow> {
    conversations
        .into_iter()
        .map(|conversation| {
            let messages = service
                .messages(&conversation.destination_hash, None)
                .unwrap_or_default()
                .into_iter()
                .map(|message| message_line(message.outbound, &message.state, &message.content))
                .collect();
            ConversationRow {
                destination_hash: conversation.destination_hash.clone(),
                title: conversation
                    .display_name
                    .clone()
                    .unwrap_or_else(|| conversation.destination_hash.clone()),
                label: format!(
                    "{}{}  {}",
                    if conversation.unread > 0 {
                        format!("[{}] ", conversation.unread)
                    } else {
                        String::new()
                    },
                    conversation
                        .display_name
                        .as_deref()
                        .unwrap_or(&conversation.destination_hash),
                    conversation.last_message.as_deref().unwrap_or("")
                ),
                messages,
            }
        })
        .collect()
}

fn directory_lines(entries: Vec<DirectoryEntry>) -> Vec<DirectoryRow> {
    entries
        .into_iter()
        .map(|entry| DirectoryRow {
            destination_hash: entry.destination_hash.clone(),
            title: entry
                .display_name
                .clone()
                .unwrap_or_else(|| entry.destination_hash.clone()),
            label: format!(
                "{} {:<18} {}",
                if entry.active { "+" } else { "-" },
                entry.kind,
                entry
                    .display_name
                    .as_deref()
                    .unwrap_or(&entry.destination_hash)
            ),
        })
        .collect()
}

fn main() -> anyhow::Result<()> {
    let cli = TuiCli::parse();
    let config = AppConfig::from_cli(Cli {
        listen: "127.0.0.1:8080".parse().expect("constant socket address"),
        allow_remote: false,
        auth_token_file: None,
        offline: cli.offline,
        rns_config: cli.rns_config,
        state_dir: cli.state_dir,
    })?;
    let tokio = tokio::runtime::Runtime::new()?;
    let core = {
        let _guard = tokio.enter();
        Runtime::start(config)?
    };
    let service = core.service();
    let initial = tokio.block_on(async {
        // Give the freshly spawned network task a chance to publish Starting
        // before the first frontend snapshot is captured.
        tokio::task::yield_now().await;
        snapshot(&service).await
    });
    let shared = Rc::new(RefCell::new(initial));
    let (sender, updates) = mpsc::channel();
    let (command_sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
    let bridge = tokio.spawn(async move {
        let mut events = service.subscribe();
        let mut refresh = tokio::time::interval(Duration::from_secs(2));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                event = events.recv() => {
                    match event {
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            if sender.send(snapshot(&service).await).is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = refresh.tick() => {
                    if sender.send(snapshot(&service).await).is_err() {
                        break;
                    }
                }
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    match command {
                        UiCommand::SendMessage { destination_hash, content } => {
                            let failed_destination = destination_hash.clone();
                            let result = service.send_message(SendMessage {
                                destination_hash,
                                title: String::new(),
                                content,
                                delivery_method: "automatic".into(),
                                propagation_node: None,
                            }).await;
                            let mut update = snapshot(&service).await;
                            match result {
                                Ok(_) => update.network.push("Message queued for delivery".into()),
                                Err(error) => {
                                    let error = error.to_string();
                                    update.network.push(format!("Send failed: {error}"));
                                    append_send_error(&mut update, &failed_destination, &error);
                                }
                            }
                            if sender.send(update).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    });

    let mut app = TuiApp::new(Box::new(CrosstermBackend::new()?), shared.clone(), updates);
    let _ = app.run(shared, command_sender);

    bridge.abort();
    tokio.block_on(core.shutdown());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tv::HeadlessBackend;

    #[test]
    fn dashboard_constructs_and_renders() {
        let (backend, screen) = HeadlessBackend::new(100, 30);
        let state = Rc::new(RefCell::new(UiState {
            network: vec!["State: Online".into()],
            conversations: vec![ConversationRow {
                destination_hash: "aabbccddeeff00112233445566778899".into(),
                title: "Alice".into(),
                label: "Alice  hello".into(),
                messages: vec!["Peer delivered\nhello".into()],
            }],
            directory: vec![DirectoryRow {
                destination_hash: "aabbccddeeff00112233445566778899".into(),
                title: "Alice".into(),
                label: "+ lxmf.delivery Alice".into(),
            }],
            selected_destination_hash: None,
        }));
        let (_sender, receiver) = mpsc::channel();
        let mut app = TuiApp::new(Box::new(backend), state, receiver);

        app.program.pump_once();
        let frame = screen.snapshot();
        assert!(frame.contains("LXMF Conversations"));
        assert!(frame.contains("Network"));
        assert!(frame.contains("Directory"));
    }

    #[test]
    fn conversation_and_composer_dialogs_bind_selected_peer() {
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
        let conversation = conversation_window(
            Rect::new(0, 0, 100, 28),
            state,
            destination_hash.into(),
            active_composer.clone(),
        );
        assert!(conversation.window.flags().grow);
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

    #[test]
    fn f4_opens_composer_and_submits_message() {
        let destination_hash = "aabbccddeeff00112233445566778899";
        let (backend, screen) = HeadlessBackend::new(100, 30);
        let state = Rc::new(RefCell::new(UiState {
            conversations: vec![ConversationRow {
                destination_hash: destination_hash.into(),
                title: "Alice".into(),
                label: "Alice".into(),
                messages: Vec::new(),
            }],
            ..UiState::default()
        }));
        let (_update_sender, updates) = mpsc::channel();
        let (command_sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);

        screen.push_key(Key::F(4), tv::KeyModifiers::default());
        screen.push_paste("hello from TUI");
        screen.push_key(Key::Enter, tv::KeyModifiers::default());
        screen.push_event(Event::Command(Command::QUIT));
        app.run(state.clone(), command_sender);

        assert!(screen.snapshot().contains("LXMF Conversation — Alice"));
        assert!(state.borrow().conversations[0].messages.is_empty());

        match commands.try_recv() {
            Ok(UiCommand::SendMessage {
                destination_hash: actual,
                content,
            }) => {
                assert_eq!(actual, destination_hash);
                assert_eq!(content, "hello from TUI");
            }
            Err(error) => panic!("F4 did not submit a compose command: {error}"),
        }
    }

    #[test]
    fn f4_uses_peer_focused_with_arrow_keys() {
        let first = "aabbccddeeff00112233445566778899";
        let second = "00112233445566778899aabbccddeeff";
        let (backend, screen) = HeadlessBackend::new(100, 30);
        let state = Rc::new(RefCell::new(UiState {
            conversations: vec![
                ConversationRow {
                    destination_hash: first.into(),
                    title: "Alice".into(),
                    label: "Alice".into(),
                    messages: Vec::new(),
                },
                ConversationRow {
                    destination_hash: second.into(),
                    title: "Bob".into(),
                    label: "Bob".into(),
                    messages: Vec::new(),
                },
            ],
            ..UiState::default()
        }));
        let (_update_sender, updates) = mpsc::channel();
        let (command_sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);

        screen.push_key(Key::Down, tv::KeyModifiers::default());
        screen.push_key(Key::F(4), tv::KeyModifiers::default());
        screen.push_paste("hello Bob");
        screen.push_key(Key::Enter, tv::KeyModifiers::default());
        screen.push_event(Event::Command(Command::QUIT));
        app.run(state, command_sender);

        match commands.try_recv() {
            Ok(UiCommand::SendMessage {
                destination_hash,
                content,
            }) => {
                assert_eq!(destination_hash, second);
                assert_eq!(content, "hello Bob");
            }
            Err(error) => panic!("focused peer was not used by composer: {error}"),
        }
    }

    #[test]
    fn f4_uses_peer_selected_in_directory() {
        let destination_hash = "00112233445566778899aabbccddeeff";
        let (backend, screen) = HeadlessBackend::new(100, 30);
        let state = Rc::new(RefCell::new(UiState {
            directory: vec![DirectoryRow {
                destination_hash: destination_hash.into(),
                title: "Bob".into(),
                label: "+ lxmf.delivery Bob".into(),
            }],
            ..UiState::default()
        }));
        let (_update_sender, updates) = mpsc::channel();
        let (command_sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);

        screen.push_key(
            Key::Char('3'),
            tv::KeyModifiers {
                alt: true,
                ..tv::KeyModifiers::default()
            },
        );
        screen.push_key(Key::F(4), tv::KeyModifiers::default());
        screen.push_paste("hello directory peer");
        screen.push_key(Key::Enter, tv::KeyModifiers::default());
        screen.push_event(Event::Command(Command::QUIT));
        app.run(state, command_sender);

        match commands.try_recv() {
            Ok(UiCommand::SendMessage {
                destination_hash: actual,
                content,
            }) => {
                assert_eq!(actual, destination_hash);
                assert_eq!(content, "hello directory peer");
            }
            Err(error) => panic!("directory peer was not used by composer: {error}"),
        }
    }

    #[test]
    fn refreshed_directory_restores_selected_peer() {
        let selected = "00112233445566778899aabbccddeeff";
        let state = Rc::new(RefCell::new(UiState {
            directory: vec![
                DirectoryRow {
                    destination_hash: "aabbccddeeff00112233445566778899".into(),
                    title: "Alice".into(),
                    label: "+ lxmf.delivery Alice".into(),
                },
                DirectoryRow {
                    destination_hash: selected.into(),
                    title: "Bob".into(),
                    label: "+ lxmf.delivery Bob (updated)".into(),
                },
            ],
            selected_destination_hash: Some(selected.into()),
            ..UiState::default()
        }));
        let list = StateList::new(Rect::new(0, 0, 40, 10), state, Pane::Directory);

        assert_eq!(list.selected_index(), Some(1));
    }

    #[test]
    fn messages_are_rendered_as_single_directional_lines() {
        assert_eq!(
            message_line(true, "delivered", "hello\nfrom   TUI"),
            "[>] hello from TUI"
        );
        assert_eq!(message_line(true, "retrying", "hello"), "[~] hello");
        assert_eq!(message_line(true, "failed", "hello"), "[!] hello");
        assert_eq!(message_line(false, "delivered", "reply"), "[<] reply");
    }

    #[test]
    fn send_failures_are_visible_in_conversation_history() {
        let destination = "00112233445566778899aabbccddeeff";
        let mut state = UiState {
            directory: vec![DirectoryRow {
                destination_hash: destination.into(),
                title: "Bob".into(),
                label: "Bob".into(),
            }],
            ..UiState::default()
        };

        append_send_error(&mut state, destination, "network unavailable");

        assert_eq!(
            state.conversations[0].messages,
            ["[!] Send failed: network unavailable"]
        );
    }
}
