use super::*;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct SavedWindow {
    pub key: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Layout {
    version: u32,
    #[serde(default)]
    pub active: Option<String>,
    pub windows: Vec<SavedWindow>,
    #[serde(skip)]
    requested_focus: Option<String>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            version: 1,
            active: None,
            windows: Vec::new(),
            requested_focus: None,
        }
    }
}

impl Layout {
    pub(super) fn focus_existing(&mut self, key: &str) -> bool {
        if self.windows.iter().any(|window| window.key == key) {
            self.requested_focus = Some(key.into());
            true
        } else {
            false
        }
    }
}

pub(super) type Store = Rc<RefCell<Layout>>;

pub(super) fn load(path: &Path) -> anyhow::Result<Option<Layout>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 1024 * 1024, "tui.yaml exceeds 1 MiB");
    let mut layout: Layout = serde_yaml::from_slice(&bytes)?;
    anyhow::ensure!(
        layout.version == 1,
        "unsupported TUI layout version {}",
        layout.version
    );
    anyhow::ensure!(layout.windows.len() <= 128, "too many saved windows");
    let mut keys = std::collections::HashSet::new();
    layout
        .windows
        .retain(|window| valid_key(&window.key) && keys.insert(window.key.clone()));
    for window in &mut layout.windows {
        if !window.url.as_deref().is_some_and(|url| {
            rsnomadnet_core::browser::NomadUrl::parse(url).is_ok_and(|u| u.is_page())
        }) {
            window.url = None;
        }
    }
    Ok(Some(layout))
}

fn valid_key(key: &str) -> bool {
    if matches!(key, "network" | "directory" | "conversations") {
        return true;
    }
    key.split_once(':').is_some_and(|(kind, hash)| {
        matches!(kind, "node" | "rrc" | "lxmf")
            && hash.len() == 32
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub(super) fn save(path: &Path, layout: &Layout) -> anyhow::Result<()> {
    let bytes = serde_yaml::to_string(layout)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing layout directory"))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temporary = parent.join(format!(".tui-{}-{nonce}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn fit(saved: &SavedWindow, desktop: Rect, minimum: tv::Point) -> Rect {
    let available = tv::Point::new(
        (desktop.b.x - desktop.a.x).max(1),
        (desktop.b.y - desktop.a.y).max(1),
    );
    let width = saved
        .width
        .clamp(minimum.x.max(1).min(available.x), available.x);
    let height = saved
        .height
        .clamp(minimum.y.max(1).min(available.y), available.y);
    let x = saved.x.clamp(0, available.x - width);
    let y = saved.y.clamp(0, available.y - height);
    Rect::new(x, y, x + width, y + height)
}

pub(super) fn online(shared: &Shared) -> bool {
    shared
        .borrow()
        .network
        .iter()
        .any(|line| line == "State: Online")
}

pub(super) struct TrackedWindow {
    inner: Box<dyn View>,
    key: String,
    store: Store,
    focus_on_start: bool,
    pub pending: Option<(
        Shared,
        tokio::sync::mpsc::UnboundedSender<UiCommand>,
        UiCommand,
    )>,
}

impl TrackedWindow {
    pub fn new(
        mut inner: Box<dyn View>,
        key: String,
        store: Store,
        saved: Option<&Layout>,
        desktop: Rect,
    ) -> Self {
        if let Some(window) = saved.and_then(|s| s.windows.iter().find(|w| w.key == key)) {
            let minimum = inner
                .size_limits(tv::Point::new(
                    desktop.b.x - desktop.a.x,
                    desktop.b.y - desktop.a.y,
                ))
                .0;
            inner.change_bounds(fit(window, desktop, minimum));
        }
        let focus_on_start = saved.and_then(|s| s.active.as_ref()) == Some(&key);
        let mut result = Self {
            inner,
            key,
            store,
            focus_on_start,
            pending: None,
        };
        result.record();
        result
    }

    fn record(&mut self) {
        let rect = self.inner.state().get_bounds();
        let url = self
            .inner
            .as_any_mut()
            .and_then(|v| v.downcast_mut::<ManagedWindow>())
            .and_then(|v| v.browser_url());
        let entry = SavedWindow {
            key: self.key.clone(),
            x: rect.a.x,
            y: rect.a.y,
            width: rect.b.x - rect.a.x,
            height: rect.b.y - rect.a.y,
            url,
        };
        let mut store = self.store.borrow_mut();
        if let Some(old) = store.windows.iter_mut().find(|w| w.key == self.key) {
            *old = entry;
        } else {
            store.windows.push(entry);
        }
        if self.inner.state().state.active {
            store.active = Some(self.key.clone());
        }
    }
}

impl Drop for TrackedWindow {
    fn drop(&mut self) {
        let mut store = self.store.borrow_mut();
        store.windows.retain(|w| w.key != self.key);
        if store.active.as_ref() == Some(&self.key) {
            store.active = None;
        }
    }
}

#[delegate(to = inner)]
impl View for TrackedWindow {
    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.inner.draw(ctx);
        self.record();
    }
    fn change_bounds(&mut self, bounds: Rect) {
        self.inner.change_bounds(bounds);
        self.record();
    }
    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        {
            let mut store = self.store.borrow_mut();
            if store.requested_focus.as_deref() == Some(&self.key) {
                store.requested_focus = None;
                if let Some(id) = self.inner.state().id() {
                    ctx.request_focus(id);
                }
            }
        }
        if self.focus_on_start && matches!(event, Event::Timer(_)) {
            self.focus_on_start = false;
            if let Some(id) = self.state().id() {
                ctx.request_focus(id);
            }
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|(shared, _, _)| online(shared))
        {
            let (_, commands, command) = self.pending.take().unwrap();
            let _ = commands.send(command);
        }
        self.inner.handle_event(event, ctx);
        self.record();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str) -> SavedWindow {
        SavedWindow {
            key: key.into(),
            x: 4,
            y: 3,
            width: 50,
            height: 15,
            url: None,
        }
    }

    #[test]
    fn yaml_round_trip_atomic_replacement_and_empty_layout() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tui.yaml");
        assert!(load(&path).unwrap().is_none());
        let expected = Layout {
            version: 1,
            active: Some("directory".into()),
            windows: vec![entry("directory")],
            ..Default::default()
        };
        save(&path, &expected).unwrap();
        let actual = load(&path).unwrap().unwrap();
        assert_eq!(actual.active, expected.active);
        assert_eq!(actual.windows[0].width, 50);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        save(&path, &Layout::default()).unwrap();
        assert!(load(&path).unwrap().unwrap().windows.is_empty());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn corrupt_or_future_yaml_is_rejected_without_modification() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tui.yaml");
        for source in ["windows: [", "version: 99\nwindows: []\n"] {
            std::fs::write(&path, source).unwrap();
            assert!(load(&path).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        }
    }

    #[test]
    fn invalid_targets_and_duplicates_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tui.yaml");
        let layout = Layout {
            windows: vec![
                entry("directory"),
                entry("directory"),
                entry("node:invalid"),
                entry("unknown"),
            ],
            ..Default::default()
        };
        save(&path, &layout).unwrap();
        let saved = load(&path).unwrap().unwrap();
        assert_eq!(saved.windows.len(), 1);
        assert_eq!(saved.windows[0].key, "directory");
    }

    #[test]
    fn clamps_geometry_even_for_extreme_values_and_small_terminals() {
        let mut saved = entry("directory");
        saved.x = i32::MAX;
        saved.y = i32::MIN;
        saved.width = i32::MAX;
        saved.height = i32::MIN;
        assert_eq!(
            fit(&saved, Rect::new(0, 1, 30, 9), tv::Point::new(44, 10)),
            Rect::new(0, 0, 30, 8)
        );
    }

    #[test]
    fn headless_restores_geometry_and_does_not_reopen_closed_windows() {
        let (backend, screen) = tv::HeadlessBackend::new(100, 30);
        let shared = Rc::new(RefCell::new(UiState::default()));
        let (_sender, updates) = update_channel();
        let saved = Layout {
            active: Some("directory".into()),
            windows: vec![entry("directory")],
            ..Default::default()
        };
        let mut app = TuiApp::with_layout(Box::new(backend), shared, updates, Some(saved));
        for _ in 0..12 {
            app.program.pump_once();
        }
        let snapshot = app.layout.borrow().clone();
        assert_eq!(snapshot.windows.len(), 1);
        let window = &snapshot.windows[0];
        assert_eq!(
            (window.x, window.y, window.width, window.height),
            (4, 3, 50, 15)
        );
        assert_eq!(snapshot.active.as_deref(), Some("directory"));
        screen.push_event(Event::Command(Command::ZOOM));
        for _ in 0..4 {
            app.program.pump_once();
        }
        assert!(app.layout.borrow().windows[0].width > 50);
        screen.push_event(Event::Command(Command::CLOSE));
        for _ in 0..4 {
            app.program.pump_once();
        }
        assert!(app.layout.borrow().windows.is_empty());
    }

    #[test]
    fn headless_restores_dynamic_windows_without_loading_offline() {
        let (backend, screen) = tv::HeadlessBackend::new(100, 30);
        let shared = Rc::new(RefCell::new(UiState::default()));
        let (_sender, updates) = update_channel();
        let hash = "00112233445566778899aabbccddeeff";
        let mut browser = entry(&format!("node:{hash}"));
        browser.url = Some(format!("{hash}:/page/saved.mu"));
        let saved = Layout {
            windows: vec![
                entry(&format!("lxmf:{hash}")),
                entry(&format!("rrc:{hash}")),
                browser,
            ],
            ..Default::default()
        };
        let mut app = TuiApp::with_layout(Box::new(backend), shared.clone(), updates, Some(saved));
        let (sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        screen.push_event(Event::Command(Command::QUIT));
        app.run(shared.clone(), sender);
        assert_eq!(app.layout.borrow().windows.len(), 3);
        assert!(matches!(
            commands.try_recv(),
            Ok(UiCommand::OpenConversation { .. })
        ));
        assert!(
            commands.try_recv().is_err(),
            "Network requests must wait for Online"
        );
        assert!(
            app.layout
                .borrow()
                .windows
                .iter()
                .any(|w| w.url.as_deref() == Some(&format!("{hash}:/page/saved.mu")))
        );
    }

    #[test]
    fn headless_restores_active_window_after_initial_focus() {
        let (backend, _) = tv::HeadlessBackend::new(100, 30);
        let shared = Rc::new(RefCell::new(UiState::default()));
        let (_sender, updates) = update_channel();
        let saved = Layout {
            active: Some("network".into()),
            windows: vec![entry("network"), entry("directory"), entry("conversations")],
            ..Default::default()
        };
        let mut app = TuiApp::with_layout(Box::new(backend), shared, updates, Some(saved));
        for _ in 0..4 {
            app.program.pump_once();
        }
        std::thread::sleep(Duration::from_millis(120));
        for _ in 0..12 {
            app.program.pump_once();
        }
        assert_eq!(app.layout.borrow().active.as_deref(), Some("network"));
    }
}
