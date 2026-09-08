use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use clap::Parser;
use rsnomadnet_core::Runtime;
use rsnomadnet_core::browser::{BrowserPage, Inline, MicronBlock};
use rsnomadnet_core::config::{AppConfig, Cli};
use rsnomadnet_core::models::{
    ConversationSummary, DirectoryEntry, NetworkSnapshot, RrcHubView, RrcMessageView, ServerEvent,
};
use rsnomadnet_core::service::{AppService, FetchPage, SendMessage};
use tv::window::WindowPalette;
use tv::{
    Backend, Button, ButtonFlags, Command, Context, CrosstermBackend, Desktop, Dialog, DrawCtx,
    Event, FieldValue, GrowMode, InputLine, Key, ListBox, Menu, MenuBar, Program, Rect, ScrollBar,
    StaticText, StatusDef, StatusLine, SystemClock, Theme, View, ViewId, ViewState, Window,
    WindowFlags, alt, delegate,
};
use tv::{KeyEvent, KeyModifiers};
use tvision_rs as tv;

#[path = "tui/updates.rs"]
mod updates;
use updates::{UpdateReceiver, UpdateSender, update_channel};
#[path = "tui/bridge.rs"]
mod bridge;
#[path = "tui/windows.rs"]
mod windows;
use windows::{ManagedWindow, WindowRegistry};

const REFRESH: Command = Command::custom("rsnomadnet.refresh");
const OPEN_CONVERSATION: Command = Command::custom("rsnomadnet.open_conversation");
const SEND_MESSAGE: Command = Command::custom("rsnomadnet.send_message");
const LOAD_OLDER: Command = Command::custom("rsnomadnet.load_older");
const OPEN_RRC_HUB: Command = Command::custom("rsnomadnet.open_rrc_hub");
const OPEN_NODE_BROWSER: Command = Command::custom("rsnomadnet.open_node_browser");
const MAX_DIRECTORY_ROWS: usize = 200;

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

#[derive(Clone, Default)]
struct UiState {
    network: Vec<String>,
    conversations: Vec<ConversationRow>,
    directory: Vec<DirectoryRow>,
    directory_views: HashMap<String, Vec<String>>,
    selected_destination_hash: Option<String>,
    send_results: HashMap<String, SendResult>,
    pending_sends: HashMap<String, String>,
    send_errors: HashMap<String, String>,
    drafts: HashMap<String, String>,
    selected_directory_hash: Option<String>,
}

#[derive(Clone)]
struct SendResult {
    content: String,
    error: Option<String>,
}

#[derive(Clone)]
struct ConversationRow {
    destination_hash: String,
    title: String,
    label: String,
    messages: Vec<String>,
}

#[derive(Clone)]
struct DirectoryRow {
    destination_hash: String,
    title: String,
    label: String,
    kind: DirectoryKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectoryKind {
    Peer,
    Rrc,
    Node,
    Other,
}

enum UiCommand {
    SendMessage {
        destination_hash: String,
        content: String,
    },
    ConnectRrc {
        destination_hash: String,
    },
    FetchNodePage {
        destination_hash: String,
    },
    OpenConversation {
        destination_hash: String,
    },
    CloseConversation {
        destination_hash: String,
    },
    LoadOlder {
        destination_hash: String,
    },
}

type Shared = Rc<RefCell<UiState>>;
type SharedText = Rc<RefCell<String>>;
type ActiveComposer = Rc<RefCell<Option<(String, SharedText)>>>;

#[derive(Clone, Copy)]
enum Pane {
    Conversations,
    Directory,
}

struct NetworkInfo {
    text: StaticText,
    shared: Shared,
}

#[delegate(to = text)]
impl View for NetworkInfo {
    fn draw(&mut self, context: &mut DrawCtx) {
        self.text.set_text(self.shared.borrow().network.join("\n"));
        self.text.draw(context);
    }
}

fn window_key(key: Key, ctrl: bool, shift: bool, alt: bool) -> KeyEvent {
    KeyEvent::new(key, KeyModifiers { ctrl, shift, alt })
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

struct ConversationWindow {
    window: Dialog,
    destination_hash: String,
    content: SharedText,
    active_composer: ActiveComposer,
    shared: Shared,
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
    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }

    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
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

struct StateList {
    list: ListBox,
    state: Shared,
    pane: Pane,
    seeded: bool,
    row_ids: Vec<String>,
}

impl StateList {
    fn new(bounds: Rect, state: Shared, pane: Pane) -> Self {
        let mut view = Self {
            list: ListBox::new(bounds, 1, None, None),
            state,
            pane,
            seeded: false,
            row_ids: Vec::new(),
        };
        view.state_mut().grow_mode = GrowMode {
            hi_x: true,
            hi_y: true,
            ..Default::default()
        };
        view
    }

    fn lines(&self) -> Vec<String> {
        let state = self.state.borrow();
        match self.pane {
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
        self.row_ids.get(index as usize).cloned()
    }

    fn ids(&self) -> Vec<String> {
        let state = self.state.borrow();
        match self.pane {
            Pane::Conversations => state
                .conversations
                .iter()
                .map(|row| row.destination_hash.clone())
                .collect(),
            Pane::Directory => state
                .directory
                .iter()
                .map(|row| row.destination_hash.clone())
                .collect(),
        }
    }

    fn focused_directory_kind(&self) -> Option<DirectoryKind> {
        let FieldValue::Int(index) = self.list.value()? else {
            return None;
        };
        self.state
            .borrow()
            .directory
            .get(index as usize)
            .map(|entry| entry.kind)
    }
}

#[delegate(to = list)]
impl View for StateList {
    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }

    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
        let open = matches!(event, Event::KeyDown(key) if key.key == Key::Enter)
            && matches!(self.pane, Pane::Conversations | Pane::Directory);
        let refresh = matches!(
            event,
            Event::Broadcast { command, .. } if *command == REFRESH
        );
        if !self.seeded || refresh {
            let lines = self.lines();
            let ids = self.ids();
            if !self.seeded || self.list.list() != lines || self.row_ids != ids {
                let selected = self.focused_destination();
                let old_index = self.list.value();
                let index = selected
                    .as_ref()
                    .and_then(|id| ids.iter().position(|item| item == id));
                self.seeded = true;
                self.row_ids = ids;
                self.list.new_list(lines, context);
                if let Some(index) = index {
                    self.list
                        .set_value_ctx(FieldValue::Int(index as i32), context);
                } else if let Some(index) = old_index {
                    self.list.set_value_ctx(index, context);
                }
            }
        }
        self.list.handle_event(event, context);
        // A refresh rebuilds every list. Do not let an unfocused pane's
        // temporary first row replace the peer selected in another pane.
        if !refresh
            && self.list.state().state.focused
            && let Some(destination_hash) = self.focused_destination()
        {
            self.state.borrow_mut().selected_destination_hash = Some(destination_hash.clone());
            if matches!(self.pane, Pane::Directory) {
                self.state.borrow_mut().selected_directory_hash = Some(destination_hash);
            }
        }
        if open && self.state.borrow().selected_destination_hash.is_some() {
            let command = match self.pane {
                Pane::Conversations => Some(OPEN_CONVERSATION),
                Pane::Directory => match self.focused_directory_kind() {
                    Some(DirectoryKind::Peer) => Some(OPEN_CONVERSATION),
                    Some(DirectoryKind::Rrc) => Some(OPEN_RRC_HUB),
                    Some(DirectoryKind::Node) => Some(OPEN_NODE_BROWSER),
                    _ => None,
                },
            };
            if let Some(command) = command {
                context.put_event(Event::Command(command));
            }
            event.clear();
        }
    }
}

struct PumpView {
    state: ViewState,
    shared: Shared,
    updates: UpdateReceiver,
    armed: bool,
}

impl PumpView {
    fn new(shared: Shared, updates: UpdateReceiver) -> Self {
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

fn drain_updates(updates: &UpdateReceiver) -> Option<UiState> {
    updates.try_recv().ok()
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
        if let Some(mut update) = drain_updates(&self.updates) {
            let mut current = self.shared.borrow_mut();
            let selected_directory_hash = current.selected_directory_hash.clone();
            if let Some(selected) = &selected_directory_hash {
                if !update
                    .directory
                    .iter()
                    .any(|row| &row.destination_hash == selected)
                {
                    if let Some(row) = current
                        .directory
                        .iter()
                        .find(|row| &row.destination_hash == selected)
                        .cloned()
                    {
                        update.directory.truncate(MAX_DIRECTORY_ROWS - 1);
                        update.directory.push(row);
                    }
                }
            }
            for (destination, result) in &update.send_results {
                current.pending_sends.remove(destination);
                if let Some(error) = &result.error {
                    current
                        .send_errors
                        .insert(destination.clone(), error.clone());
                } else {
                    current.send_errors.remove(destination);
                }
            }
            let selected = current.selected_destination_hash.clone();
            let mut directory_views = current.directory_views.clone();
            directory_views.extend(update.directory_views);
            let mut send_results = current.send_results.clone();
            send_results.extend(update.send_results);
            let pending_sends = current.pending_sends.clone();
            let send_errors = current.send_errors.clone();
            let drafts = current.drafts.clone();
            drop(current);
            *self.shared.borrow_mut() = UiState {
                selected_destination_hash: selected,
                directory_views,
                send_results,
                pending_sends,
                send_errors,
                drafts,
                selected_directory_hash,
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
    fn new(backend: Box<dyn Backend>, state: Shared, updates: UpdateReceiver) -> Self {
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

    fn desktop(mut bounds: Rect, state: Shared, updates: UpdateReceiver) -> Option<Box<dyn View>> {
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
        conversations.state_mut().options.tileable = true;
        conversations.insert_child(Box::new(StateList::new(
            Rect::new(1, 1, extent.b.x - 1, extent.b.y - 1),
            state.clone(),
            Pane::Conversations,
        )));
        let split = top + height / 2;
        let mut network = Window::new(
            Rect::new(middle, top, bounds.b.x - 1, split),
            Some("Network".into()),
            2,
        );
        network.set_palette(WindowPalette::Gray);
        network.state_mut().options.tileable = true;
        let extent = network.state().get_extent();
        let mut info = NetworkInfo {
            text: StaticText::new(Rect::new(1, 1, extent.b.x - 1, extent.b.y - 1), ""),
            shared: state.clone(),
        };
        info.state_mut().grow_mode = GrowMode {
            hi_x: true,
            hi_y: true,
            ..Default::default()
        };
        network.insert_child(Box::new(info));
        let mut directory = Window::new(
            Rect::new(middle, split, bounds.b.x - 1, bottom),
            Some("Directory".into()),
            3,
        );
        let extent = directory.state().get_extent();
        directory.state_mut().options.tileable = true;
        directory.insert_child(Box::new(StateList::new(
            Rect::new(1, 1, extent.b.x - 1, extent.b.y - 1),
            state.clone(),
            Pane::Directory,
        )));

        // The pump belongs directly to the Desktop. Its pre-process flag then
        // sees the first keyboard event regardless of which window is focused,
        // so external results cannot remain queued behind a stale Loading view.
        desktop.insert_view(Box::new(PumpView::new(state.clone(), updates)));
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
                    .item("~Alt-X~ Exit", alt('x'), Command::QUIT)
                    .item("~F5~ Zoom", KeyEvent::from(Key::F(5)), Command::ZOOM)
                    .item("~F6~ Next", KeyEvent::from(Key::F(6)), Command::NEXT)
                    .key_item(window_key(Key::F(5), true, false, false), Command::RESIZE)
                    .key_item(window_key(Key::F(6), false, true, false), Command::PREV)
                    .key_item(window_key(Key::F(3), false, false, true), Command::CLOSE)
            })
            .build();
        Some(Box::new(StatusLine::new(bounds, definitions)))
    }

    fn menu_bar(mut bounds: Rect) -> Option<Box<dyn View>> {
        bounds.b.y = bounds.a.y + 1;
        let menu = Menu::builder()
            .submenu("~F~ile", alt('f'), |menu| {
                menu.command_key("E~x~it", Command::QUIT, alt('x'), "Alt-X")
            })
            .submenu("~W~indows", alt('w'), |menu| {
                menu.command_key(
                    "~S~ize/Move",
                    Command::RESIZE,
                    window_key(Key::F(5), true, false, false),
                    "Ctrl-F5",
                )
                .command_key("~Z~oom", Command::ZOOM, KeyEvent::from(Key::F(5)), "F5")
                .command("~T~ile", Command::TILE)
                .command("C~a~scade", Command::CASCADE)
                .command_key("~N~ext", Command::NEXT, KeyEvent::from(Key::F(6)), "F6")
                .command_key(
                    "~P~revious",
                    Command::PREV,
                    window_key(Key::F(6), false, true, false),
                    "Shift-F6",
                )
                .command_key(
                    "~C~lose",
                    Command::CLOSE,
                    window_key(Key::F(3), false, false, true),
                    "Alt-F3",
                )
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
        let windows = Rc::new(RefCell::new(WindowRegistry::default()));
        self.program.run_app(move |program, command| {
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
                    if !message.is_empty()
                        && !state.borrow().pending_sends.contains_key(&destination_hash)
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
            } else if command == OPEN_CONVERSATION {
                let Some(destination_hash) = selected_destination(&state.borrow()) else {
                    program.exec_view(Box::new(message_dialog(
                        "LXMF conversation",
                        "Select an LXMF peer in Conversations or Directory first.",
                    )));
                    return;
                };
                let bounds = program.desktop_rect();
                let key = format!("lxmf:{destination_hash}");
                if windows.borrow_mut().focus_existing(&key) {
                    return;
                }
                let _ = commands.send(UiCommand::OpenConversation {
                    destination_hash: destination_hash.clone(),
                });
                program.desktop_insert(Box::new(ManagedWindow::new(
                    Box::new(conversation_window(
                        bounds,
                        state.clone(),
                        destination_hash,
                        active_composer.clone(),
                    )),
                    key,
                    windows.clone(),
                    commands.clone(),
                )));
            } else if matches!(command, OPEN_RRC_HUB | OPEN_NODE_BROWSER) {
                let Some(destination_hash) = selected_destination(&state.borrow()) else {
                    return;
                };
                let bounds = program.desktop_rect();
                let (title, key, ui_command) = if command == OPEN_RRC_HUB {
                    (
                        "RRC Hub",
                        format!("rrc:{destination_hash}"),
                        UiCommand::ConnectRrc {
                            destination_hash: destination_hash.clone(),
                        },
                    )
                } else {
                    (
                        "NomadNet Browser",
                        format!("node:{destination_hash}"),
                        UiCommand::FetchNodePage {
                            destination_hash: destination_hash.clone(),
                        },
                    )
                };
                if windows.borrow_mut().focus_existing(&key) {
                    return;
                }
                state
                    .borrow_mut()
                    .directory_views
                    .insert(key.clone(), vec!["Loading…".into()]);
                program.desktop_insert(Box::new(ManagedWindow::new(
                    Box::new(directory_target_window(
                        bounds,
                        state.clone(),
                        &destination_hash,
                        title,
                        key.clone(),
                    )),
                    key,
                    windows.clone(),
                    commands.clone(),
                )));
                let _ = commands.send(ui_command);
            }
        })
    }
}

fn directory_target_window(
    desktop: Rect,
    state: Shared,
    destination_hash: &str,
    kind_title: &str,
    key: String,
) -> Dialog {
    let width = 64.min(desktop.b.x - desktop.a.x - 2).max(36);
    let height = 18.min(desktop.b.y - desktop.a.y - 2).max(10);
    let left = desktop.a.x + ((desktop.b.x - desktop.a.x - width) / 2).max(0);
    let top = desktop.a.y + ((desktop.b.y - desktop.a.y - height) / 2).max(0);
    let mut window = Dialog::new(
        Rect::new(left, top, left + width, top + height),
        Some(format!(
            "{kind_title} — {}",
            directory_title(&state.borrow(), destination_hash)
        )),
    );
    window.set_flags(WindowFlags {
        r#move: true,
        grow: true,
        close: true,
        zoom: true,
    });
    let extent = window.state().get_extent();
    let mut view =
        DirectoryTargetView::new(Rect::new(1, 1, extent.b.x - 1, extent.b.y - 1), state, key);
    view.state_mut().grow_mode = GrowMode {
        hi_x: true,
        hi_y: true,
        ..Default::default()
    };
    window.insert_child(Box::new(view));
    window
}

struct DirectoryTargetView {
    list: ListBox,
    state: Shared,
    key: String,
    seeded: bool,
}

impl DirectoryTargetView {
    fn new(bounds: Rect, state: Shared, key: String) -> Self {
        Self {
            list: ListBox::new(bounds, 1, None, None),
            state,
            key,
            seeded: false,
        }
    }

    fn lines(&self) -> Vec<String> {
        self.state
            .borrow()
            .directory_views
            .get(&self.key)
            .cloned()
            .unwrap_or_else(|| vec!["Loading…".into()])
    }
}

#[delegate(to = list)]
impl View for DirectoryTargetView {
    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }

    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
        let refresh = matches!(
            event,
            Event::Broadcast { command, .. } if *command == REFRESH
        );
        if !self.seeded || refresh {
            let lines = self.lines();
            if !self.seeded || self.list.list() != lines {
                let selected = self.list.value();
                self.seeded = true;
                self.list.new_list(lines, context);
                if let Some(selected) = selected {
                    self.list.set_value_ctx(selected, context);
                }
            }
        }
        self.list.handle_event(event, context);
    }
}

fn directory_title<'a>(state: &'a UiState, destination_hash: &'a str) -> &'a str {
    state
        .directory
        .iter()
        .find(|entry| entry.destination_hash == destination_hash)
        .map(|entry| entry.title.as_str())
        .unwrap_or(destination_hash)
}

fn node_index_url(destination_hash: &str) -> String {
    format!("{destination_hash}:/page/index.mu")
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
    let content = Rc::new(RefCell::new(
        state
            .borrow()
            .drafts
            .get(&destination_hash)
            .cloned()
            .unwrap_or_default(),
    ));
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
        shared: state,
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

async fn snapshot(service: &AppService) -> UiState {
    let network = service.network_snapshot().await;
    let conversations = service.conversations().unwrap_or_default();
    let directory = service.directory().unwrap_or_default();
    UiState {
        network: network_lines(network),
        conversations: conversation_rows(service, conversations),
        directory: directory_lines(directory),
        directory_views: HashMap::new(),
        selected_destination_hash: None,
        ..UiState::default()
    }
}

fn rrc_hub_lines(service: &AppService, hub: RrcHubView) -> Vec<String> {
    let destination_hash = hub.destination_hash.clone();
    let mut lines = vec![
        format!(
            "State: {}",
            if hub.connected {
                "connected"
            } else {
                "offline"
            }
        ),
        format!("Identity: {}", hub.local_identity),
        format!("Nick: {}", hub.nick.as_deref().unwrap_or("-")),
        hub.detail,
    ];
    if hub.rooms.is_empty() {
        lines.push("Rooms: none".into());
    } else {
        lines.push("Rooms:".into());
        lines.extend(hub.rooms.into_iter().map(|room| format!("  {room}")));
    }
    let history = service
        .rrc_history(&destination_hash, None)
        .unwrap_or_default();
    if !history.is_empty() {
        lines.push(String::new());
        lines.push("Messages:".into());
        lines.extend(history.into_iter().map(rrc_message_line));
    }
    lines
}

fn rrc_message_line(message: RrcMessageView) -> String {
    let room = message
        .room
        .as_deref()
        .map(|room| format!("#{room} "))
        .unwrap_or_default();
    let nick = message.nick.as_deref().unwrap_or(&message.source_hash);
    if message.kind == "action" {
        format!("[{room}* {nick}] {}", message.body)
    } else {
        format!("[{room}{nick}] {}", message.body)
    }
}

fn browser_page_lines(page: BrowserPage) -> Vec<String> {
    let mut lines = vec![page.title.unwrap_or_else(|| page.url.clone()), page.url];
    for block in page.blocks {
        match block {
            MicronBlock::Heading { parts, .. } => lines.push(inline_text(&parts)),
            MicronBlock::Paragraph { depth, parts, .. } => lines.push(format!(
                "{}{}",
                "  ".repeat(depth as usize),
                inline_text(&parts)
            )),
            MicronBlock::Divider { character, .. } => {
                lines.push(std::iter::repeat_n(character, 40).collect())
            }
            MicronBlock::Preformatted { text } => lines.extend(text.lines().map(str::to_owned)),
            MicronBlock::Table { rows, .. } => lines.extend(rows.into_iter().map(|row| {
                row.into_iter()
                    .map(|cell| inline_text(&cell))
                    .collect::<Vec<_>>()
                    .join(" | ")
            })),
            MicronBlock::Partial { target, .. } => lines.push(format!("[partial: {target}]")),
        }
    }
    lines
}

fn inline_text(parts: &[Inline]) -> String {
    parts
        .iter()
        .map(|part| match part {
            Inline::Text { text, .. } => text.clone(),
            Inline::Link { label, target, .. } => format!("{label} [{target}]"),
            Inline::Input { value, .. } => value.clone(),
            Inline::Checkbox { label, checked, .. } | Inline::Radio { label, checked, .. } => {
                format!("[{}] {label}", if *checked { 'x' } else { ' ' })
            }
            Inline::Anchor { name } => format!("#{name}"),
        })
        .collect()
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
    _service: &AppService,
    conversations: Vec<ConversationSummary>,
) -> Vec<ConversationRow> {
    conversations
        .into_iter()
        .map(|conversation| {
            let messages = Vec::new();
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
        // Database::directory() orders by last_seen DESC. The web frontend can
        // cheaply render the complete collection in the browser, while a large
        // terminal ListBox is rebuilt on every refresh and becomes sluggish.
        .take(MAX_DIRECTORY_ROWS)
        .map(|entry| DirectoryRow {
            destination_hash: entry.destination_hash.clone(),
            title: entry
                .display_name
                .clone()
                .unwrap_or_else(|| entry.destination_hash.clone()),
            kind: match entry.kind.as_str() {
                "peer" => DirectoryKind::Peer,
                "rrc" => DirectoryKind::Rrc,
                "node" => DirectoryKind::Node,
                _ => DirectoryKind::Other,
            },
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
    // Subscribe before reading the initial snapshot so startup events cannot
    // fall into a gap between the database read and starting the bridge.
    let events = service.subscribe();
    let initial = tokio.block_on(async {
        // Give the freshly spawned network task a chance to publish Starting
        // before the first frontend snapshot is captured.
        tokio::task::yield_now().await;
        snapshot(&service).await
    });
    let shared = Rc::new(RefCell::new(initial.clone()));
    let (sender, updates) = update_channel();
    let (command_sender, commands) = tokio::sync::mpsc::unbounded_channel();
    let bridge = tokio.spawn(bridge::run(service, initial, sender, commands, events));

    let mut app = TuiApp::new(Box::new(CrosstermBackend::new()?), shared.clone(), updates);
    let _ = app.run(shared, command_sender);

    drop(app);
    tokio.block_on(async {
        bridge.abort();
        let _ = bridge.await;
        core.shutdown().await;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tv::HeadlessBackend;

    fn with_context(run: impl FnOnce(&mut Context)) {
        let mut events = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred = Vec::new();
        let mut context = Context::new(&mut events, &mut timers, 0, &mut deferred);
        run(&mut context);
    }

    #[test]
    fn window_commands_arrange_zoom_and_close_network() {
        let (backend, screen) = HeadlessBackend::new(100, 30);
        let shared = Rc::new(RefCell::new(UiState {
            network: vec!["State: Online".into()],
            ..UiState::default()
        }));
        let (_sender, updates) = update_channel();
        let mut app = TuiApp::new(Box::new(backend), shared, updates);
        for _ in 0..12 {
            app.program.pump_once();
        }
        let initial = screen.snapshot();
        screen.push_event(Event::Command(Command::TILE));
        for _ in 0..12 {
            app.program.pump_once();
        }
        let tiled = screen.snapshot();
        assert_ne!(tiled, initial);
        screen.push_event(Event::Command(Command::CASCADE));
        for _ in 0..12 {
            app.program.pump_once();
        }
        assert_ne!(screen.snapshot(), tiled);
        screen.push_key(
            Key::Char('2'),
            KeyModifiers {
                alt: true,
                ..Default::default()
            },
        );
        for _ in 0..12 {
            app.program.pump_once();
        }
        let before_zoom = screen.snapshot();
        screen.push_key(Key::F(5), KeyModifiers::default());
        for _ in 0..12 {
            app.program.pump_once();
        }
        assert_ne!(screen.snapshot(), before_zoom);
        assert!(screen.snapshot().contains("State: Online"));
        screen.push_key(
            Key::F(3),
            KeyModifiers {
                alt: true,
                ..Default::default()
            },
        );
        for _ in 0..12 {
            app.program.pump_once();
        }
        assert!(!screen.snapshot().contains("State: Online"));
    }

    #[test]
    fn periodic_refresh_does_not_move_history_selection() {
        let state = Rc::new(RefCell::new(UiState::default()));
        state
            .borrow_mut()
            .directory_views
            .insert("node:aa".into(), (0..60).map(|n| n.to_string()).collect());
        let mut page = DirectoryTargetView::new(Rect::new(0, 0, 40, 8), state, "node:aa".into());
        with_context(|ctx| {
            page.handle_event(
                &mut Event::Broadcast {
                    command: REFRESH,
                    source: None,
                },
                ctx,
            );
            page.list.set_value_ctx(FieldValue::Int(30), ctx);
            page.handle_event(
                &mut Event::Broadcast {
                    command: REFRESH,
                    source: None,
                },
                ctx,
            );
            assert_eq!(page.list.value(), Some(FieldValue::Int(30)));
        });
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
    fn directory_tracks_identity_when_equal_labels_are_reordered() {
        let state = Rc::new(RefCell::new(UiState {
            directory: vec!["aa", "bb"]
                .into_iter()
                .map(|id| DirectoryRow {
                    destination_hash: id.into(),
                    title: "Same name".into(),
                    label: "Same name".into(),
                    kind: DirectoryKind::Peer,
                })
                .collect(),
            ..UiState::default()
        }));
        let mut list = StateList::new(Rect::new(0, 0, 40, 8), state.clone(), Pane::Directory);
        with_context(|ctx| {
            list.handle_event(
                &mut Event::Broadcast {
                    command: REFRESH,
                    source: None,
                },
                ctx,
            );
            list.list.set_value_ctx(FieldValue::Int(1), ctx);
            state.borrow_mut().directory.reverse();
            list.handle_event(
                &mut Event::Broadcast {
                    command: REFRESH,
                    source: None,
                },
                ctx,
            );
            assert_eq!(list.focused_destination().as_deref(), Some("bb"));
            assert_eq!(list.list.value(), Some(FieldValue::Int(0)));
        });
    }

    #[test]
    fn send_acknowledgement_keeps_failed_or_newly_edited_text() {
        let state = Rc::new(RefCell::new(UiState::default()));
        let mut window = conversation_window(
            Rect::new(0, 0, 100, 30),
            state.clone(),
            "aa".into(),
            Rc::new(RefCell::new(None)),
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
    fn coalescing_preserves_send_failures_until_consumed() {
        let (sender, updates) = update_channel();
        let mut update = UiState::default();
        update.send_results.insert(
            "aa".into(),
            SendResult {
                content: "draft".into(),
                error: Some("offline".into()),
            },
        );
        sender.send(update).unwrap();
        for _ in 0..1000 {
            sender.send(UiState::default()).unwrap();
        }
        assert_eq!(
            updates.try_recv().unwrap().send_results["aa"]
                .error
                .as_deref(),
            Some("offline")
        );
        assert!(updates.try_recv().is_err());
    }

    #[test]
    fn send_error_remains_visible_after_network_refresh() {
        let state = Rc::new(RefCell::new(UiState::default()));
        state
            .borrow_mut()
            .pending_sends
            .insert("aa".into(), "draft".into());
        let (sender, updates) = update_channel();
        let mut pump = PumpView::new(state.clone(), updates);
        let timer = tv::TimerQueue::new().set_timer(0, Duration::from_millis(1), None);
        let mut update = UiState::default();
        update.send_results.insert(
            "aa".into(),
            SendResult {
                content: "draft".into(),
                error: Some("offline".into()),
            },
        );
        with_context(|ctx| {
            sender.send(update).unwrap();
            pump.handle_event(&mut Event::Timer(timer), ctx);
            sender.send(UiState::default()).unwrap();
            pump.handle_event(&mut Event::Timer(timer), ctx);
            assert!(!state.borrow().pending_sends.contains_key("aa"));
            assert_eq!(
                state.borrow().send_errors.get("aa").map(String::as_str),
                Some("offline")
            );
        });
    }

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
                kind: DirectoryKind::Peer,
            }],
            directory_views: HashMap::new(),
            selected_destination_hash: None,
            ..UiState::default()
        }));
        let (_sender, receiver) = update_channel();
        let mut app = TuiApp::new(Box::new(backend), state, receiver);

        app.program.pump_once();
        let frame = screen.snapshot();
        assert!(frame.contains("LXMF Conversations"));
        assert!(frame.contains("Network"));
        assert!(frame.contains("Directory"));
    }

    #[test]
    fn enter_on_rrc_directory_entry_opens_hub_window() {
        let destination_hash = "00112233445566778899aabbccddeeff";
        let (backend, screen) = HeadlessBackend::new(100, 30);
        let state = Rc::new(RefCell::new(UiState {
            directory: vec![DirectoryRow {
                destination_hash: destination_hash.into(),
                title: "SPb Hub".into(),
                label: "+ rrc SPb Hub".into(),
                kind: DirectoryKind::Rrc,
            }],
            ..UiState::default()
        }));
        let (_update_sender, updates) = update_channel();
        let (command_sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);

        screen.push_key(
            Key::Char('3'),
            tv::KeyModifiers {
                alt: true,
                ..tv::KeyModifiers::default()
            },
        );
        screen.push_key(Key::Enter, tv::KeyModifiers::default());
        screen.push_event(Event::Command(Command::QUIT));
        app.run(state, command_sender);

        let frame = screen.snapshot();
        assert!(frame.contains("RRC Hub"));
        assert!(frame.contains("SPb Hub"));
        assert!(matches!(
            commands.try_recv(),
            Ok(UiCommand::ConnectRrc { destination_hash: actual }) if actual == destination_hash
        ));
    }

    #[test]
    fn enter_on_node_directory_entry_opens_browser_window() {
        let destination_hash = "00112233445566778899aabbccddeeff";
        let (backend, screen) = HeadlessBackend::new(100, 30);
        let state = Rc::new(RefCell::new(UiState {
            directory: vec![DirectoryRow {
                destination_hash: destination_hash.into(),
                title: "SPb Node".into(),
                label: "+ node SPb Node".into(),
                kind: DirectoryKind::Node,
            }],
            ..UiState::default()
        }));
        let (_update_sender, updates) = update_channel();
        let (command_sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);

        screen.push_key(
            Key::Char('3'),
            tv::KeyModifiers {
                alt: true,
                ..tv::KeyModifiers::default()
            },
        );
        screen.push_key(Key::Enter, tv::KeyModifiers::default());
        screen.push_event(Event::Command(Command::QUIT));
        app.run(state, command_sender);

        let frame = screen.snapshot();
        assert!(frame.contains("NomadNet Browser"));
        assert!(frame.contains("SPb Node"));
        assert!(matches!(
            commands.try_recv(),
            Ok(UiCommand::FetchNodePage { destination_hash: actual }) if actual == destination_hash
        ));
        assert_eq!(
            node_index_url(destination_hash),
            "00112233445566778899aabbccddeeff:/page/index.mu"
        );
    }

    #[test]
    fn queued_resource_result_survives_a_later_periodic_snapshot() {
        let (sender, updates) = update_channel();
        let mut result = UiState::default();
        result
            .directory_views
            .insert("node:0011".into(), vec!["Loaded page".into()]);
        sender.send(result).unwrap();
        sender.send(UiState::default()).unwrap();

        let merged = drain_updates(&updates).expect("queued updates");
        assert_eq!(
            merged.directory_views.get("node:0011"),
            Some(&vec!["Loaded page".into()])
        );
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
    fn enter_opens_conversation_and_submits_message() {
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
        let (_update_sender, updates) = update_channel();
        let (command_sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);

        screen.push_key(Key::Enter, tv::KeyModifiers::default());
        screen.push_paste("hello from TUI");
        screen.push_key(Key::Enter, tv::KeyModifiers::default());
        screen.push_event(Event::Command(Command::QUIT));
        app.run(state.clone(), command_sender);

        assert!(screen.snapshot().contains("LXMF Conversation — Alice"));
        assert!(state.borrow().conversations[0].messages.is_empty());

        assert!(matches!(
            commands.try_recv(),
            Ok(UiCommand::OpenConversation { .. })
        ));
        match commands.try_recv() {
            Ok(UiCommand::SendMessage {
                destination_hash: actual,
                content,
            }) => {
                assert_eq!(actual, destination_hash);
                assert_eq!(content, "hello from TUI");
            }
            Err(error) => panic!("Enter did not submit a compose command: {error}"),
            Ok(_) => panic!("Enter submitted the wrong command"),
        }
    }

    #[test]
    fn enter_uses_peer_focused_with_arrow_keys() {
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
        let (_update_sender, updates) = update_channel();
        let (command_sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);

        screen.push_key(Key::Down, tv::KeyModifiers::default());
        screen.push_key(Key::Enter, tv::KeyModifiers::default());
        screen.push_paste("hello Bob");
        screen.push_key(Key::Enter, tv::KeyModifiers::default());
        screen.push_event(Event::Command(Command::QUIT));
        app.run(state, command_sender);

        assert!(matches!(
            commands.try_recv(),
            Ok(UiCommand::OpenConversation { .. })
        ));
        match commands.try_recv() {
            Ok(UiCommand::SendMessage {
                destination_hash,
                content,
            }) => {
                assert_eq!(destination_hash, second);
                assert_eq!(content, "hello Bob");
            }
            Err(error) => panic!("focused peer was not used by composer: {error}"),
            Ok(_) => panic!("focused peer submitted the wrong command"),
        }
    }

    #[test]
    fn enter_opens_peer_selected_in_directory() {
        let destination_hash = "00112233445566778899aabbccddeeff";
        let (backend, screen) = HeadlessBackend::new(100, 30);
        let state = Rc::new(RefCell::new(UiState {
            directory: vec![DirectoryRow {
                destination_hash: destination_hash.into(),
                title: "Bob".into(),
                label: "+ lxmf.delivery Bob".into(),
                kind: DirectoryKind::Peer,
            }],
            ..UiState::default()
        }));
        let (_update_sender, updates) = update_channel();
        let (command_sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);

        screen.push_key(
            Key::Char('3'),
            tv::KeyModifiers {
                alt: true,
                ..tv::KeyModifiers::default()
            },
        );
        screen.push_key(Key::Enter, tv::KeyModifiers::default());
        screen.push_paste("hello directory peer");
        screen.push_key(Key::Enter, tv::KeyModifiers::default());
        screen.push_event(Event::Command(Command::QUIT));
        app.run(state, command_sender);

        assert!(matches!(
            commands.try_recv(),
            Ok(UiCommand::OpenConversation { .. })
        ));
        match commands.try_recv() {
            Ok(UiCommand::SendMessage {
                destination_hash: actual,
                content,
            }) => {
                assert_eq!(actual, destination_hash);
                assert_eq!(content, "hello directory peer");
            }
            Err(error) => panic!("directory peer was not used by composer: {error}"),
            Ok(_) => panic!("directory peer submitted the wrong command"),
        }
    }

    #[test]
    fn directory_is_limited_to_recent_terminal_rows() {
        let entries = (0..MAX_DIRECTORY_ROWS + 25)
            .map(|index| DirectoryEntry {
                destination_hash: format!("{index:032x}"),
                identity_hash: None,
                delivery_hash: None,
                kind: "node".into(),
                display_name: Some(format!("Node {index}")),
                hops: 1,
                last_seen: (MAX_DIRECTORY_ROWS + 25 - index) as i64,
                active: true,
            })
            .collect();

        let rows = directory_lines(entries);
        assert_eq!(rows.len(), MAX_DIRECTORY_ROWS);
        assert_eq!(rows.first().unwrap().title, "Node 0");
        assert_eq!(rows.last().unwrap().title, "Node 199");
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
    fn rrc_messages_include_room_and_nick() {
        assert_eq!(
            rrc_message_line(RrcMessageView {
                hub_hash: "0011".into(),
                room: Some("general".into()),
                source_hash: "aabb".into(),
                nick: Some("Alice".into()),
                body: "waves".into(),
                timestamp_ms: 0,
                kind: "action".into(),
            }),
            "[#general * Alice] waves"
        );
    }
}
