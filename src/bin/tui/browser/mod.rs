use super::*;
use rsnomadnet_core::browser::NomadUrl;
use std::sync::mpsc;
use std::time::Instant;
use tv::{Color, Point, Style};
use unicode_segmentation::UnicodeSegmentation;

mod render;
#[cfg(test)]
mod tests;
use render::{Control, Layout};

pub(super) const HELP: tv::help::HelpCtx = tv::help::HelpCtx::custom("nomadnet.browser");
pub(super) const BACK: Command = Command::custom("browser.back");
pub(super) const FORWARD: Command = Command::custom("browser.forward");
pub(super) const RELOAD: Command = Command::custom("browser.reload");
pub(super) const STOP: Command = Command::custom("browser.stop");

pub(super) struct Request {
    pub url: String,
    reload: bool,
    fields: BTreeMap<String, String>,
    generation: u64,
    fragment: Option<String>,
    cancel: tokio::sync::watch::Receiver<bool>,
    response: mpsc::Sender<Reply>,
}

struct Reply {
    generation: u64,
    fragment: Option<String>,
    result: Result<BrowserPage, String>,
}

impl Request {
    pub(super) fn reject(self, error: &str) {
        let _ = self.response.send(Reply {
            generation: self.generation,
            fragment: self.fragment,
            result: Err(error.into()),
        });
    }

    pub(super) async fn execute(mut self, service: AppService) {
        if *self.cancel.borrow() {
            return;
        }
        let result = tokio::select! {
            _ = self.cancel.changed() => return,
            result = tokio::time::timeout(Duration::from_secs(190), service.fetch_page(FetchPage {
                url: self.url, reload: self.reload, fields: self.fields,
            })) => result.map_err(|_| "Page request timed out".to_owned()).and_then(|r| r.map_err(|e| e.to_string())),
        };
        let _ = self.response.send(Reply {
            generation: self.generation,
            fragment: self.fragment,
            result,
        });
    }
}

pub(super) struct Fragment {
    page: Option<BrowserPage>,
    error: Option<String>,
    pending: bool,
    next: Instant,
    interval: u64,
}

struct Session {
    url: String,
    page: Option<BrowserPage>,
    form: HashMap<String, Control>,
    fragments: HashMap<String, Fragment>,
    layout: Layout,
    columns: usize,
    dirty: bool,
    top: usize,
    left: usize,
    focus: Option<String>,
    status: String,
    loading: bool,
    generation: u64,
    cancel: tokio::sync::watch::Sender<bool>,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    sender: mpsc::Sender<Reply>,
    replies: mpsc::Receiver<Reply>,
    history: Vec<String>,
    position: usize,
    stopped: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
    }
}

fn resolve(current: &str, target: &str) -> Result<String, String> {
    let target = if target.starts_with(':') {
        format!("{}{}", current.split(':').next().unwrap_or(""), target)
    } else if target.starts_with("/page/") || target.starts_with("/file/") {
        format!("{}:{target}", current.split(':').next().unwrap_or(""))
    } else {
        target.to_owned()
    };
    NomadUrl::parse(&target)
        .map(|url| url.canonical())
        .map_err(|e| e.to_string())
}

impl Session {
    fn new(url: String, commands: tokio::sync::mpsc::UnboundedSender<UiCommand>) -> Self {
        let (sender, replies) = mpsc::channel();
        let (cancel, _) = tokio::sync::watch::channel(false);
        Self {
            url,
            page: None,
            form: HashMap::new(),
            fragments: HashMap::new(),
            layout: Layout::default(),
            columns: 0,
            dirty: true,
            top: 0,
            left: 0,
            focus: None,
            status: "Loading…".into(),
            loading: false,
            generation: 0,
            cancel,
            commands,
            sender,
            replies,
            history: Vec::new(),
            position: 0,
            stopped: false,
        }
    }

    fn request(
        &self,
        url: String,
        reload: bool,
        fields: BTreeMap<String, String>,
        fragment: Option<String>,
    ) {
        let request = Request {
            url,
            reload,
            fields,
            generation: self.generation,
            fragment,
            cancel: self.cancel.subscribe(),
            response: self.sender.clone(),
        };
        if let Err(error) = self.commands.send(UiCommand::BrowserFetch(request)) {
            if let UiCommand::BrowserFetch(request) = error.0 {
                request.reject("Application worker stopped");
            }
        }
    }

    fn navigate(
        &mut self,
        target: &str,
        reload: bool,
        fields: BTreeMap<String, String>,
        record: bool,
    ) {
        let base = self
            .page
            .as_ref()
            .map(|page| page.url.as_str())
            .unwrap_or(&self.url);
        let url = match resolve(base, target) {
            Ok(url) => url,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        if !NomadUrl::parse(&url).is_ok_and(|url| url.is_page()) {
            self.status = "This is a file link; use the web frontend to download files".into();
            return;
        }
        let _ = self.cancel.send(true);
        self.cancel = tokio::sync::watch::channel(false).0;
        self.generation += 1;
        self.loading = true;
        self.stopped = false;
        self.fragments.clear();
        self.status = format!("Loading {url}");
        self.url = url.clone();
        if record && self.history.get(self.position) != Some(&url) {
            self.history.truncate(if self.history.is_empty() {
                0
            } else {
                self.position + 1
            });
            self.history.push(url.clone());
            self.position = self.history.len() - 1;
        }
        self.request(url, reload, fields, None);
    }

    fn go_history(&mut self, forward: bool) {
        let index = if forward {
            self.position + 1
        } else {
            self.position.saturating_sub(1)
        };
        if let Some(url) = self.history.get(index).cloned() {
            if index != self.position {
                self.position = index;
                self.navigate(&url, false, BTreeMap::new(), false);
            }
        }
    }

    fn stop(&mut self) {
        let _ = self.cancel.send(true);
        self.loading = false;
        self.stopped = true;
        self.generation += 1;
        self.status = "Stopped".into();
    }

    fn poll(&mut self) {
        while let Ok(reply) = self.replies.try_recv() {
            if reply.generation != self.generation {
                continue;
            }
            if let Some(key) = reply.fragment {
                if let Some(fragment) = self.fragments.get_mut(&key) {
                    fragment.pending = false;
                    match reply.result {
                        Ok(page) => {
                            fragment.page = Some(page);
                            fragment.error = None;
                        }
                        Err(error) => {
                            fragment.error = Some(error.clone());
                            self.status = format!("Partial: {error}");
                        }
                    }
                    fragment.next = Instant::now() + Duration::from_secs(fragment.interval.max(1));
                }
            } else {
                self.loading = false;
                match reply.result {
                    Ok(page) => {
                        self.status = format!(
                            "{} — {}",
                            if page.from_cache {
                                "Cached"
                            } else {
                                "Received"
                            },
                            page.title.as_deref().unwrap_or(&page.url)
                        );
                        self.url = page.url.clone();
                        self.page = Some(page);
                        self.form.clear();
                        self.top = 0;
                        self.left = 0;
                        self.focus = None;
                    }
                    Err(error) => self.status = format!("Load failed: {error}"),
                }
            }
            self.dirty = true;
        }
    }

    fn layout(&mut self, columns: usize) {
        if self.dirty || self.columns != columns {
            self.columns = columns;
            self.layout = self
                .page
                .as_ref()
                .map(|page| render::render(page, columns, &mut self.form, &self.fragments))
                .unwrap_or_default();
            self.form
                .retain(|key, _| self.layout.controls.contains(key));
            self.fragments
                .retain(|key, _| self.layout.partials.iter().any(|(path, _)| path == key));
            self.dirty = false;
        }
    }

    fn fields(&self, names: &[String]) -> BTreeMap<String, String> {
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        for assignment in names {
            if let Some((name, value)) = assignment.split_once('=') {
                if !name.is_empty() {
                    fields.insert(format!("var_{name}"), value.into());
                }
            }
        }
        for id in &self.layout.controls {
            let Some(control) = self.form.get(id) else {
                continue;
            };
            let (name, value, selected) = match control {
                Control::Input { name, value, .. } => (name, value, true),
                Control::Choice {
                    name,
                    value,
                    checked,
                    ..
                } => (name, value, *checked),
                _ => continue,
            };
            if !selected || !(names.iter().any(|s| s == "*") || names.contains(name)) {
                continue;
            }
            let key = format!("field_{name}");
            match control {
                Control::Choice { .. } => {
                    fields
                        .entry(key)
                        .and_modify(|s| {
                            s.push(',');
                            s.push_str(value);
                        })
                        .or_insert_with(|| value.clone());
                }
                _ => {
                    fields.entry(key).or_insert_with(|| value.clone());
                }
            }
        }
        fields
    }

    fn partials(&mut self, forced: Option<&str>) {
        if self.loading || self.stopped {
            return;
        }
        for (key, block) in self.layout.partials.clone() {
            if self
                .fragments
                .values()
                .filter(|fragment| fragment.pending)
                .count()
                >= 4
            {
                break;
            }
            let MicronBlock::Partial {
                target,
                interval_seconds,
                fields,
            } = block
            else {
                continue;
            };
            let id = fields
                .iter()
                .find_map(|s| s.strip_prefix("pid="))
                .unwrap_or("");
            if forced.is_some_and(|requested| !requested.is_empty() && requested != id) {
                continue;
            }
            let initial = !self.fragments.contains_key(&key);
            let fragment = self
                .fragments
                .entry(key.clone())
                .or_insert_with(|| Fragment {
                    page: None,
                    error: None,
                    pending: false,
                    next: Instant::now(),
                    interval: interval_seconds,
                });
            if fragment.pending
                || (!initial
                    && forced.is_none()
                    && (interval_seconds == 0 || fragment.next > Instant::now()))
            {
                continue;
            }
            fragment.pending = true;
            let url = match resolve(&self.url, &target) {
                Ok(url) => url,
                Err(error) => {
                    fragment.pending = false;
                    fragment.error = Some(error);
                    fragment.next = Instant::now() + Duration::from_secs(interval_seconds.max(1));
                    self.dirty = true;
                    continue;
                }
            };
            self.request(url, true, self.fields(&fields), Some(key));
        }
    }

    fn activate(&mut self, id: &str, shared: &Shared, ctx: &mut Context) {
        if self.loading {
            return;
        }
        let Some(control) = self.form.get(id).cloned() else {
            return;
        };
        match control {
            Control::Link { target, fields } => {
                if target.to_lowercase().starts_with("lxmf@") {
                    let hash = target[5..].to_lowercase();
                    if hash.len() == 32 && hex::decode(&hash).is_ok() {
                        shared.borrow_mut().selected_destination_hash = Some(hash);
                        ctx.put_event(Event::Command(OPEN_CONVERSATION));
                    } else {
                        self.status = "Invalid LXMF address".into();
                    }
                } else if let Some(anchor) = target.strip_prefix('#') {
                    if anchor.is_empty() {
                        if let Some(row) = self.layout.headings.iter().find(|row| **row > self.top)
                        {
                            self.top = *row;
                        }
                    } else if let Some(row) = self.layout.anchors.get(anchor) {
                        self.top = *row;
                    } else {
                        self.status = format!("Anchor not found: {anchor}");
                    }
                } else if let Some(partial) = target.strip_prefix("p:") {
                    self.partials(Some(partial));
                } else {
                    self.navigate(&target, false, self.fields(&fields), true);
                }
            }
            Control::Choice {
                name,
                checked,
                radio,
                ..
            } => {
                if radio {
                    for value in self.form.values_mut() {
                        if let Control::Choice {
                            name: other,
                            checked,
                            radio: true,
                            ..
                        } = value
                        {
                            if *other == name {
                                *checked = false;
                            }
                        }
                    }
                }
                if let Some(Control::Choice { checked: value, .. }) = self.form.get_mut(id) {
                    *value = radio || !checked;
                }
                self.dirty = true;
            }
            Control::Input { .. } => {}
        }
    }

    fn edit(&mut self, event: &Event) -> bool {
        let Some(Control::Input { value, cursor, .. }) =
            self.focus.as_ref().and_then(|id| self.form.get_mut(id))
        else {
            return false;
        };
        let mut chars: Vec<String> = value.graphemes(true).map(str::to_owned).collect();
        *cursor = (*cursor).min(chars.len());
        let insert = match event {
            Event::Paste(text) => Some(text.clone()),
            Event::KeyDown(key) if !key.modifiers.alt && !key.modifiers.ctrl => match key.key {
                Key::Char(ch) if !ch.is_control() => Some(ch.to_string()),
                Key::Backspace => {
                    if *cursor > 0 {
                        *cursor -= 1;
                        chars.remove(*cursor);
                    }
                    None
                }
                Key::Delete => {
                    if *cursor < chars.len() {
                        chars.remove(*cursor);
                    }
                    None
                }
                Key::Left => {
                    *cursor = cursor.saturating_sub(1);
                    None
                }
                Key::Right => {
                    *cursor = (*cursor + 1).min(chars.len());
                    None
                }
                Key::Home => {
                    *cursor = 0;
                    None
                }
                Key::End => {
                    *cursor = chars.len();
                    None
                }
                _ => return false,
            },
            _ => return false,
        };
        if let Some(insert) = insert {
            let clean: String = insert.chars().filter(|ch| !ch.is_control()).collect();
            if value.len() + clean.len() <= 16384 {
                let added: Vec<String> = clean.graphemes(true).map(str::to_owned).collect();
                let count = added.len();
                chars.splice(*cursor..*cursor, added);
                *cursor += count;
            }
        }
        *value = chars.concat();
        self.dirty = true;
        true
    }
}

type SharedSession = Rc<RefCell<Session>>;

struct PageView {
    state: ViewState,
    session: SharedSession,
    shared: Shared,
    origin: Point,
}

impl View for PageView {
    fn state(&self) -> &ViewState {
        &self.state
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.state
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.origin = ctx.origin();
        let extent = self.state.get_extent();
        let width = extent.b.x.saturating_sub(1).max(1) as usize;
        let height = extent.b.y.max(1) as usize;
        let mut session = self.session.borrow_mut();
        session.layout(width);
        session.top = session
            .top
            .min(session.layout.rows.len().saturating_sub(height));
        session.left = session.left.min(session.layout.width.saturating_sub(width));
        let style = render::base_style(session.page.as_ref());
        ctx.fill(extent, ' ', style);
        self.state.state.cursor_vis = false;
        for (y, row) in session
            .layout
            .rows
            .iter()
            .skip(session.top)
            .take(height)
            .enumerate()
        {
            let mut x = -(session.left as i32);
            for glyph in row {
                let focused = self.state.state.focused
                    && glyph.control.is_some()
                    && glyph.control == session.focus;
                let style = if focused {
                    glyph.style.reversed()
                } else {
                    glyph.style
                };
                if x + glyph.width as i32 > 0 && x < width as i32 {
                    ctx.sub(Rect::new(0, 0, width as i32, height as i32))
                        .put_str(x, y as i32, &glyph.text, style);
                    if focused && glyph.caret && x >= 0 {
                        self.state.state.cursor_vis = true;
                        self.state.set_cursor(x, y as i32);
                    }
                }
                x += glyph.width as i32;
            }
        }
        let track = Style::new(Color::Bios(8), style.bg);
        let thumb = session.top * height / session.layout.rows.len().max(1);
        for y in 0..height {
            ctx.put_char(
                width as i32,
                y as i32,
                if y == thumb { '█' } else { '│' },
                track,
            );
        }
        if session.page.is_none() {
            ctx.put_str(1, 1, &session.status, style);
        }
    }

    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        let mut session = self.session.borrow_mut();
        let extent = self.state.get_extent();
        let height = extent.b.y.max(1) as usize;
        session.layout(extent.b.x.saturating_sub(1).max(1) as usize);
        if session.edit(event) {
            event.clear();
            return;
        }
        let mut activate = None;
        match event {
            Event::KeyDown(key) => match key.key {
                Key::Tab => {
                    let order = &session.layout.controls;
                    if !order.is_empty() {
                        let index = session
                            .focus
                            .as_ref()
                            .and_then(|id| order.iter().position(|v| v == id));
                        let next = if key.modifiers.shift {
                            index
                                .map(|i| (i + order.len() - 1) % order.len())
                                .unwrap_or(order.len() - 1)
                        } else {
                            index.map(|i| (i + 1) % order.len()).unwrap_or(0)
                        };
                        let id = order[next].clone();
                        if let Some(row) = session
                            .layout
                            .rows
                            .iter()
                            .position(|row| row.iter().any(|g| g.control.as_ref() == Some(&id)))
                        {
                            if row < session.top || row >= session.top + height {
                                session.top = row;
                            }
                        }
                        session.focus = Some(id);
                    }
                }
                Key::Enter | Key::Char(' ') => activate = session.focus.clone(),
                Key::Up => session.top = session.top.saturating_sub(1),
                Key::Down => session.top += 1,
                Key::PageUp => session.top = session.top.saturating_sub(height.saturating_sub(1)),
                Key::PageDown => session.top += height.saturating_sub(1),
                Key::Home => {
                    session.top = 0;
                    session.left = 0;
                }
                Key::End => session.top = session.layout.rows.len().saturating_sub(height),
                Key::Left if key.modifiers.shift => session.left = session.left.saturating_sub(4),
                Key::Right if key.modifiers.shift => session.left += 4,
                Key::Left => session.go_history(false),
                Key::Right => session.go_history(true),
                _ => return,
            },
            Event::MouseWheel(mouse) if self.state.state.focused => match mouse.wheel {
                tv::event::MouseWheel::Up => session.top = session.top.saturating_sub(3),
                tv::event::MouseWheel::Down => session.top += 3,
                tv::event::MouseWheel::Left => session.left = session.left.saturating_sub(4),
                tv::event::MouseWheel::Right => session.left += 4,
                _ => return,
            },
            Event::MouseDown(mouse) if mouse.buttons.left => {
                let x = mouse.position.x - self.origin.x;
                let y = mouse.position.y - self.origin.y;
                if x == extent.b.x - 1 {
                    session.top = (y.max(0) as usize * session.layout.rows.len() / height)
                        .min(session.layout.rows.len().saturating_sub(height));
                } else if let Some(row) = session.layout.rows.get(session.top + y.max(0) as usize) {
                    let mut column = 0;
                    for glyph in row {
                        if (x.max(0) as usize + session.left) < column + glyph.width {
                            activate = glyph.control.clone();
                            break;
                        }
                        column += glyph.width;
                    }
                    session.focus = activate.clone();
                }
            }
            _ => return,
        }
        if let Some(id) = activate {
            session.activate(&id, &self.shared, ctx);
        }
        event.clear();
    }
}

struct Address {
    input: InputLine,
    session: SharedSession,
    last_url: String,
    page: ViewId,
}

#[delegate(to = input)]
impl View for Address {
    fn draw(&mut self, ctx: &mut DrawCtx) {
        let url = self.session.borrow().url.clone();
        if self.last_url != url {
            self.input.set_value(FieldValue::Text(url.clone()));
            self.last_url = url;
        }
        self.input.draw(ctx);
    }
    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        if matches!(event, Event::KeyDown(key) if key.key == Key::Enter) {
            if let Some(FieldValue::Text(url)) = self.input.value() {
                self.session
                    .borrow_mut()
                    .navigate(&url, false, BTreeMap::new(), true);
            }
            ctx.request_focus(self.page);
            event.clear();
        } else {
            self.input.handle_event(event, ctx);
        }
    }
}

struct BrowserStatus {
    text: StaticText,
    session: SharedSession,
}
#[delegate(to = text)]
impl View for BrowserStatus {
    fn draw(&mut self, ctx: &mut DrawCtx) {
        let session = self.session.borrow();
        let status = match session.focus.as_ref().and_then(|id| session.form.get(id)) {
            Some(Control::Link { target, .. }) if !session.loading => {
                format!("Enter: {target} | {}", session.status)
            }
            _ => session.status.clone(),
        };
        self.text.set_text(status);
        self.text.draw(ctx);
    }
}

pub(super) struct BrowserWindow {
    window: Dialog,
    session: SharedSession,
    address: ViewId,
    page: ViewId,
    seeded: bool,
}

#[delegate(to = window)]
impl View for BrowserWindow {
    fn get_help_ctx(&self) -> tv::help::HelpCtx {
        HELP
    }

    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        if !self.seeded {
            self.seeded = true;
            ctx.request_focus(self.page);
        }
        {
            let mut session = self.session.borrow_mut();
            session.poll();
            if matches!(event, Event::Timer(_)) {
                session.partials(None);
            }
        }
        let command = match event {
            Event::Command(command) => Some(*command),
            Event::KeyDown(key) if key.modifiers.alt && key.key == Key::Left => Some(BACK),
            Event::KeyDown(key) if key.modifiers.alt && key.key == Key::Right => Some(FORWARD),
            Event::KeyDown(key) if key.modifiers.ctrl && key.key == Key::Char('r') => Some(RELOAD),
            Event::KeyDown(key) if key.key == Key::Esc => Some(STOP),
            Event::KeyDown(key) if key.modifiers.ctrl && key.key == Key::Char('l') => {
                ctx.request_focus(self.address);
                event.clear();
                return;
            }
            _ => None,
        };
        match command {
            Some(BACK) => self.session.borrow_mut().go_history(false),
            Some(FORWARD) => self.session.borrow_mut().go_history(true),
            Some(RELOAD) => {
                let url = self.session.borrow().url.clone();
                self.session
                    .borrow_mut()
                    .navigate(&url, true, BTreeMap::new(), false);
            }
            Some(STOP) => self.session.borrow_mut().stop(),
            _ => {
                self.window.handle_event(event, ctx);
                return;
            }
        }
        ctx.request_focus(self.page);
        event.clear();
    }
}

pub(super) fn window(
    desktop: Rect,
    shared: Shared,
    destination: &str,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
) -> BrowserWindow {
    let width = (desktop.b.x - desktop.a.x - 2).clamp(44, 100);
    let height = (desktop.b.y - desktop.a.y - 2).clamp(10, 32);
    let title = format!(
        "NomadNet Browser — {}",
        directory_title(&shared.borrow(), destination)
    );
    let mut window = Dialog::new(Rect::new(1, 1, width + 1, height + 1), Some(title));
    window.set_flags(WindowFlags {
        r#move: true,
        grow: true,
        close: true,
        zoom: true,
    });
    window.set_min_size(Point::new(44, 10));
    let url = node_index_url(destination);
    let session = Rc::new(RefCell::new(Session::new(url.clone(), commands)));
    session
        .borrow_mut()
        .navigate(&url, false, BTreeMap::new(), true);
    let mut state = ViewState::new(Rect::new(1, 2, width - 1, height - 2));
    state.options.selectable = true;
    state.grow_mode = GrowMode {
        hi_x: true,
        hi_y: true,
        ..Default::default()
    };
    let page = window.insert_child(Box::new(PageView {
        state,
        session: session.clone(),
        shared,
        origin: Point::new(0, 0),
    }));
    let mut address = Address {
        input: InputLine::with_limit(Rect::new(1, 1, width - 1, 2), 2048),
        session: session.clone(),
        last_url: String::new(),
        page,
    };
    address.state_mut().grow_mode.hi_x = true;
    let address = window.insert_child(Box::new(address));
    let mut status = BrowserStatus {
        text: StaticText::new(Rect::new(1, height - 2, width - 1, height - 1), ""),
        session: session.clone(),
    };
    status.state_mut().grow_mode = GrowMode {
        hi_x: true,
        lo_y: true,
        hi_y: true,
        ..Default::default()
    };
    window.insert_child(Box::new(status));
    BrowserWindow {
        window,
        session,
        address,
        page,
        seeded: false,
    }
}
