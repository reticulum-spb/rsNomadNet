use super::*;
use rsnomadnet_core::browser::{Alignment, MicronStyle, parse_page};

const URL: &str = "00112233445566778899aabbccddeeff:/page/index.mu";

fn page(source: &str) -> BrowserPage {
    parse_page(URL.into(), source.as_bytes(), false).unwrap()
}

fn session(source: &str) -> (Session, tokio::sync::mpsc::UnboundedReceiver<UiCommand>) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut session = Session::new(URL.into(), sender);
    session.page = Some(page(source));
    session.layout(60);
    (session, receiver)
}

fn lines(layout: &Layout) -> Vec<String> {
    layout
        .rows
        .iter()
        .map(|row| row.iter().map(|g| g.text.as_str()).collect())
        .collect()
}

#[test]
fn renders_styles_unicode_alignment_and_blank_lines() {
    let mut p = page("Привет 世界\n\n`c`Ff00`!Bold`!\n>>Раздел\n");
    let layout = render::render(&p, 24, &mut HashMap::new(), &HashMap::new());
    assert_eq!(lines(&layout)[0], "Привет 世界");
    assert!(layout.rows[1].is_empty());
    assert_eq!(layout.rows[0].iter().map(|g| g.width).sum::<usize>(), 11);
    let bold = layout.rows[2].iter().find(|g| g.text == "B").unwrap();
    assert_eq!(bold.style.fg, Color::Rgb(255, 0, 0));
    assert!(bold.style.modifiers.bold);
    assert_eq!(bold.style.bg, Color::Bios(0));
    assert!(lines(&layout)[2].starts_with("          Bold"));
    assert_eq!(layout.anchors.get("раздел"), Some(&3));
    p.background = Some("#123456".into());
    assert_eq!(render::base_style(Some(&p)).bg, Color::Rgb(18, 52, 86));
}

#[test]
fn reflow_keeps_unicode_and_literal_and_table_layout() {
    let mut p = page("Привет 世界 длинная строка\n");
    p.blocks.push(MicronBlock::Preformatted {
        text: "a\tb\n01234567890123456789".into(),
    });
    p.blocks.push(MicronBlock::Table {
        alignment: Alignment::Left,
        max_width: None,
        column_alignments: vec![Alignment::Left, Alignment::Right],
        rows: vec![vec![
            vec![Inline::Text {
                text: "A".into(),
                style: MicronStyle::default(),
            }],
            vec![Inline::Text {
                text: "B".into(),
                style: MicronStyle::default(),
            }],
        ]],
    });
    let narrow = render::render(&p, 12, &mut HashMap::new(), &HashMap::new());
    let wide = render::render(&p, 60, &mut HashMap::new(), &HashMap::new());
    assert!(narrow.rows.len() > wide.rows.len());
    assert!(lines(&narrow).contains(&"a   b".into()));
    assert!(lines(&narrow).contains(&"01234567890123456789".into()));
    assert!(lines(&narrow).last().unwrap().contains(" │ "));
    assert!(lines(&narrow).last().unwrap().ends_with('B'));
}

#[test]
fn forms_survive_resize_and_submit_scoped_fields() {
    let (mut session, _) = session(
        "`<8|name`Alice>\n`<?|news|yes|*`Subscribe>\n`[Send`:/page/result.mu`*|action=send]\n",
    );
    let input = session.layout.controls[0].clone();
    session.focus = Some(input.clone());
    assert!(session.edit(&Event::Paste("界".into())));
    session.layout(15);
    assert!(
        matches!(session.form.get(&input), Some(Control::Input {value, ..}) if value == "Alice界")
    );
    assert_eq!(
        session
            .layout
            .rows
            .iter()
            .flatten()
            .filter(|g| g.caret)
            .count(),
        1
    );
    let fields = session.fields(&["*".into(), "action=send".into()]);
    assert_eq!(fields.get("field_name").unwrap(), "Alice界");
    assert_eq!(fields.get("field_news").unwrap(), "yes");
    assert_eq!(fields.get("var_action").unwrap(), "send");
    assert!(!session.fields(&["news".into()]).contains_key("field_name"));
}

#[test]
fn navigation_cancels_old_requests_and_ignores_late_replies() {
    let (mut session, mut commands) = session("Initial");
    session.navigate(URL, false, BTreeMap::new(), true);
    let UiCommand::BrowserFetch(first) = commands.try_recv().unwrap() else {
        panic!()
    };
    session.navigate(":/page/next.mu", false, BTreeMap::new(), true);
    assert!(*first.cancel.borrow());
    first
        .response
        .send(Reply {
            generation: first.generation,
            fragment: None,
            result: Ok(page("Stale")),
        })
        .unwrap();
    session.poll();
    assert!(session.loading);
    let UiCommand::BrowserFetch(second) = commands.try_recv().unwrap() else {
        panic!()
    };
    assert!(second.url.ends_with(":/page/next.mu"));
    let mut next = page("Latest");
    next.url = second.url.clone();
    second
        .response
        .send(Reply {
            generation: second.generation,
            fragment: None,
            result: Ok(next),
        })
        .unwrap();
    session.poll();
    session.layout(60);
    assert_eq!(lines(&session.layout), ["Latest"]);
    assert!(!session.loading);
    session.go_history(false);
    let UiCommand::BrowserFetch(back) = commands.try_recv().unwrap() else {
        panic!()
    };
    assert_eq!(back.url, URL);
    drop(session);
    assert!(*back.cancel.borrow());
}

#[test]
fn partials_fetch_once_or_refresh_explicitly_without_duplicate_requests() {
    let (mut session, mut commands) = session("`{:/page/status.mu`0`pid=status}\n");
    session.partials(None);
    let UiCommand::BrowserFetch(request) = commands.try_recv().unwrap() else {
        panic!()
    };
    assert_eq!(request.fields.get("var_pid").unwrap(), "status");
    session.partials(None);
    assert!(commands.try_recv().is_err());
    request
        .response
        .send(Reply {
            generation: request.generation,
            fragment: request.fragment,
            result: Ok(page("Live status")),
        })
        .unwrap();
    session.poll();
    session.layout(60);
    assert_eq!(lines(&session.layout), ["Live status"]);
    session.partials(None);
    assert!(commands.try_recv().is_err());
    session.partials(Some("status"));
    assert!(matches!(
        commands.try_recv(),
        Ok(UiCommand::BrowserFetch(_))
    ));
    session.stop();
    session.partials(Some("status"));
    assert!(commands.try_recv().is_err());
}

#[test]
fn headless_browser_renders_black_page_and_keyboard_links() {
    let (backend, screen) = tv::HeadlessBackend::new(100, 30);
    let shared = Rc::new(RefCell::new(UiState::default()));
    let (_sender, updates) = update_channel();
    let (sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
    let mut app = TuiApp::new(Box::new(backend), shared.clone(), updates);
    let browser = window(
        Rect::new(0, 0, 100, 28),
        shared,
        URL.split(':').next().unwrap(),
        sender,
    );
    let session = browser.session.clone();
    app.program.desktop_insert(Box::new(browser));
    let UiCommand::BrowserFetch(request) = commands.try_recv().unwrap() else {
        panic!()
    };
    request
        .response
        .send(Reply {
            generation: request.generation,
            fragment: None,
            result: Ok(page("Browser content\n`[Next page`:/page/next.mu]\n")),
        })
        .unwrap();
    for _ in 0..12 {
        app.program.pump_once();
    }
    assert!(screen.snapshot().contains("Browser content"));
    let status: String = screen
        .buffer()
        .row(29)
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        status.contains("Left Back") && status.contains("Right Forward"),
        "{status}"
    );
    assert!(
        status.contains("Ctrl-R Reload") && status.contains("Esc Stop"),
        "{status}"
    );
    assert!(
        screen
            .buffer()
            .cells()
            .iter()
            .any(|cell| cell.symbol() == "B" && cell.style().bg == Color::Bios(0))
    );
    let initial_columns = session.borrow().columns;
    screen.push_event(Event::Command(Command::ZOOM));
    for _ in 0..4 {
        app.program.pump_once();
    }
    screen.resize(70, 22);
    for _ in 0..4 {
        app.program.pump_once();
    }
    assert!(session.borrow().columns < initial_columns);
    assert!(screen.snapshot().contains("Browser content"));
    screen.push_key(Key::Tab, tv::KeyModifiers::default());
    for _ in 0..4 {
        app.program.pump_once();
    }
    assert!(
        session.borrow().focus.is_some(),
        "Tab must select a page link"
    );
    screen.push_key(Key::Enter, tv::KeyModifiers::default());
    for _ in 0..4 {
        app.program.pump_once();
    }
    assert!(
        matches!(commands.try_recv(), Ok(UiCommand::BrowserFetch(request)) if request.url.ends_with("/page/next.mu"))
    );
    screen.push_key(Key::Left, tv::KeyModifiers::default());
    for _ in 0..4 {
        app.program.pump_once();
    }
    assert!(
        matches!(commands.try_recv(), Ok(UiCommand::BrowserFetch(request)) if request.url == URL)
    );
    screen.push_key(Key::Right, tv::KeyModifiers::default());
    for _ in 0..4 {
        app.program.pump_once();
    }
    assert!(
        matches!(commands.try_recv(), Ok(UiCommand::BrowserFetch(request)) if request.url.ends_with("/page/next.mu"))
    );
    screen.push_key(
        Key::Char('r'),
        tv::KeyModifiers {
            ctrl: true,
            ..Default::default()
        },
    );
    for _ in 0..4 {
        app.program.pump_once();
    }
    let UiCommand::BrowserFetch(reload) = commands.try_recv().unwrap() else {
        panic!()
    };
    assert!(reload.reload);
    screen.push_key(Key::Esc, tv::KeyModifiers::default());
    for _ in 0..4 {
        app.program.pump_once();
    }
    assert!(*reload.cancel.borrow());
    assert!(session.borrow().stopped);
    assert!(
        screen.snapshot().contains("Browser content"),
        "Esc must not close Browser"
    );
    screen.push_key(
        Key::Char('l'),
        tv::KeyModifiers {
            ctrl: true,
            ..Default::default()
        },
    );
    screen.push_key(Key::Left, tv::KeyModifiers::default());
    for _ in 0..4 {
        app.program.pump_once();
    }
    assert!(
        commands.try_recv().is_err(),
        "Left must edit the address, not navigate"
    );
    screen.push_key(Key::F(6), tv::KeyModifiers::default());
    for _ in 0..8 {
        app.program.pump_once();
    }
    let status: String = screen
        .buffer()
        .row(21)
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        !status.contains("Left Back"),
        "Browser shortcuts must disappear in another window: {status}"
    );
    assert_eq!(
        render::base_style(session.borrow().page.as_ref()).bg,
        Color::Bios(0)
    );
}

#[test]
fn nested_partials_resolve_links_against_their_own_node() {
    let (mut session, mut commands) =
        session("`{11111111111111111111111111111111:/page/fragment.mu`0}\n");
    session.partials(None);
    let UiCommand::BrowserFetch(request) = commands.try_recv().unwrap() else {
        panic!()
    };
    let mut fragment = page("`[Other`:/page/other.mu]\n");
    fragment.url = request.url.clone();
    request
        .response
        .send(Reply {
            generation: request.generation,
            fragment: request.fragment,
            result: Ok(fragment),
        })
        .unwrap();
    session.poll();
    session.layout(60);
    assert!(
        matches!(session.form.get(&session.layout.controls[0]), Some(Control::Link {target, ..}) if target == "11111111111111111111111111111111:/page/other.mu")
    );
}
