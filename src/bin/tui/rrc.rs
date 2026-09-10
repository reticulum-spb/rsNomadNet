use super::*;
use std::collections::HashSet;
type MessageKey = (String, u64, String, String);

pub(super) fn hub_lines(service: &AppService, hub: RrcHubView) -> Vec<String> {
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

pub(super) fn message_line(message: RrcMessageView) -> String {
    if server_message(&message) {
        return message.body;
    }
    use unicode_segmentation::UnicodeSegmentation;
    let nick = message
        .nick
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&message.source_hash);
    let parts: Vec<_> = nick.graphemes(true).collect();
    let nick = if parts.len() > 15 {
        format!(
            "{}...{}",
            parts[..8].concat(),
            parts[parts.len() - 4..].concat()
        )
    } else {
        nick.to_string()
    };
    format!("{nick}: {}", message.body)
}

fn server_message(message: &RrcMessageView) -> bool {
    if message.source_hash.is_empty() || message.source_hash == message.hub_hash {
        return true;
    }
    let Ok(bytes) = hex::decode(&message.source_hash) else {
        return false;
    };
    let Ok(identity) = <[u8; 16]>::try_from(bytes.as_slice()) else {
        return false;
    };
    hex::encode(
        rns_identity::destination::Destination::hash_from_name_and_identity(
            "rrc.hub",
            Some(&identity),
        ),
    ) == message.hub_hash
}

const ROOMS: Command = Command::custom("rrc.rooms");
const DISCONNECT: Command = Command::custom("rrc.disconnect");
pub(super) const CLEAR: Command = Command::custom("rrc.clear_history");
pub(super) const HELP: tv::help::HelpCtx = tv::help::HelpCtx::custom("rrc.window");

#[derive(Debug, PartialEq)]
enum Action {
    Clear(String),
    Register(String),
    Rooms,
    Disconnect,
    Join(String, Option<String>),
    Part(String),
    Nick(String),
    Users(String),
    Ping,
    Send(String, String, bool),
}

fn parse(text: &str, room: &str) -> Result<Action, String> {
    let text = text.trim();
    let (command, arg) = text.split_once(' ').unwrap_or((text, ""));
    let arg = arg.trim();
    match command {
        "/register" => {
            let room = if arg.is_empty() { room } else { arg }.trim_start_matches('#').to_ascii_lowercase();
            if room.is_empty() || room.chars().any(char::is_whitespace) {
                Err("Usage: /register [room]; select a room first if omitted".into())
            } else { Ok(Action::Register(room)) }
        }
        "/rooms" => Ok(Action::Rooms),
        "/disconnect" => Ok(Action::Disconnect),
        "/ping" => Ok(Action::Ping),
        "/join" if !arg.is_empty() => {
            let (name, key) = arg.split_once(' ').unwrap_or((arg, ""));
            Ok(Action::Join(name.trim_start_matches('#').to_ascii_lowercase(), (!key.trim().is_empty()).then(|| key.trim().into())))
        }
        "/part" => Ok(Action::Part(if arg.is_empty() { room } else { arg }.into())),
        "/users" => Ok(Action::Users(if arg.is_empty() { room } else { arg }.into())),
        "/nick" if !arg.is_empty() => Ok(Action::Nick(arg.into())),
        "/me" if !arg.is_empty() && !room.is_empty() => Ok(Action::Send(room.into(), arg.into(), true)),
        _ if text.starts_with('/') => Err("Commands: /join room [key], /register [room], /part [room], /nick name, /rooms, /users [room], /ping, /disconnect, /me text".into()),
        _ if text.is_empty() => Err("Enter a message or command".into()),
        _ if room.is_empty() => Err("Join a room first".into()),
        _ => Ok(Action::Send(room.into(), text.into(), false)),
    }
}

pub(super) struct Request {
    hub: String,
    action: Action,
    reply: tokio::sync::oneshot::Sender<Result<Answer, String>>,
}

enum Answer {
    Cleared(String),
    Disconnected,
    Status(String),
    Rooms(Vec<String>),
    Joined(String),
}

impl Request {
    pub(super) fn reject(self, error: &str) {
        let _ = self.reply.send(Err(error.into()));
    }
    pub(super) async fn execute(self, service: AppService) {
        let result = tokio::time::timeout(Duration::from_secs(45), async {
            let answer = match self.action {
                Action::Clear(room) => {
                    service.clear_rrc_history(&self.hub, &room)?;
                    Answer::Cleared(room)
                }
                Action::Register(room) => {
                    service.rrc_register(&self.hub, room).await?;
                    Answer::Status("Registration requested; see server reply".into())
                }
                Action::Rooms => Answer::Rooms(
                    service
                        .rrc_list_rooms(&self.hub)
                        .await?
                        .into_iter()
                        .map(|r| r.name)
                        .collect(),
                ),
                Action::Disconnect => {
                    service.rrc_disconnect(&self.hub).await?;
                    Answer::Disconnected
                }
                Action::Join(room, key) => {
                    service.rrc_join(&self.hub, room.clone(), key).await?;
                    Answer::Joined(room)
                }
                Action::Part(room) => {
                    service.rrc_part(&self.hub, room).await?;
                    Answer::Status("Part requested".into())
                }
                Action::Nick(nick) => {
                    service.rrc_set_nick(&self.hub, nick).await?;
                    Answer::Status("Nickname updated".into())
                }
                Action::Users(room) => Answer::Status(
                    service
                        .rrc_list_users(&self.hub, room)
                        .await?
                        .into_iter()
                        .map(|u| u.nick.unwrap_or(u.identity))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                Action::Ping => {
                    Answer::Status(format!("Pong: {} ms", service.rrc_ping(&self.hub).await?))
                }
                Action::Send(room, body, action) => {
                    service
                        .rrc_send(&self.hub, Some(room), body, action)
                        .await?;
                    Answer::Status("Message sent".into())
                }
            };
            Ok::<_, anyhow::Error>(answer)
        })
        .await
        .map_err(|_| "RRC operation timed out".to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
        let _ = self.reply.send(result);
    }
}

struct Pending {
    hub: String,
    draft_key: String,
    draft: Option<String>,
    reply: tokio::sync::oneshot::Receiver<Result<Answer, String>>,
}

struct Session {
    unread: HashMap<(String, String), usize>,
    seen: HashMap<String, HashSet<MessageKey>>,
    disconnected: HashMap<String, bool>,
    hub: String,
    room: HashMap<String, String>,
    drafts: HashMap<String, String>,
    rooms: HashMap<String, Vec<String>>,
    status: HashMap<String, String>,
    pending: Option<Pending>,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
}
type Local = Rc<RefCell<Session>>;

impl Session {
    fn label(&self, room: &str) -> String {
        match self
            .unread
            .get(&(self.hub.clone(), room.into()))
            .copied()
            .unwrap_or(0)
        {
            0 => room.to_string(),
            count => format!("{room} ({count})"),
        }
    }
    fn observe_messages(&mut self, state: &UiState, active: bool) {
        for (hub, messages) in &state.rrc_history {
            let current: HashSet<_> = messages
                .iter()
                .map(|m| {
                    (
                        m.room.clone().unwrap_or_default(),
                        m.timestamp_ms,
                        m.source_hash.clone(),
                        m.body.clone(),
                    )
                })
                .collect();
            if let Some(previous) = self.seen.insert(hub.clone(), current.clone()) {
                if !self.disconnected.contains_key(hub) {
                    let own = state
                        .rrc_hubs
                        .iter()
                        .find(|h| &h.destination_hash == hub)
                        .map(|h| h.local_identity.as_str());
                    for (room, _, source, _) in current.difference(&previous) {
                        if !room.is_empty() && own != Some(source.as_str()) {
                            *self.unread.entry((hub.clone(), room.clone())).or_default() += 1;
                        }
                    }
                }
            }
        }
        if active {
            if let Some(room) = self.room.get(&self.hub) {
                self.unread.remove(&(self.hub.clone(), room.clone()));
            }
        }
    }
    fn draft_key(&self) -> String {
        format!(
            "{}:{}",
            self.hub,
            self.room.get(&self.hub).map(String::as_str).unwrap_or("")
        )
    }
    fn send(&mut self, action: Action, draft: Option<String>) {
        if self.pending.is_some() {
            self.status.insert(
                self.hub.clone(),
                "Please wait for the current operation".into(),
            );
            return;
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .commands
            .send(UiCommand::Rrc(Request {
                hub: self.hub.clone(),
                action,
                reply,
            }))
            .is_err()
        {
            self.status
                .insert(self.hub.clone(), "RRC service stopped".into());
            return;
        }
        self.status.insert(self.hub.clone(), "Working…".into());
        self.pending = Some(Pending {
            draft_key: self.draft_key(),
            hub: self.hub.clone(),
            draft,
            reply: response,
        });
    }
    fn poll(&mut self) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let result = match pending.reply.try_recv() {
            Ok(result) => result,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(_) => Err("RRC worker stopped".into()),
        };
        let pending = self.pending.take().unwrap();
        match result {
            Ok(answer) => {
                if pending
                    .draft
                    .as_ref()
                    .is_some_and(|d| self.drafts.get(&pending.draft_key) == Some(d))
                {
                    self.drafts.remove(&pending.draft_key);
                }
                let status = match answer {
                    Answer::Cleared(room) => {
                        self.unread.remove(&(pending.hub.clone(), room));
                        "History cleared".into()
                    }
                    Answer::Disconnected => {
                        self.unread.retain(|(hub, _), _| hub != &pending.hub);
                        self.room.remove(&pending.hub);
                        self.rooms.remove(&pending.hub);
                        self.disconnected.insert(pending.hub.clone(), false);
                        "Disconnected".into()
                    }
                    Answer::Rooms(mut rooms) => {
                        rooms.sort();
                        rooms.dedup();
                        self.rooms.insert(pending.hub.clone(), rooms);
                        "Rooms updated".into()
                    }
                    Answer::Joined(room) => {
                        self.room.insert(pending.hub.clone(), room);
                        "Joined".into()
                    }
                    Answer::Status(status) => status,
                };
                self.status.insert(pending.hub, status);
            }
            Err(error) => {
                self.status.insert(pending.hub, format!("Error: {error}"));
            }
        }
    }
}

struct Servers {
    list: ListBox,
    shared: Shared,
    local: Local,
    hashes: Vec<String>,
}
#[delegate(to = list)]
impl View for Servers {
    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        let state = self.shared.borrow();
        let mut hashes: Vec<_> = state
            .rrc_hubs
            .iter()
            .filter(|h| h.connected)
            .map(|h| h.destination_hash.clone())
            .collect();
        let selected = self.local.borrow().hub.clone();
        if !hashes.contains(&selected) {
            hashes.push(selected.clone());
        }
        hashes.sort();
        hashes.dedup();
        let rows: Vec<_> = hashes
            .iter()
            .map(|hash| {
                let hub = state.rrc_hubs.iter().find(|h| &h.destination_hash == hash);
                format!(
                    "{} {}",
                    if hub.is_some_and(|h| h.connected) {
                        "+"
                    } else {
                        "-"
                    },
                    hub.and_then(|h| h.name.clone())
                        .unwrap_or_else(|| directory_title(&state, hash).to_string())
                )
            })
            .collect();
        if matches!(event, Event::KeyDown(k) if k.key == Key::Enter)
            && !state
                .rrc_hubs
                .iter()
                .any(|h| h.destination_hash == selected && h.connected)
        {
            let _ = self.local.borrow().commands.send(UiCommand::ConnectRrc {
                destination_hash: selected.clone(),
            });
            event.clear();
        }
        drop(state);
        let selected_hash = match self.list.value() {
            Some(FieldValue::Int(index)) => self.hashes.get(index as usize),
            _ => None,
        };
        if self.hashes != hashes || self.list.list() != rows || selected_hash != Some(&selected) {
            self.list.new_list(rows, ctx);
            self.list.set_value_ctx(
                FieldValue::Int(hashes.iter().position(|h| h == &selected).unwrap_or(0) as i32),
                ctx,
            );
            self.hashes = hashes;
        }
        self.list.handle_event(event, ctx);
        if let Some(FieldValue::Int(index)) = self.list.value() {
            if let Some(hash) = self.hashes.get(index as usize) {
                self.local.borrow_mut().hub = hash.clone();
            }
        }
    }
}

struct Rooms {
    group: tv::Group,
    shared: Shared,
    local: Local,
    buttons: Vec<(ViewId, String)>,
    signature: (String, Vec<String>),
    offset: i32,
}

struct RoomRadio {
    radio: tv::RadioButtons,
    room: String,
    local: Local,
    shared: Shared,
    shown: u32,
}
impl RoomRadio {
    fn sync(&mut self) {
        // Mouse tracking applies the native cluster press after event dispatch.
        // Observe it before restoring the confirmed selection, including in draw.
        if self.radio.cluster.value == 0 && self.shown != 0 {
            let mut local = self.local.borrow_mut();
            let hub = local.hub.clone();
            let joined = !local.disconnected.contains_key(&hub)
                && self.shared.borrow().rrc_hubs.iter().any(|h| {
                    h.destination_hash == hub && h.connected && h.rooms.contains(&self.room)
                });
            if joined {
                local.room.insert(hub.clone(), self.room.clone());
                local.unread.remove(&(hub, self.room.clone()));
            } else {
                local.send(Action::Join(self.room.clone(), None), None);
            }
        }
        let local = self.local.borrow();
        self.radio.cluster.strings[0] = local.label(&self.room).replace('~', "");
        self.radio.cluster.value = if local.room.get(&local.hub) == Some(&self.room) {
            0
        } else {
            u32::MAX
        };
        self.shown = self.radio.cluster.value;
    }
}
#[delegate(to = radio)]
impl View for RoomRadio {
    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.sync();
        self.radio.draw(ctx);
    }
    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        self.sync();
        self.radio.handle_event(event, ctx);
        self.sync();
    }
}
#[delegate(to = group)]
impl View for Rooms {
    fn draw(&mut self, ctx: &mut DrawCtx) {
        ctx.fill(
            self.group.state().get_extent(),
            ' ',
            ctx.style(tv::theme::Role::ClusterNormal),
        );
        self.group.draw(ctx);
    }
    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        let mut local = self.local.borrow_mut();
        let hub = local.hub.clone();
        let mut rooms = local.rooms.get(&hub).cloned().unwrap_or_default();
        if let Some(server) = self
            .shared
            .borrow()
            .rrc_hubs
            .iter()
            .find(|h| h.destination_hash == hub && !local.disconnected.contains_key(&hub))
        {
            rooms.extend(server.rooms.clone());
            if let Some(room) = server.rooms.first() {
                local
                    .room
                    .entry(hub.clone())
                    .or_insert_with(|| room.clone());
            }
        }
        rooms.sort();
        rooms.dedup();
        if local.disconnected.contains_key(&hub) {
            rooms.clear();
        }
        drop(local);
        if self.signature != (hub.clone(), rooms.clone()) {
            for (id, _) in self.buttons.drain(..) {
                self.group.remove(id, ctx);
            }
            self.offset = 0;
            let mut x = 0;
            for room in &rooms {
                let width =
                    (unicode_width::UnicodeWidthStr::width(room.as_str()) as i32 + 6).max(8);
                let id = self.group.insert(Box::new(RoomRadio {
                    radio: tv::RadioButtons::new(
                        Rect::new(x, 0, x + width, 1),
                        vec![room.replace('~', "")],
                    ),
                    room: room.clone(),
                    local: self.local.clone(),
                    shared: self.shared.clone(),
                    shown: 0,
                }));
                self.buttons.push((id, room.clone()));
                x += width;
            }
            self.signature = (hub, rooms);
        }
        // Tab traversal scrolls the horizontal button strip to keep focus visible.
        let mut x = -self.offset;
        for (id, room) in &self.buttons {
            let label = self.local.borrow().label(room);
            let width = (unicode_width::UnicodeWidthStr::width(label.as_str()) as i32 + 6).max(8);
            if let Some(view) = self.group.find_mut(*id) {
                view.change_bounds(Rect::new(x, 0, x + width, 1));
            }
            x += width;
        }
        self.group.handle_event(event, ctx);
        let width = self.group.state().get_extent().b.x;
        let mut shift = 0;
        for (id, _) in &self.buttons {
            if let Some(button) = self.group.find_mut(*id) {
                if button.state().state.focused {
                    let bounds = button.state().get_bounds();
                    shift = if bounds.a.x < 0 || bounds.b.x - bounds.a.x >= width {
                        -bounds.a.x
                    } else if bounds.b.x > width {
                        width - bounds.b.x
                    } else {
                        0
                    };
                    break;
                }
            }
        }
        if shift != 0 {
            self.offset -= shift;
            for (id, _) in &self.buttons {
                if let Some(button) = self.group.find_mut(*id) {
                    let mut bounds = button.state().get_bounds();
                    bounds.a.x += shift;
                    bounds.b.x += shift;
                    button.change_bounds(bounds);
                }
            }
        }
    }
}

struct History {
    list: ListBox,
    shared: Shared,
    local: Local,
    target: (String, String),
}

struct SessionInfo {
    text: StaticText,
    shared: Shared,
    local: Local,
}
#[delegate(to = text)]
impl View for SessionInfo {
    fn draw(&mut self, ctx: &mut DrawCtx) {
        let local = self.local.borrow();
        let state = self.shared.borrow();
        let status = local
            .status
            .get(&local.hub)
            .cloned()
            .or_else(|| {
                state
                    .rrc_hubs
                    .iter()
                    .find(|h| h.destination_hash == local.hub)
                    .map(|h| h.detail.clone())
            })
            .or_else(|| {
                state
                    .directory_views
                    .get(&format!("rrc:{}", local.hub))
                    .map(|lines| lines.join(" "))
            })
            .unwrap_or_default();
        self.text.set_text(status);
        self.text.draw(ctx);
    }
}
#[delegate(to = list)]
impl View for History {
    fn apply_scroll_sync(&mut self, h: Option<i32>, v: Option<i32>, ctx: &mut Context) {
        self.list.apply_scroll_sync(h, v, ctx);
        ctx.put_event(Event::Nothing);
    }
    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        let local = self.local.borrow();
        let hub = &local.hub;
        let room = local.room.get(hub).cloned().unwrap_or_default();
        let state = self.shared.borrow();
        let lines: Vec<_> = state
            .rrc_history
            .get(hub)
            .into_iter()
            .flatten()
            .filter(|_| !local.disconnected.contains_key(hub))
            .filter(|m| m.room.as_deref().is_none_or(|r| r == room))
            .cloned()
            .map(rrc_message_line)
            .collect();
        let target = (hub.clone(), room);
        drop(local);
        drop(state);
        if self.list.list() != lines || self.target != target {
            let selection = self.list.value();
            let follow = self.target != target
                || matches!(selection, Some(FieldValue::Int(i)) if i as usize >= self.list.list().len().saturating_sub(1));
            self.list.new_list(lines, ctx);
            tv::widgets::list_viewer::update_steps(&self.list, ctx);
            if follow {
                self.list.set_value_ctx(
                    FieldValue::Int(self.list.list().len().saturating_sub(1) as i32),
                    ctx,
                );
            } else if let Some(value) = selection {
                self.list.set_value_ctx(value, ctx);
            }
            self.target = target;
        }
        self.list.handle_event(event, ctx);
    }
}

struct Input {
    input: InputLine,
    local: Local,
}
#[delegate(to = input)]
impl View for Input {
    fn draw(&mut self, ctx: &mut DrawCtx) {
        let local = self.local.borrow();
        let value = FieldValue::Text(
            local
                .drafts
                .get(&local.draft_key())
                .cloned()
                .unwrap_or_default(),
        );
        if self.input.value() != Some(value.clone()) {
            self.input.set_value(value);
        }
        self.input.draw(ctx);
    }
    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        let mut local = self.local.borrow_mut();
        let hub = local.hub.clone();
        let draft_key = local.draft_key();
        let draft = local.drafts.get(&draft_key).cloned().unwrap_or_default();
        if self.input.value() != Some(FieldValue::Text(draft.clone())) {
            self.input
                .set_value_ctx(FieldValue::Text(draft.clone()), ctx);
        }
        if matches!(event, Event::KeyDown(k) if k.key == Key::Enter) {
            let room = local.room.get(&hub).cloned().unwrap_or_default();
            match parse(&draft, &room) {
                Ok(action) => local.send(action, Some(draft)),
                Err(error) => {
                    local.status.insert(hub, error);
                }
            }
            event.clear();
            return;
        }
        self.input.handle_event(event, ctx);
        if let Some(FieldValue::Text(text)) = self.input.value() {
            local.drafts.insert(draft_key, text);
        }
    }
}

pub(super) struct RrcWindow {
    window: Window,
    local: Local,
    shared: Shared,
}
#[delegate(to = window)]
impl View for RrcWindow {
    fn get_help_ctx(&self) -> tv::help::HelpCtx {
        HELP
    }
    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        if let Some(hub) = self.shared.borrow_mut().rrc_focus.take() {
            self.local.borrow_mut().hub = hub;
        }
        self.local.borrow_mut().poll();
        {
            let state = self.shared.borrow();
            self.local
                .borrow_mut()
                .disconnected
                .retain(|hash, saw_offline| {
                    let online = state
                        .rrc_hubs
                        .iter()
                        .any(|hub| &hub.destination_hash == hash && hub.connected);
                    if !online {
                        *saw_offline = true;
                    }
                    !(*saw_offline && online)
                });
        }
        let clear = matches!(event, Event::Command(command) if *command == CLEAR)
            || matches!(event, Event::KeyDown(key) if key.modifiers.ctrl && !key.modifiers.alt && matches!(key.key, Key::Char('l' | 'L')));
        if clear {
            let mut local = self.local.borrow_mut();
            let hub = local.hub.clone();
            if let Some(room) = local
                .room
                .get(&hub)
                .filter(|room| !room.is_empty())
                .cloned()
            {
                local.send(Action::Clear(room), None);
            } else {
                local.status.insert(hub, "Select a room first".into());
            }
            event.clear();
        }
        if let Event::Command(command) = event {
            let action = if *command == ROOMS {
                Some(Action::Rooms)
            } else if *command == DISCONNECT {
                Some(Action::Disconnect)
            } else {
                None
            };
            if let Some(action) = action {
                self.local.borrow_mut().send(action, None);
                event.clear();
            }
        }
        self.window.handle_event(event, ctx);
        self.local
            .borrow_mut()
            .observe_messages(&self.shared.borrow(), self.window.state().state.active);
    }
}

pub(super) fn window(
    desktop: Rect,
    shared: Shared,
    hash: &str,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
) -> RrcWindow {
    let local = Rc::new(RefCell::new(Session {
        unread: HashMap::new(),
        seen: HashMap::new(),
        disconnected: HashMap::new(),
        hub: hash.into(),
        room: HashMap::new(),
        drafts: HashMap::new(),
        rooms: HashMap::new(),
        status: HashMap::new(),
        pending: None,
        commands,
    }));
    let width = (desktop.b.x - desktop.a.x - 2).clamp(50, 110);
    let height = (desktop.b.y - desktop.a.y - 2).clamp(12, 30);
    let mut window = Window::new(
        Rect::new(
            desktop.a.x + 1,
            desktop.a.y + 1,
            desktop.a.x + 1 + width,
            desktop.a.y + 1 + height,
        ),
        Some("RRC Hub".into()),
        0,
    );
    window.set_palette(WindowPalette::Blue);
    window.set_min_size(tv::Point::new(50, 12));
    let mut columns = tv::Splitter::cols().joined();
    let mut left = tv::Splitter::rows().joined();
    left.insert(
        Box::new(Servers {
            list: ListBox::new(Rect::new(0, 0, 22, 8), 1, None, None),
            shared: shared.clone(),
            local: local.clone(),
            hashes: Vec::new(),
        }),
        tv::Constraints::flex().min(2),
    );
    let mut actions = tv::Group::new(Rect::new(0, 0, 22, 4));
    actions.state_mut().options.selectable = true;
    actions.insert(Box::new(Button::new(
        Rect::new(0, 0, 22, 2),
        "~D~isconnect",
        DISCONNECT,
        ButtonFlags::default(),
    )));
    actions.insert(Box::new(Button::new(
        Rect::new(0, 2, 22, 4),
        "~R~ooms",
        ROOMS,
        ButtonFlags::default(),
    )));
    actions.change_bounds(Rect::new(0, 0, 22, 5));
    actions.insert(Box::new(SessionInfo {
        text: StaticText::new(Rect::new(0, 4, 22, 5), ""),
        shared: shared.clone(),
        local: local.clone(),
    }));
    left.insert(Box::new(actions), tv::Constraints::fixed(5));
    columns.insert(Box::new(left), tv::Constraints::fixed(22));
    let mut right = tv::Splitter::rows().joined();
    let mut room_group = tv::Group::new(Rect::new(0, 0, 50, 1));
    room_group.state_mut().options.selectable = true;
    right.insert(
        Box::new(Rooms {
            group: room_group,
            shared: shared.clone(),
            local: local.clone(),
            buttons: Vec::new(),
            signature: (String::new(), Vec::new()),
            offset: 0,
        }),
        tv::Constraints::fixed(1),
    );
    let mut history = tv::Group::new(Rect::new(0, 0, 50, 12));
    history.state_mut().options.selectable = true;
    let mut bar = ScrollBar::new(Rect::new(49, 0, 50, 12));
    bar.state_mut().grow_mode = GrowMode {
        lo_x: true,
        hi_x: true,
        hi_y: true,
        ..Default::default()
    };
    let bar = history.insert(Box::new(bar));
    let mut list = History {
        list: ListBox::new(Rect::new(0, 0, 49, 12), 1, None, Some(bar)),
        shared: shared.clone(),
        local: local.clone(),
        target: (String::new(), String::new()),
    };
    list.state_mut().grow_mode = GrowMode {
        hi_x: true,
        hi_y: true,
        ..Default::default()
    };
    history.insert(Box::new(list));
    right.insert(Box::new(history), tv::Constraints::flex().min(2));
    right.insert(
        Box::new(Input {
            input: InputLine::with_limit(Rect::new(0, 0, 50, 1), 4096),
            local: local.clone(),
        }),
        tv::Constraints::fixed(1),
    );
    columns.insert(Box::new(right), tv::Constraints::flex().min(20));
    columns.change_bounds(Rect::new(1, 1, width - 1, height - 1));
    window.insert_child(Box::new(columns));
    RrcWindow {
        window,
        local,
        shared,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ctrl_l_targets_selected_room_without_clearing_draft() {
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = Rc::new(RefCell::new(UiState::default()));
        let mut view = window(Rect::new(0, 0, 100, 30), state, "hub", commands);
        view.local
            .borrow_mut()
            .room
            .insert("hub".into(), "news".into());
        let key = view.local.borrow().draft_key();
        view.local
            .borrow_mut()
            .drafts
            .insert(key.clone(), "unfinished".into());
        crate::tests::with_context(|ctx| {
            view.handle_event(
                &mut Event::KeyDown(window_key(Key::Char('l'), true, false, false)),
                ctx,
            );
        });
        let UiCommand::Rrc(request) = receiver.try_recv().unwrap() else {
            panic!("wrong command")
        };
        assert_eq!(request.hub, "hub");
        assert_eq!(request.action, Action::Clear("news".into()));
        request
            .reply
            .send(Ok(Answer::Cleared("news".into())))
            .ok()
            .unwrap();
        view.local.borrow_mut().poll();
        assert_eq!(view.local.borrow().drafts[&key], "unfinished");
    }
    #[test]
    fn unread_counts_only_new_messages_and_clears_on_open() {
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        let state = Rc::new(RefCell::new(UiState::default()));
        let view = window(Rect::new(0, 0, 100, 30), state.clone(), "hub", commands);
        let mut local = view.local.borrow_mut();
        local.room.insert("hub".into(), "general".into());
        state.borrow_mut().rrc_history.insert("hub".into(), vec![]);
        local.observe_messages(&state.borrow(), true);
        for (i, room) in ["general", "news", "news"].iter().enumerate() {
            state
                .borrow_mut()
                .rrc_history
                .get_mut("hub")
                .unwrap()
                .push(RrcMessageView {
                    hub_hash: "hub".into(),
                    room: Some((*room).into()),
                    source_hash: "peer".into(),
                    nick: None,
                    body: "Hello".into(),
                    timestamp_ms: i as u64,
                    kind: "message".into(),
                });
        }
        local.observe_messages(&state.borrow(), true);
        local.observe_messages(&state.borrow(), true);
        assert_eq!(local.label("general"), "general");
        assert_eq!(local.label("news"), "news (2)");
        local.room.insert("hub".into(), "news".into());
        local.observe_messages(&state.borrow(), true);
        assert_eq!(local.label("news"), "news");
    }
    #[test]
    fn server_messages_have_no_address_prefix_but_peer_notices_do() {
        let identity = [9; 16];
        let hub = rns_identity::destination::Destination::hash_from_name_and_identity(
            "rrc.hub",
            Some(&identity),
        );
        let mut message = RrcMessageView {
            hub_hash: hex::encode(hub),
            room: None,
            source_hash: hex::encode(identity),
            nick: None,
            body: "Server notice".into(),
            timestamp_ms: 0,
            kind: "notice".into(),
        };
        assert_eq!(message_line(message.clone()), "Server notice");
        message.source_hash = hex::encode([8; 16]);
        assert!(message_line(message).ends_with(": Server notice"));
    }
    #[test]
    fn history_lines_only_contain_short_sender_and_body() {
        let mut message = RrcMessageView {
            hub_hash: "hub".into(),
            room: Some("general".into()),
            source_hash: "8aecd0211234567890a587".into(),
            nick: None,
            body: "Hello".into(),
            timestamp_ms: 0,
            kind: "message".into(),
        };
        assert_eq!(message_line(message.clone()), "8aecd021...a587: Hello");
        message.nick = Some("Alice".into());
        message.kind = "action".into();
        assert_eq!(message_line(message.clone()), "Alice: Hello");
        message.nick = Some("АлександрАлександрович".into());
        assert_eq!(message_line(message), "Александ...ович: Hello");
    }
    #[test]
    fn register_uses_current_or_explicit_room() {
        assert_eq!(
            parse("/register", "general").unwrap(),
            Action::Register("general".into())
        );
        assert_eq!(
            parse("/register #Rust", "general").unwrap(),
            Action::Register("rust".into())
        );
        assert!(parse("/register", "").is_err());
        assert!(parse("/register #", "general").is_err());
        assert!(parse("/register one two", "general").is_err());
    }
    #[test]
    fn join_keeps_current_room_until_confirmation_and_retains_failed_command() {
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let shared = Rc::new(RefCell::new(UiState::default()));
        let view = window(Rect::new(0, 0, 100, 30), shared, "hub", commands);
        let mut local = view.local.borrow_mut();
        local.room.insert("hub".into(), "old".into());
        let key = local.draft_key();
        local.drafts.insert(key.clone(), "/join new".into());
        local.send(Action::Join("new".into(), None), Some("/join new".into()));
        local.poll();
        assert_eq!(local.room["hub"], "old");
        let UiCommand::Rrc(request) = receiver.try_recv().unwrap() else {
            panic!("wrong command")
        };
        request.reject("Join timed out");
        local.poll();
        assert_eq!(local.room["hub"], "old");
        assert_eq!(local.drafts[&key], "/join new");
        local.send(Action::Join("new".into(), None), Some("/join new".into()));
        let UiCommand::Rrc(request) = receiver.try_recv().unwrap() else {
            panic!("wrong command")
        };
        request
            .reply
            .send(Ok(Answer::Joined("new".into())))
            .ok()
            .unwrap();
        local.poll();
        assert_eq!(local.room["hub"], "new");
        assert!(!local.drafts.contains_key(&key));
    }
    #[test]
    fn room_button_joins_its_own_room() {
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let shared = Rc::new(RefCell::new(UiState::default()));
        let view = window(Rect::new(0, 0, 100, 30), shared.clone(), "hub-a", commands);
        view.local
            .borrow_mut()
            .rooms
            .insert("hub-a".into(), vec!["general".into(), "news".into()]);
        let mut rooms = Rooms {
            group: tv::Group::new(Rect::new(0, 0, 40, 2)),
            shared,
            local: view.local.clone(),
            buttons: Vec::new(),
            signature: (String::new(), Vec::new()),
            offset: 0,
        };
        crate::tests::with_context(|ctx| {
            rooms.handle_event(&mut Event::Nothing, ctx);
            let source = rooms
                .buttons
                .iter()
                .find(|(_, name)| name == "news")
                .unwrap()
                .0;
            let radio = rooms.group.find_mut(source).unwrap();
            radio.state_mut().state.focused = true;
            radio.handle_event(&mut Event::KeyDown(KeyEvent::from(Key::Char(' '))), ctx);
        });
        let UiCommand::Rrc(request) = receiver.try_recv().unwrap() else {
            panic!("wrong command")
        };
        assert_eq!(request.hub, "hub-a");
        assert_eq!(request.action, Action::Join("news".into(), None));
        assert!(view.local.borrow().room.get("hub-a").is_none());
        request
            .reply
            .send(Ok(Answer::Joined("news".into())))
            .ok()
            .unwrap();
        view.local.borrow_mut().poll();
        assert_eq!(view.local.borrow().room["hub-a"], "news");
        rooms.shared.borrow_mut().rrc_hubs.push(RrcHubView {
            destination_hash: "hub-a".into(),
            local_identity: "local".into(),
            name: None,
            nick: None,
            version: None,
            supports_resources: false,
            supports_actions: false,
            supports_direct_notices: false,
            supports_room_state: false,
            supports_user_list: false,
            max_message_bytes: None,
            connected: true,
            rooms: vec!["general".into(), "news".into()],
            room_states: vec![],
            detail: String::new(),
        });
        view.local
            .borrow_mut()
            .room
            .insert("hub-a".into(), "general".into());
        crate::tests::with_context(|ctx| {
            let id = rooms
                .buttons
                .iter()
                .find(|(_, room)| room == "news")
                .unwrap()
                .0;
            let radio = rooms.group.find_mut(id).unwrap();
            radio.state_mut().state.focused = true;
            radio.handle_event(&mut Event::KeyDown(KeyEvent::from(Key::Char(' '))), ctx);
        });
        assert_eq!(view.local.borrow().room["hub-a"], "news");
        assert!(
            receiver.try_recv().is_err(),
            "joined room must not send another JOIN"
        );
    }
    #[test]
    fn panels_render_room_history_and_resize() {
        let (backend, screen) = tv::HeadlessBackend::new(120, 35);
        let (_sender, updates) = update_channel();
        let state = Rc::new(RefCell::new(UiState::default()));
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let hash = "ab".repeat(16);
        state.borrow_mut().rrc_history.insert(
            hash.clone(),
            vec![
                RrcMessageView {
                    hub_hash: hash.clone(),
                    room: Some("general".into()),
                    source_hash: "cd".repeat(16),
                    nick: Some("Alice".into()),
                    body: "Visible room message".into(),
                    timestamp_ms: 0,
                    kind: "message".into(),
                },
                RrcMessageView {
                    hub_hash: hash.clone(),
                    room: Some("private".into()),
                    source_hash: "cd".repeat(16),
                    nick: Some("Alice".into()),
                    body: "Hidden other room".into(),
                    timestamp_ms: 0,
                    kind: "message".into(),
                },
            ],
        );
        let view = window(app.program.desktop_rect(), state.clone(), &hash, commands);
        let local = view.local.clone();
        local
            .borrow_mut()
            .rooms
            .insert(hash.clone(), vec!["general".into(), "private".into()]);
        local
            .borrow_mut()
            .room
            .insert(hash.clone(), "general".into());
        app.program.desktop_insert(Box::new(view));
        for _ in 0..30 {
            app.program.pump_once();
        }
        let text = screen.snapshot();
        assert!(text.contains("Disconnect"));
        assert!(text.contains("Rooms"));
        assert!(text.contains("general"));
        assert!(text.contains("private"));
        assert!(text.contains("Visible room message"));
        assert!(!text.contains("Hidden other room"));
        screen.push_key(Key::F(5), KeyModifiers::default());
        for _ in 0..30 {
            app.program.pump_once();
        }
        assert!(screen.snapshot().contains("Visible room message"));
        assert!(receiver.try_recv().is_err());
        local.borrow_mut().send(Action::Disconnect, None);
        let UiCommand::Rrc(request) = receiver.try_recv().unwrap() else {
            panic!("wrong command")
        };
        request.reply.send(Ok(Answer::Disconnected)).ok().unwrap();
        screen.push_key(Key::Tab, KeyModifiers::default());
        for _ in 0..30 {
            app.program.pump_once();
        }
        assert!(!screen.snapshot().contains("Visible room message"));
        assert!(!screen.snapshot().contains("general"));
        assert!(!screen.snapshot().contains("private"));
        assert_eq!(
            state.borrow().rrc_history[&hash].len(),
            2,
            "history cache must not be deleted"
        );
    }

    #[test]
    fn failed_sends_preserve_draft_and_replies_remain_bound_to_original_hub() {
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let shared = Rc::new(RefCell::new(UiState::default()));
        let view = window(Rect::new(0, 0, 100, 30), shared, "hub-a", commands);
        let mut local = view.local.borrow_mut();
        local.room.insert("hub-a".into(), "room".into());
        let key = local.draft_key();
        local.drafts.insert(key.clone(), "hello".into());
        local.send(
            Action::Send("room".into(), "hello".into(), false),
            Some("hello".into()),
        );
        let UiCommand::Rrc(request) = receiver.try_recv().unwrap() else {
            panic!("wrong command")
        };
        assert_eq!(request.hub, "hub-a");
        local.hub = "hub-b".into();
        request.reply.send(Err("offline".into())).ok().unwrap();
        local.poll();
        assert_eq!(local.drafts.get(&key).map(String::as_str), Some("hello"));
        assert!(local.status["hub-a"].contains("offline"));
        assert!(!local.status.contains_key("hub-b"));
    }
    #[test]
    fn commands_are_explicit_and_text_requires_room() {
        assert_eq!(
            parse("/join #test secret", "").unwrap(),
            Action::Join("test".into(), Some("secret".into()))
        );
        assert_eq!(
            parse("hello", "#test").unwrap(),
            Action::Send("#test".into(), "hello".into(), false)
        );
        assert!(parse("hello", "").is_err());
        assert!(parse("/unknown text", "#test").is_err());
    }
}
