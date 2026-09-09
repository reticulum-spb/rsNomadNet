use super::*;
use std::cell::Cell;

#[derive(Clone)]
pub(super) struct Filters(Rc<Cell<u32>>);

impl Default for Filters {
    fn default() -> Self {
        Self(Rc::new(Cell::new(0b1111)))
    }
}

impl Filters {
    pub(super) fn includes(&self, kind: DirectoryKind) -> bool {
        let bit = match kind {
            DirectoryKind::Peer => 1,
            DirectoryKind::Propagation => 2,
            DirectoryKind::Rrc => 4,
            DirectoryKind::Node => 8,
            DirectoryKind::Other => return false,
        };
        self.0.get() & bit != 0
    }
}

struct FilterBoxes {
    boxes: tv::CheckBoxes,
    filters: Filters,
}

#[delegate(to = boxes)]
impl View for FilterBoxes {
    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.boxes.cluster.value = self.filters.0.get();
        self.boxes.draw(ctx);
    }
    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        self.boxes.cluster.value = self.filters.0.get();
        self.boxes.handle_event(event, ctx);
        let value = self.boxes.cluster.value;
        if self.filters.0.replace(value) != value {
            ctx.put_event(Event::Broadcast {
                command: REFRESH,
                source: None,
            });
        }
    }
}

pub(super) struct DirectoryWindow {
    window: Window,
    filters: Filters,
    list: ViewId,
    list_pane: ViewId,
    seeded: bool,
}

impl DirectoryWindow {
    fn focus_list(&mut self, ctx: &mut Context) {
        self.window.focus_descendant(self.list_pane, ctx);
        self.window.focus_descendant(self.list, ctx);
    }
}

#[delegate(to = window)]
impl View for DirectoryWindow {
    fn settle_currency(&mut self, ctx: &mut Context) {
        self.window.settle_currency(ctx);
        if !self.seeded {
            self.seeded = true;
            self.focus_list(ctx);
        }
    }

    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        if matches!(event, Event::MouseWheel(_)) {
            // TVision broadcasts wheel events; only the active Directory owns them.
            if !self.window.state().state.active {
                return;
            }
            self.focus_list(ctx);
        }
        if let Event::KeyDown(key) = event {
            if key.modifiers.alt && !key.modifiers.ctrl {
                let bit = match key.key {
                    Key::Char('p' | 'P') => Some(1),
                    Key::Char('o' | 'O') => Some(2),
                    Key::Char('r' | 'R') => Some(4),
                    Key::Char('n' | 'N') => Some(8),
                    _ => None,
                };
                if let Some(bit) = bit {
                    self.filters.0.set(self.filters.0.get() ^ bit);
                    ctx.put_event(Event::Broadcast {
                        command: REFRESH,
                        source: None,
                    });
                    self.focus_list(ctx);
                    event.clear();
                    return;
                }
            }
        }
        self.window.handle_event(event, ctx);
    }
}

pub(super) fn window(mut window: Window, state: Shared) -> DirectoryWindow {
    window.set_palette(WindowPalette::Blue);
    window.set_min_size(tv::Point::new(48, 8));
    let extent = window.state().get_extent();
    let interior = Rect::new(1, 1, extent.b.x - 1, extent.b.y - 1);
    let mut panels = tv::Splitter::rows().joined();
    panels.state_mut().options.first_click = true;
    let filters = Filters::default();
    let mut boxes = tv::CheckBoxes::new(
        Rect::new(1, 1, extent.b.x - 1, 2),
        vec![
            "~P~eer".into(),
            "Pr~o~p".into(),
            "~R~RC".into(),
            "~N~ode".into(),
        ],
    );
    boxes.cluster.value = filters.0.get();
    boxes.state_mut().grow_mode.hi_x = true;
    panels.insert(
        Box::new(FilterBoxes {
            boxes,
            filters: filters.clone(),
        }),
        tv::Constraints::fixed(1),
    );
    let width = (interior.b.x - interior.a.x).max(2);
    let height = (interior.b.y - interior.a.y - 2).max(1);
    let mut list_pane = tv::Group::new(Rect::new(0, 0, width, height));
    list_pane.state_mut().options.selectable = true;
    list_pane.state_mut().options.first_click = true;
    let mut scrollbar = ScrollBar::new(Rect::new(width - 1, 0, width, height));
    scrollbar.state_mut().grow_mode = GrowMode {
        lo_x: true,
        hi_x: true,
        hi_y: true,
        ..Default::default()
    };
    let scrollbar = list_pane.insert(Box::new(scrollbar));
    let mut list = StateList::with_scrollbar(
        Rect::new(0, 0, width - 1, height),
        state,
        Pane::Directory,
        Some(scrollbar),
    );
    list.filters = Some(filters.clone());
    let list = list_pane.insert(Box::new(list));
    let list_pane = panels.insert(Box::new(list_pane), tv::Constraints::flex().min(1));
    panels.change_bounds(interior);
    window.insert_child(Box::new(panels));
    DirectoryWindow {
        window,
        filters,
        list,
        list_pane,
        seeded: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_and_scrollbar_move_the_directory_list() {
        let (backend, screen) = tv::HeadlessBackend::new(100, 30);
        let state = shared();
        state.borrow_mut().directory = (0..60)
            .map(|i| DirectoryRow {
                destination_hash: format!("{i:032x}"),
                title: format!("entry-{i:02}"),
                label: format!("entry-{i:02}"),
                kind: DirectoryKind::Peer,
            })
            .collect();
        let (_sender, updates) = update_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);
        screen.push_key(
            Key::Char('3'),
            KeyModifiers {
                alt: true,
                ..Default::default()
            },
        );
        for _ in 0..8 {
            app.program.pump_once();
        }
        for _ in 0..5 {
            screen.push_event(Event::MouseWheel(tv::event::MouseEvent {
                wheel: tv::event::MouseWheel::Down,
                ..Default::default()
            }));
        }
        for _ in 0..100 {
            app.program.pump_once();
        }
        assert_eq!(
            state.borrow().selected_directory_hash.as_deref(),
            Some(format!("{:032x}", 15).as_str())
        );
        assert!(!screen.snapshot().contains("entry-00"));
        screen.push_event(Event::MouseWheel(tv::event::MouseEvent {
            wheel: tv::event::MouseWheel::Up,
            ..Default::default()
        }));
        for _ in 0..30 {
            app.program.pump_once();
        }
        assert_eq!(
            state.borrow().selected_directory_hash.as_deref(),
            Some(format!("{:032x}", 12).as_str())
        );
        let window = app
            .layout
            .borrow()
            .windows
            .iter()
            .find(|w| w.key == "directory")
            .unwrap()
            .clone();
        let mouse = tv::event::MouseEvent {
            position: tv::Point::new(window.x + window.width - 2, window.y + window.height - 1),
            buttons: tv::event::MouseButtons {
                left: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let before = screen.snapshot();
        assert!(
            matches!(
                screen
                    .buffer()
                    .get(mouse.position.x as u16, mouse.position.y as u16)
                    .symbol(),
                "▼" | "↓"
            ),
            "bar bottom at {:?}: {:?}",
            mouse.position,
            screen
                .buffer()
                .get(mouse.position.x as u16, mouse.position.y as u16)
                .symbol()
        );
        screen.push_event(Event::MouseDown(mouse));
        for _ in 0..20 {
            app.program.pump_once();
        }
        screen.push_event(Event::MouseUp(tv::event::MouseEvent {
            buttons: Default::default(),
            ..mouse
        }));
        for _ in 0..60 {
            app.program.pump_once();
        }
        assert_ne!(
            screen.snapshot(),
            before,
            "scrollbar arrow must update the list"
        );
        assert_eq!(
            state.borrow().selected_directory_hash.as_deref(),
            Some(format!("{:032x}", 13).as_str())
        );
        // Leaving the list for the filter pane must not disable wheel navigation.
        screen.push_key(Key::Tab, KeyModifiers::default());
        screen.push_event(Event::MouseWheel(tv::event::MouseEvent {
            wheel: tv::event::MouseWheel::Up,
            ..Default::default()
        }));
        for _ in 0..40 {
            app.program.pump_once();
        }
        assert_eq!(
            state.borrow().selected_directory_hash.as_deref(),
            Some(format!("{:032x}", 10).as_str())
        );
    }

    #[test]
    fn native_panels_keep_separator_joined_through_resize_and_zoom() {
        let (backend, screen) = tv::HeadlessBackend::new(100, 30);
        let (_sender, updates) = update_channel();
        let mut app = TuiApp::new(Box::new(backend), shared(), updates);
        screen.push_key(
            Key::Char('3'),
            KeyModifiers {
                alt: true,
                ..Default::default()
            },
        );
        for _ in 0..8 {
            app.program.pump_once();
        }
        let check = |app: &TuiApp| {
            let store = app.layout.borrow();
            let window = store.windows.iter().find(|w| w.key == "directory").unwrap();
            let buffer = screen.buffer();
            // Desktop begins below the application menu.
            let y = (window.y + 3) as u16;
            let x = window.x as u16;
            assert_eq!(buffer.get(x, y).symbol(), "╟");
            assert_eq!(buffer.get(x + window.width as u16 - 1, y).symbol(), "╢");
            for column in 1..window.width as u16 - 1 {
                assert_eq!(buffer.get(x + column, y).symbol(), "─");
            }
            let first_row: String = (1..window.width as u16 - 1)
                .map(|column| buffer.get(x + column, y + 1).symbol())
                .collect();
            assert!(first_row.contains("target-0"), "{first_row}");
        };
        check(&app);
        screen.push_event(Event::Command(Command::RESIZE));
        screen.push_key(
            Key::Down,
            KeyModifiers {
                shift: true,
                ..Default::default()
            },
        );
        screen.push_key(
            Key::Left,
            KeyModifiers {
                shift: true,
                ..Default::default()
            },
        );
        screen.push_key(Key::Enter, KeyModifiers::default());
        for _ in 0..12 {
            app.program.pump_once();
        }
        check(&app);
        screen.push_event(Event::Command(Command::ZOOM));
        for _ in 0..8 {
            app.program.pump_once();
        }
        check(&app);
        for (width, height) in [(120, 40), (80, 24), (100, 30)] {
            screen.resize(width, height);
            for _ in 0..12 {
                app.program.pump_once();
            }
            check(&app);
        }
        screen.push_event(Event::Command(Command::ZOOM));
        for _ in 0..8 {
            app.program.pump_once();
        }
        check(&app);
    }

    #[test]
    fn filters_share_one_row_with_separator_above_list() {
        let (backend, screen) = tv::HeadlessBackend::new(100, 30);
        let (_sender, updates) = update_channel();
        let mut app = TuiApp::new(Box::new(backend), shared(), updates);
        screen.push_key(
            Key::Char('3'),
            KeyModifiers {
                alt: true,
                ..Default::default()
            },
        );
        for _ in 0..12 {
            app.program.pump_once();
        }
        let buffer = screen.buffer();
        let rows: Vec<String> = (0..buffer.height())
            .map(|y| buffer.row(y).iter().map(|cell| cell.symbol()).collect())
            .collect();
        let y = rows
            .iter()
            .position(|row| {
                ["Peer", "Prop", "RRC", "Node"]
                    .iter()
                    .all(|label| row.contains(label))
            })
            .expect("all filters must fit on one row");
        assert!(rows[y + 1].contains("╟────────────────"));
        assert!(rows[y + 1].contains('╢'));
        assert!(rows[y + 2].contains("target-0"));
    }

    fn shared() -> Shared {
        Rc::new(RefCell::new(UiState {
            directory: [
                DirectoryKind::Peer,
                DirectoryKind::Propagation,
                DirectoryKind::Rrc,
                DirectoryKind::Node,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, kind)| DirectoryRow {
                destination_hash: format!("{i:032x}"),
                title: format!("target-{i}"),
                label: format!("target-{i}"),
                kind,
            })
            .collect(),
            ..Default::default()
        }))
    }

    #[test]
    fn alt_shortcuts_toggle_each_kind_and_empty_list_does_not_open_stale_target() {
        let (backend, screen) = tv::HeadlessBackend::new(100, 30);
        let state = shared();
        let (_sender, updates) = update_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);
        let (sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        screen.push_key(
            Key::Char('3'),
            KeyModifiers {
                alt: true,
                ..Default::default()
            },
        );
        for key in ['p', 'o', 'r', 'n'] {
            screen.push_key(
                Key::Char(key),
                KeyModifiers {
                    alt: true,
                    ..Default::default()
                },
            );
        }
        screen.push_key(Key::Enter, KeyModifiers::default());
        screen.push_event(Event::Command(Command::QUIT));
        app.run(state, sender);
        assert!(commands.try_recv().is_err());
        let frame = screen.snapshot();
        for i in 0..4 {
            assert!(!frame.contains(&format!("target-{i}")), "{frame}");
        }
        assert!(frame.contains("Prop"));
    }

    #[test]
    fn enter_on_filtered_rrc_uses_its_identity_and_kind() {
        let (backend, screen) = tv::HeadlessBackend::new(100, 30);
        let state = shared();
        let (_sender, updates) = update_channel();
        let mut app = TuiApp::new(Box::new(backend), state.clone(), updates);
        let (sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        for key in ['3', 'p', 'o'] {
            screen.push_key(
                Key::Char(key),
                KeyModifiers {
                    alt: true,
                    ..Default::default()
                },
            );
        }
        screen.push_key(Key::Enter, KeyModifiers::default());
        screen.push_event(Event::Command(Command::QUIT));
        app.run(state, sender);
        assert!(
            matches!(commands.try_recv(), Ok(UiCommand::ConnectRrc {destination_hash}) if destination_hash == format!("{:032x}", 2))
        );
    }

    #[test]
    fn selection_survives_filter_changes_and_updates() {
        let state = shared();
        let filters = Filters::default();
        let mut list = StateList::new(Rect::new(0, 0, 40, 8), state.clone(), Pane::Directory);
        list.filters = Some(filters.clone());
        let mut events = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred = Vec::new();
        let mut ctx = Context::new(&mut events, &mut timers, 0, &mut deferred);
        let refresh = || Event::Broadcast {
            command: REFRESH,
            source: None,
        };
        list.handle_event(&mut refresh(), &mut ctx);
        list.list.set_value_ctx(FieldValue::Int(2), &mut ctx);
        filters.0.set(0b1100);
        list.handle_event(&mut refresh(), &mut ctx);
        assert_eq!(list.focused_destination(), Some(format!("{:032x}", 2)));
        assert_eq!(list.focused_directory_kind(), Some(DirectoryKind::Rrc));
        state.borrow_mut().directory.reverse();
        list.handle_event(&mut refresh(), &mut ctx);
        assert_eq!(list.focused_destination(), Some(format!("{:032x}", 2)));
        filters.0.set(15);
        list.handle_event(&mut refresh(), &mut ctx);
        assert_eq!(list.row_ids.len(), 4);
        assert_eq!(list.focused_destination(), Some(format!("{:032x}", 2)));
    }
}
