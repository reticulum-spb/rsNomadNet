use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use clap::Parser;
use rsnomadnet_core::Runtime;
use rsnomadnet_core::config::{AppConfig, Cli};
use rsnomadnet_core::models::{ConversationSummary, DirectoryEntry, NetworkSnapshot};
use rsnomadnet_core::service::AppService;
use tv::{
    Backend, Command, Context, CrosstermBackend, Desktop, DrawCtx, Event, ListBox, Menu, MenuBar,
    Program, Rect, StatusDef, StatusLine, SystemClock, Theme, View, ViewState, Window, alt,
    delegate,
};
use tvision_rs as tv;

const REFRESH: Command = Command::custom("rsnomadnet.refresh");

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
    conversations: Vec<String>,
    directory: Vec<String>,
}

type Shared = Rc<RefCell<UiState>>;

#[derive(Clone, Copy)]
enum Pane {
    Network,
    Conversations,
    Directory,
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
            Pane::Conversations => state.conversations.clone(),
            Pane::Directory => state.directory.clone(),
        }
    }
}

#[delegate(to = list)]
impl View for StateList {
    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }

    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
        let refresh = matches!(
            event,
            Event::Broadcast { command, .. } if *command == REFRESH
        );
        if !self.seeded || refresh {
            self.seeded = true;
            let lines = self.lines();
            self.list.new_list(lines, context);
        }
        self.list.handle_event(event, context);
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
        Self {
            state: ViewState::new(Rect::new(0, 0, 0, 0)),
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
            *self.shared.borrow_mut() = update;
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
        network.insert_child(Box::new(PumpView::new(state.clone(), updates)));

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

        desktop.insert_view(Box::new(conversations));
        desktop.insert_view(Box::new(network));
        desktop.insert_view(Box::new(directory));
        Some(Box::new(desktop))
    }

    fn status_line(mut bounds: Rect) -> Option<Box<dyn View>> {
        bounds.a.y = bounds.b.y - 1;
        let definitions = StatusDef::list()
            .def_all(|definition| definition.item("~Alt-X~ Exit", alt('x'), Command::QUIT))
            .build();
        Some(Box::new(StatusLine::new(bounds, definitions)))
    }

    fn menu_bar(mut bounds: Rect) -> Option<Box<dyn View>> {
        bounds.b.y = bounds.a.y + 1;
        let menu = Menu::builder()
            .submenu("~F~ile", alt('f'), |menu| {
                menu.command_key("E~x~it", Command::QUIT, alt('x'), "Alt-X")
            })
            .build();
        Some(Box::new(MenuBar::new(bounds, menu)))
    }

    fn run(&mut self) -> Command {
        self.program.run_app(|_, _| {})
    }
}

async fn snapshot(service: &AppService) -> UiState {
    let network = service.network_snapshot().await;
    let conversations = service.conversations().unwrap_or_default();
    let directory = service.directory().unwrap_or_default();
    UiState {
        network: network_lines(network),
        conversations: conversation_lines(conversations),
        directory: directory_lines(directory),
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

fn conversation_lines(conversations: Vec<ConversationSummary>) -> Vec<String> {
    if conversations.is_empty() {
        return vec!["No conversations".into()];
    }
    conversations
        .into_iter()
        .map(|conversation| {
            format!(
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
            )
        })
        .collect()
}

fn directory_lines(entries: Vec<DirectoryEntry>) -> Vec<String> {
    if entries.is_empty() {
        return vec!["No known destinations".into()];
    }
    entries
        .into_iter()
        .map(|entry| {
            format!(
                "{} {:<18} {}",
                if entry.active { "+" } else { "-" },
                entry.kind,
                entry
                    .display_name
                    .as_deref()
                    .unwrap_or(&entry.destination_hash)
            )
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
    let initial = tokio.block_on(snapshot(&service));
    let shared = Rc::new(RefCell::new(initial));
    let (sender, updates) = mpsc::channel();
    let bridge = tokio.spawn(async move {
        let mut events = service.subscribe();
        while events.recv().await.is_ok() {
            if sender.send(snapshot(&service).await).is_err() {
                break;
            }
        }
    });

    let mut app = TuiApp::new(Box::new(CrosstermBackend::new()?), shared, updates);
    let _ = app.run();

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
            conversations: vec!["Alice  hello".into()],
            directory: vec!["+ lxmf.delivery Alice".into()],
        }));
        let (_sender, receiver) = mpsc::channel();
        let mut app = TuiApp::new(Box::new(backend), state, receiver);

        app.program.pump_once();
        let frame = screen.snapshot();
        assert!(frame.contains("LXMF Conversations"));
        assert!(frame.contains("Network"));
        assert!(frame.contains("Directory"));
    }
}
