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
#[path = "tui/browser/mod.rs"]
mod browser;
#[path = "tui/conversation.rs"]
mod conversation;
#[path = "tui/conversations.rs"]
mod conversations;
#[path = "tui/files.rs"]
mod files;
#[path = "tui/state_list.rs"]
mod state_list;
use conversation::{ActiveComposer, LOAD_OLDER, SEND_MESSAGE, window as conversation_window};
use conversations::rows as conversation_rows;
use state_list::StateList;
#[path = "tui/directory.rs"]
mod directory;
#[path = "tui/layout.rs"]
mod layout;
#[path = "tui/logging.rs"]
mod logging;
#[path = "tui/windows.rs"]
mod windows;
use windows::{ManagedWindow, WindowRegistry};

const REFRESH: Command = Command::custom("rsnomadnet.refresh");
const ANNOUNCE: Command = Command::custom("rsnomadnet.announce");
const RENAME: Command = Command::custom("rsnomadnet.rename");
const NETWORK_HELP: tv::help::HelpCtx = tv::help::HelpCtx::custom("rsnomadnet.network");
const OPEN_CONVERSATIONS: Command = Command::custom("rsnomadnet.open_conversations");
const OPEN_NETWORK: Command = Command::custom("rsnomadnet.open_network");
const OPEN_DIRECTORY: Command = Command::custom("rsnomadnet.open_directory");
const OPEN_CONVERSATION: Command = Command::custom("rsnomadnet.open_conversation");
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
    /// Write diagnostic logs to a new file (never to the TUI terminal).
    #[arg(long)]
    log_file: Option<PathBuf>,
}

#[derive(Clone, Default)]
struct UiState {
    announce_name: String,
    announce_status: String,
    file_offers: Vec<rsnomadnet_core::attachments::FileOffer>,
    network: Vec<String>,
    network_interfaces: Vec<rsnomadnet_core::models::InterfaceSnapshot>,
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
    Propagation,
    Rrc,
    Node,
    Other,
}

enum UiCommand {
    Announce {
        name: Option<String>,
    },
    SendFile {
        destination_hash: String,
        path: PathBuf,
        reply: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    DecideFile {
        id: String,
        accept: bool,
        reply: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    ClearConversation {
        destination_hash: String,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    SendMessage {
        destination_hash: String,
        content: String,
    },
    ConnectRrc {
        destination_hash: String,
    },
    BrowserFetch(browser::Request),
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

#[derive(Clone, Copy)]
enum Pane {
    Conversations,
    Directory,
}

struct NetworkInfo {
    text: StaticText,
    shared: Shared,
}

struct NetworkWindow {
    window: Window,
}

#[delegate(to = window)]
impl View for NetworkWindow {
    fn get_help_ctx(&self) -> tv::help::HelpCtx {
        NETWORK_HELP
    }
    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        if let Event::KeyDown(key) = event {
            if key.modifiers.ctrl && !key.modifiers.alt && matches!(key.key, Key::Char('n' | 'N')) {
                ctx.put_event(Event::Command(RENAME));
                event.clear();
                return;
            }
        }
        self.window.handle_event(event, ctx);
    }
}

#[delegate(to = text)]
impl View for NetworkInfo {
    fn get_help_ctx(&self) -> tv::help::HelpCtx {
        NETWORK_HELP
    }
    fn draw(&mut self, context: &mut DrawCtx) {
        let width = self.text.state().get_extent().b.x.max(0) as usize;
        let state = self.shared.borrow();
        self.text.set_text(network_text(&state, width));
        self.text.draw(context);
    }
}

fn window_key(key: Key, ctrl: bool, shift: bool, alt: bool) -> KeyEvent {
    KeyEvent::new(key, KeyModifiers { ctrl, shift, alt })
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
            context.put_event(Event::Command(files::REVIEW));
        }
    }
}

struct TuiApp {
    program: Program,
    layout: layout::Store,
    saved: Option<layout::Layout>,
}

impl TuiApp {
    #[cfg(test)]
    fn new(backend: Box<dyn Backend>, state: Shared, updates: UpdateReceiver) -> Self {
        Self::with_layout(backend, state, updates, None)
    }

    fn with_layout(
        backend: Box<dyn Backend>,
        state: Shared,
        updates: UpdateReceiver,
        saved: Option<layout::Layout>,
    ) -> Self {
        let layout = Rc::new(RefCell::new(layout::Layout::default()));
        if let Some(saved) = &saved {
            layout.borrow_mut().directory_filters = saved.directory_filters.clone();
        }
        let tracked = layout.clone();
        let restore = saved.clone();
        let program = Program::new(
            backend,
            Box::new(SystemClock::new()),
            Theme::classic_blue(),
            move |bounds| Self::desktop(bounds, state.clone(), updates, tracked, restore),
            Self::status_line,
            Self::menu_bar,
        );
        Self {
            program,
            layout,
            saved,
        }
    }

    fn desktop(
        mut bounds: Rect,
        state: Shared,
        updates: UpdateReceiver,
        layout: layout::Store,
        saved: Option<layout::Layout>,
    ) -> Option<Box<dyn View>> {
        bounds.a.y += 1;
        bounds.b.y -= 1;
        let mut desktop = Desktop::new(bounds, |rect| Some(Desktop::init_background(rect)));
        // The pump remains alive even when every window has been closed.
        desktop.insert_view(Box::new(PumpView::new(state.clone(), updates)));
        for key in ["network", "directory", "conversations"] {
            if saved
                .as_ref()
                .is_none_or(|s| s.windows.iter().any(|w| w.key == key))
            {
                desktop.insert_view(Box::new(layout::TrackedWindow::new(
                    Self::main_window(bounds, state.clone(), key, &layout),
                    key.into(),
                    layout.clone(),
                    saved.as_ref(),
                    bounds,
                )));
            }
        }
        Some(Box::new(desktop))
    }

    fn main_window(
        bounds: Rect,
        state: Shared,
        key: &str,
        layout: &layout::Store,
    ) -> Box<dyn View> {
        let width = bounds.b.x - bounds.a.x;
        let height = bounds.b.y - bounds.a.y;
        let left = bounds.a.x + 1;
        let top = bounds.a.y + 1;
        let middle = left + width / 2;
        let bottom = bounds.b.y - 1;

        let split = top + height / 2;
        if key != "network" {
            let (rect, title, number) = match key {
                "conversations" => (
                    Rect::new(left, top, middle, bottom),
                    "LXMF Conversations",
                    1,
                ),
                "directory" => (
                    Rect::new(middle, split, bounds.b.x - 1, bottom),
                    "Directory",
                    3,
                ),
                _ => unreachable!("unknown main window"),
            };
            let mut window = Window::new(rect, Some(title.into()), number);
            window.state_mut().options.tileable = true;
            if key == "directory" {
                return Box::new(directory::window(
                    window,
                    state,
                    layout.borrow().directory_filters.clone(),
                ));
            }
            return Box::new(conversations::window(window, state));
        }
        let mut network = Window::new(
            Rect::new(middle, top, bounds.b.x - 1, split),
            Some("Network".into()),
            2,
        );
        network.set_palette(WindowPalette::Gray);
        network.state_mut().help_ctx = NETWORK_HELP;
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
        Box::new(NetworkWindow { window: network })
    }

    fn status_line(mut bounds: Rect) -> Option<Box<dyn View>> {
        bounds.a.y = bounds.b.y - 1;
        let definitions = StatusDef::list()
            .def_one_of([NETWORK_HELP], |definition| {
                definition
                    .item(
                        "~Ctrl-N~ Name",
                        window_key(Key::Char('n'), true, false, false),
                        RENAME,
                    )
                    .item("~F9~ Announce", KeyEvent::from(Key::F(9)), ANNOUNCE)
                    .item("~Alt-X~ Exit", alt('x'), Command::QUIT)
                    .item("~F5~ Zoom", KeyEvent::from(Key::F(5)), Command::ZOOM)
                    .item("~F6~ Next", KeyEvent::from(Key::F(6)), Command::NEXT)
                    .key_item(window_key(Key::F(5), true, false, false), Command::RESIZE)
                    .key_item(window_key(Key::F(6), false, true, false), Command::PREV)
                    .key_item(window_key(Key::F(3), false, false, true), Command::CLOSE)
            })
            .def_one_of([conversation::HELP], |definition| {
                definition
                    .item("~F9~ Announce", KeyEvent::from(Key::F(9)), ANNOUNCE)
                    .item("~Ctrl-A~ Send file", None, files::SEND)
                    .item("~Ctrl-L~ Clear history", None, conversation::CLEAR_HISTORY)
                    .item("~Alt-X~ Exit", alt('x'), Command::QUIT)
                    .item("~F5~ Zoom", KeyEvent::from(Key::F(5)), Command::ZOOM)
                    .item("~F6~ Next", KeyEvent::from(Key::F(6)), Command::NEXT)
                    .key_item(window_key(Key::F(5), true, false, false), Command::RESIZE)
                    .key_item(window_key(Key::F(6), false, true, false), Command::PREV)
                    .key_item(window_key(Key::F(3), false, false, true), Command::CLOSE)
            })
            .def_one_of([browser::HELP], |definition| {
                definition
                    .item("~F9~ Announce", KeyEvent::from(Key::F(9)), ANNOUNCE)
                    // Arrows are handled by the page, preserving input cursor movement.
                    .item("~Left~ Back", None, browser::BACK)
                    .item("~Right~ Forward", None, browser::FORWARD)
                    .item("~Ctrl-R~ Reload", None, browser::RELOAD)
                    .item("~Esc~ Stop", None, browser::STOP)
                    .key_item(alt('x'), Command::QUIT)
                    .key_item(KeyEvent::from(Key::F(5)), Command::ZOOM)
                    .key_item(KeyEvent::from(Key::F(6)), Command::NEXT)
                    .key_item(window_key(Key::F(5), true, false, false), Command::RESIZE)
                    .key_item(window_key(Key::F(6), false, true, false), Command::PREV)
                    .key_item(window_key(Key::F(3), false, false, true), Command::CLOSE)
            })
            .def_all(|definition| {
                definition
                    .item("~F9~ Announce", KeyEvent::from(Key::F(9)), ANNOUNCE)
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
                menu.command("C~o~nversations", OPEN_CONVERSATIONS)
                    .command("N~e~twork", OPEN_NETWORK)
                    .command("~D~irectory", OPEN_DIRECTORY)
                    .separator()
                    .command_key(
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
        let tracked = self.layout.clone();
        if let Some(saved) = &self.saved {
            for entry in &saved.windows {
                let Some((kind, hash)) = entry.key.split_once(':') else {
                    continue;
                };
                let bounds = self.program.desktop_rect();
                let mut pending = None;
                let view: Box<dyn View> = match kind {
                    "lxmf" => {
                        let _ = commands.send(UiCommand::OpenConversation {
                            destination_hash: hash.into(),
                        });
                        Box::new(conversation_window(
                            bounds,
                            state.clone(),
                            hash.into(),
                            active_composer.clone(),
                            commands.clone(),
                        ))
                    }
                    "node" => Box::new(browser::restored_window(
                        bounds,
                        state.clone(),
                        hash,
                        commands.clone(),
                        entry.url.as_deref(),
                    )),
                    "rrc" => {
                        state
                            .borrow_mut()
                            .directory_views
                            .insert(entry.key.clone(), vec!["Waiting for network…".into()]);
                        pending = Some((
                            state.clone(),
                            commands.clone(),
                            UiCommand::ConnectRrc {
                                destination_hash: hash.into(),
                            },
                        ));
                        Box::new(directory_target_window(
                            bounds,
                            state.clone(),
                            hash,
                            "RRC Hub",
                            entry.key.clone(),
                        ))
                    }
                    _ => continue,
                };
                let view = Box::new(ManagedWindow::new(
                    view,
                    entry.key.clone(),
                    windows.clone(),
                    commands.clone(),
                ));
                let mut view = layout::TrackedWindow::new(
                    view,
                    entry.key.clone(),
                    tracked.clone(),
                    Some(saved),
                    bounds,
                );
                view.pending = pending;
                self.program.desktop_insert(Box::new(view));
            }
        }
        let mut offered_files = std::collections::HashSet::new();
        self.program.run_app(move |program, command| {
            if command == ANNOUNCE || command == RENAME {
                let name = if command == RENAME {
                    let initial = state.borrow().announce_name.clone();
                    let (result, name) =
                        program.input_box("Announce name", "~N~ame", &initial, 128);
                    if result != Command::OK {
                        return;
                    }
                    Some(name)
                } else {
                    None
                };
                state.borrow_mut().announce_status = "Announcing…".into();
                if commands.send(UiCommand::Announce { name }).is_err() {
                    state.borrow_mut().announce_status = "Announce failed: service stopped".into();
                }
                return;
            }
            if command == files::REVIEW {
                let offers = state.borrow().file_offers.clone();
                offered_files.retain(|id| offers.iter().any(|offer| &offer.id == id));
                for offer in offers {
                    if offered_files.insert(offer.id.clone()) {
                        let bounds = program.desktop_rect();
                        program.desktop_insert(Box::new(files::FileWindow::offer(
                            bounds,
                            offer,
                            commands.clone(),
                        )));
                    }
                }
                return;
            }
            if command == files::SEND {
                let destination = active_composer
                    .borrow()
                    .as_ref()
                    .map(|(destination, _)| destination.clone());
                if let Some(destination) = destination {
                    if let Some(path) = program.open_file_dialog("Send LXMF file", "*") {
                        let (reply, response) = tokio::sync::oneshot::channel();
                        let name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        let _ = commands.send(UiCommand::SendFile {
                            destination_hash: destination,
                            path,
                            reply,
                        });
                        let bounds = program.desktop_rect();
                        program.desktop_insert(Box::new(files::FileWindow::sending(
                            bounds,
                            name,
                            response,
                            commands.clone(),
                        )));
                    }
                }
                return;
            }
            if let Some(key) = match command {
                OPEN_CONVERSATIONS => Some("conversations"),
                OPEN_NETWORK => Some("network"),
                OPEN_DIRECTORY => Some("directory"),
                _ => None,
            } {
                if !tracked.borrow_mut().focus_existing(key) {
                    let bounds = program.desktop_rect();
                    program.desktop_insert(Box::new(layout::TrackedWindow::new(
                        Self::main_window(bounds, state.clone(), key, &tracked),
                        key.into(),
                        tracked.clone(),
                        None,
                        bounds,
                    )));
                }
            } else if matches!(command, SEND_MESSAGE | LOAD_OLDER) {
                conversation::handle_command(command, &state, &active_composer, &commands);
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
                let view = Box::new(ManagedWindow::new(
                    Box::new(conversation_window(
                        bounds,
                        state.clone(),
                        destination_hash,
                        active_composer.clone(),
                        commands.clone(),
                    )),
                    key.clone(),
                    windows.clone(),
                    commands.clone(),
                ));
                program.desktop_insert(Box::new(layout::TrackedWindow::new(
                    view,
                    key,
                    tracked.clone(),
                    None,
                    bounds,
                )));
            } else if matches!(command, OPEN_RRC_HUB | OPEN_NODE_BROWSER) {
                let Some(destination_hash) = selected_destination(&state.borrow()) else {
                    return;
                };
                let bounds = program.desktop_rect();
                let key = format!(
                    "{}:{destination_hash}",
                    if command == OPEN_RRC_HUB {
                        "rrc"
                    } else {
                        "node"
                    }
                );
                if windows.borrow_mut().focus_existing(&key) {
                    return;
                }
                let view: Box<dyn View> = if command == OPEN_NODE_BROWSER {
                    Box::new(browser::window(
                        bounds,
                        state.clone(),
                        &destination_hash,
                        commands.clone(),
                    ))
                } else {
                    state
                        .borrow_mut()
                        .directory_views
                        .insert(key.clone(), vec!["Loading…".into()]);
                    let _ = commands.send(UiCommand::ConnectRrc {
                        destination_hash: destination_hash.clone(),
                    });
                    Box::new(directory_target_window(
                        bounds,
                        state.clone(),
                        &destination_hash,
                        "RRC Hub",
                        key.clone(),
                    ))
                };
                let view = Box::new(ManagedWindow::new(
                    view,
                    key.clone(),
                    windows.clone(),
                    commands.clone(),
                ));
                program.desktop_insert(Box::new(layout::TrackedWindow::new(
                    view,
                    key,
                    tracked.clone(),
                    None,
                    bounds,
                )));
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
        announce_name: service
            .identity_settings()
            .await
            .map(|s| s.name)
            .unwrap_or_default(),
        network_interfaces: network.interfaces.clone(),
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

fn network_lines(network: NetworkSnapshot) -> Vec<String> {
    vec![
        format!("State: {:?}", network.state),
        network.detail,
        format!(
            "Destination: {}",
            network.destination_hash.as_deref().unwrap_or("-")
        ),
    ]
}

fn update_network(state: &mut UiState, network: NetworkSnapshot) {
    state.network_interfaces = network.interfaces.clone();
    state.network = network_lines(network);
}

fn network_text(state: &UiState, width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    fn clipped(text: &str, width: usize) -> String {
        let mut used = 0;
        text.graphemes(true)
            .take_while(|s| {
                used += s.width();
                used <= width
            })
            .collect()
    }
    let counters: Vec<_> = state
        .network_interfaces
        .iter()
        .map(|i| {
            (
                format!("{:.2} KB", i.rx_bytes as f64 / 1024.0),
                format!("{:.2} KB", i.tx_bytes as f64 / 1024.0),
            )
        })
        .collect();
    let rx_width = counters.iter().map(|(rx, _)| rx.len()).max().unwrap_or(0);
    let tx_width = counters.iter().map(|(_, tx)| tx.len()).max().unwrap_or(0);
    let mut lines: Vec<_> = state.network.iter().map(|s| clipped(s, width)).collect();
    if !state.announce_name.is_empty() {
        lines.push(clipped(&format!("Name: {}", state.announce_name), width));
    }
    if !state.announce_status.is_empty() {
        lines.push(clipped(&state.announce_status, width));
    }
    for (interface, (rx, tx)) in state.network_interfaces.iter().zip(counters) {
        let counts = format!("RX {rx:>rx_width$}  TX {tx:>tx_width$}");
        let name_width = width.saturating_sub(counts.len() + 1);
        let name = clipped(
            &format!(
                "{} {}",
                if interface.online { "+" } else { "-" },
                interface.name
            ),
            name_width,
        );
        let padding = width.saturating_sub(name.width() + counts.len());
        lines.push(clipped(
            &format!("{name}{}{counts}", " ".repeat(padding)),
            width,
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
#[test]
fn network_status_and_global_announce_shortcuts() {
    let (backend, screen) = tv::HeadlessBackend::new(100, 30);
    let (_sender, updates) = update_channel();
    let shared = Rc::new(RefCell::new(UiState {
        announce_name: "Old".into(),
        ..Default::default()
    }));
    let mut app = TuiApp::new(Box::new(backend), shared.clone(), updates);
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    screen.push_key(
        Key::Char('2'),
        KeyModifiers {
            alt: true,
            ..Default::default()
        },
    );
    for _ in 0..20 {
        app.program.pump_once();
    }
    assert!(screen.snapshot().contains("Ctrl-N"));
    assert!(screen.snapshot().contains("F9"));
    screen.push_key(Key::F(9), KeyModifiers::default());
    screen.push_key(
        Key::Char('3'),
        KeyModifiers {
            alt: true,
            ..Default::default()
        },
    );
    screen.push_key(Key::F(9), KeyModifiers::default());
    screen.push_key(
        Key::Char('x'),
        KeyModifiers {
            alt: true,
            ..Default::default()
        },
    );
    app.run(shared, commands);
    assert!(matches!(
        receiver.try_recv(),
        Ok(UiCommand::Announce { name: None })
    ));
    assert!(matches!(
        receiver.try_recv(),
        Ok(UiCommand::Announce { name: None })
    ));
}

#[cfg(test)]
#[test]
fn network_counters_align_to_right_edge_after_resize() {
    use rsnomadnet_core::models::InterfaceSnapshot;
    use unicode_width::UnicodeWidthStr;
    let interface = |name: &str, rx_bytes, tx_bytes| InterfaceSnapshot {
        id: 1,
        name: name.into(),
        online: true,
        mode: String::new(),
        role: String::new(),
        bitrate: 0,
        mtu: 500,
        rx_bytes,
        tx_bytes,
        rx_rate: 0,
        tx_rate: 0,
        held_announces: 0,
        tx_drops: 0,
    };
    let state = UiState {
        network_interfaces: vec![
            interface("TCP", 1024, 1280),
            interface("Очень длинное имя 界", 1024000, 0),
        ],
        ..Default::default()
    };
    for width in [38, 60, 100] {
        let text = network_text(&state, width);
        let lines: Vec<_> = text.lines().collect();
        assert!(lines.iter().all(|line| line.width() == width));
        assert!(lines[0].contains("1.00 KB"));
        assert!(lines[0].ends_with("1.25 KB"));
        assert!(lines[1].contains("1000.00 KB"));
        for marker in ["RX", "TX"] {
            let columns: Vec<_> = lines
                .iter()
                .map(|line| line[..line.find(marker).unwrap()].width())
                .collect();
            assert_eq!(columns[0], columns[1]);
        }
    }
    for width in 0..38 {
        assert!(
            network_text(&state, width)
                .lines()
                .all(|line| line.width() <= width)
        );
    }
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
                "propagation" => DirectoryKind::Propagation,
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
    if let Some(path) = cli.log_file.as_deref() {
        logging::init(path)?;
    }
    let config = AppConfig::from_cli(Cli {
        listen: "127.0.0.1:8080".parse().expect("constant socket address"),
        allow_remote: false,
        auth_token_file: None,
        offline: cli.offline,
        rns_config: cli.rns_config,
        state_dir: cli.state_dir,
    })?;
    let layout_path = config.database_path.with_file_name("tui.yaml");
    let (saved_layout, layout_error) = match layout::load(&layout_path) {
        Ok(saved) => (saved, None),
        Err(error) => (
            None,
            Some(format!(
                "Could not load {}: {error}. The file was left unchanged.",
                layout_path.display()
            )),
        ),
    };
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

    let mut app = TuiApp::with_layout(
        Box::new(CrosstermBackend::new()?),
        shared.clone(),
        updates,
        saved_layout,
    );
    let _ = app.run(shared, command_sender);
    let save_result = if layout_error.is_none() {
        layout::save(&layout_path, &app.layout.borrow())
    } else {
        Ok(())
    };
    drop(app);
    tokio.block_on(async {
        bridge.abort();
        let _ = bridge.await;
        core.shutdown().await;
    });
    if let Some(error) = layout_error {
        eprintln!("{error}");
    }
    save_result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tv::HeadlessBackend;

    #[test]
    fn main_windows_can_be_reopened_without_duplicates() {
        let (backend, screen) = HeadlessBackend::new(100, 30);
        let shared = Rc::new(RefCell::new(UiState::default()));
        let (_sender, updates) = update_channel();
        let mut app = TuiApp::with_layout(
            Box::new(backend),
            shared.clone(),
            updates,
            Some(layout::Layout::default()),
        );
        let (sender, _commands) = tokio::sync::mpsc::unbounded_channel();
        for command in [
            OPEN_CONVERSATIONS,
            OPEN_CONVERSATIONS,
            OPEN_NETWORK,
            OPEN_NETWORK,
            OPEN_DIRECTORY,
            OPEN_DIRECTORY,
            Command::CLOSE,
            OPEN_DIRECTORY,
            OPEN_DIRECTORY,
            Command::QUIT,
        ] {
            screen.push_event(Event::Command(command));
        }
        app.run(shared, sender);
        let saved = app.layout.borrow();
        assert_eq!(saved.windows.len(), 3);
        for key in ["conversations", "network", "directory"] {
            assert_eq!(
                saved
                    .windows
                    .iter()
                    .filter(|window| window.key == key)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn windows_menu_opens_network_with_its_mnemonic() {
        let (backend, screen) = HeadlessBackend::new(100, 30);
        let shared = Rc::new(RefCell::new(UiState::default()));
        let (_sender, updates) = update_channel();
        let mut app = TuiApp::with_layout(
            Box::new(backend),
            shared.clone(),
            updates,
            Some(layout::Layout::default()),
        );
        let (sender, _commands) = tokio::sync::mpsc::unbounded_channel();
        screen.push_key(
            Key::Char('w'),
            KeyModifiers {
                alt: true,
                ..Default::default()
            },
        );
        screen.push_key(Key::Char('e'), KeyModifiers::default());
        screen.push_event(Event::Command(Command::QUIT));
        app.run(shared, sender);
        assert_eq!(app.layout.borrow().windows.len(), 1);
        assert_eq!(app.layout.borrow().windows[0].key, "network");
    }

    #[test]
    fn existing_main_window_is_focused_without_changing_geometry() {
        let (backend, _) = HeadlessBackend::new(100, 30);
        let shared = Rc::new(RefCell::new(UiState::default()));
        let (_sender, updates) = update_channel();
        let mut app = TuiApp::new(Box::new(backend), shared, updates);
        for _ in 0..8 {
            app.program.pump_once();
        }
        let before = serde_yaml::to_string(&app.layout.borrow().windows).unwrap();
        assert!(app.layout.borrow_mut().focus_existing("network"));
        std::thread::sleep(Duration::from_millis(120));
        for _ in 0..8 {
            app.program.pump_once();
        }
        assert_eq!(app.layout.borrow().active.as_deref(), Some("network"));
        assert_eq!(
            serde_yaml::to_string(&app.layout.borrow().windows).unwrap(),
            before
        );
    }

    pub(super) fn with_context(run: impl FnOnce(&mut Context)) {
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
            Ok(UiCommand::BrowserFetch(request)) if request.url == node_index_url(destination_hash)
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
