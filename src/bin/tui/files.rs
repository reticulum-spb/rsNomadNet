use super::*;
use rsnomadnet_core::attachments::FileOffer;

pub(super) const SEND: Command = Command::custom("nomadnet.send_file");
pub(super) const REVIEW: Command = Command::custom("nomadnet.review_files");
const ACCEPT: Command = Command::custom("nomadnet.accept_file");
const DECLINE: Command = Command::custom("nomadnet.decline_file");

type Reply = tokio::sync::oneshot::Receiver<Result<String, String>>;

struct FileText {
    text: StaticText,
}

#[delegate(to = text)]
impl View for FileText {
    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
}

/// Non-modal confirmation: declining or closing drops the in-memory offer.
pub(super) struct FileWindow {
    window: Dialog,
    text: ViewId,
    description: String,
    offer: Option<String>,
    response: Option<Reply>,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
}

impl FileWindow {
    fn new(
        bounds: Rect,
        description: String,
        offer: Option<String>,
        response: Option<Reply>,
        commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    ) -> Self {
        let width = 64.min(bounds.b.x - bounds.a.x).max(48);
        let x = bounds.a.x + ((bounds.b.x - bounds.a.x - width) / 2).max(0);
        let y = bounds.a.y + 2;
        let mut window = Dialog::new(Rect::new(x, y, x + width, y + 14), Some("LXMF file".into()));
        let text = window.insert_child(Box::new(FileText {
            text: StaticText::new(Rect::new(2, 1, width - 2, 10), &description),
        }));
        if offer.is_some() {
            window.insert_child(Box::new(Button::new(
                Rect::new(3, 11, 18, 13),
                "~A~ccept",
                ACCEPT,
                ButtonFlags::default(),
            )));
        }
        // Refusal is the initially focused action, never acceptance by accident.
        window.insert_child(Box::new(Button::new(
            Rect::new(width - 22, 11, width - 3, 13),
            if offer.is_some() {
                "~D~ecline / Close"
            } else {
                "~C~lose"
            },
            DECLINE,
            ButtonFlags::default(),
        )));
        Self {
            window,
            text,
            description,
            offer,
            response,
            commands,
        }
    }

    pub(super) fn offer(
        bounds: Rect,
        offer: FileOffer,
        commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    ) -> Self {
        let description = format!(
            "Incoming file: {}\nSize: {} bytes\nFrom: {}\n{}\nAccept to save in the files directory.\nOffers expire after 10 minutes.",
            offer.name,
            offer.size,
            offer.sender,
            if offer.verified {
                "Signature verified"
            } else {
                "WARNING: sender signature could not be verified"
            }
        );
        Self::new(bounds, description, Some(offer.id), None, commands)
    }

    pub(super) fn sending(
        bounds: Rect,
        name: String,
        response: Reply,
        commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    ) -> Self {
        Self::new(
            bounds,
            format!(
                "Sending file: {name}\nDirect LXMF delivery.\nClosing this window does not cancel the transfer."
            ),
            None,
            Some(response),
            commands,
        )
    }
}

impl Drop for FileWindow {
    fn drop(&mut self) {
        if let Some(id) = self.offer.take() {
            let (reply, _) = tokio::sync::oneshot::channel();
            let _ = self.commands.send(UiCommand::DecideFile {
                id,
                accept: false,
                reply,
            });
        }
    }
}

#[delegate(to = window)]
impl View for FileWindow {
    fn handle_event(&mut self, event: &mut Event, ctx: &mut Context) {
        if let Some(result) = self.response.as_mut().and_then(|r| match r.try_recv() {
            Ok(result) => Some(result),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
            Err(_) => Some(Err("Transfer worker stopped".into())),
        }) {
            self.response = None;
            let status = match result {
                Ok(status) => {
                    // Only incoming offers close automatically. Outgoing transfers
                    // keep their delivery status visible until explicitly closed.
                    if self.offer.take().is_some() {
                        if let Some(id) = self.state().id() {
                            ctx.request_close(id);
                        }
                    }
                    status
                }
                Err(error) => format!("Error: {error}"),
            };
            if let Some(text) = self
                .window
                .find_mut(self.text)
                .and_then(|v| v.as_any_mut())
                .and_then(|v| v.downcast_mut::<FileText>())
            {
                text.text
                    .set_text(format!("{status}\n\n{}", self.description));
            }
        }
        if matches!(event, Event::Command(command) if *command == ACCEPT) {
            if self.response.is_none() {
                if let Some(id) = self.offer.clone() {
                    let (reply, response) = tokio::sync::oneshot::channel();
                    let _ = self.commands.send(UiCommand::DecideFile {
                        id,
                        accept: true,
                        reply,
                    });
                    self.response = Some(response);
                }
            }
            event.clear();
        } else if matches!(event, Event::Command(command) if *command == DECLINE) {
            if let Some(id) = self.state().id() {
                ctx.request_close(id);
            }
            event.clear();
        }
        self.window.handle_event(event, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> FileOffer {
        FileOffer {
            id: "message:0".into(),
            sender: "aa".repeat(16),
            name: "report.txt".into(),
            size: 12345,
            verified: true,
        }
    }

    #[test]
    fn accepted_offer_closes_only_after_successful_save() {
        for result in [Err("disk full".into()), Ok("Saved".into())] {
            let (backend, screen) = tv::HeadlessBackend::new(100, 30);
            let (_sender, updates) = update_channel();
            let mut app = TuiApp::new(
                Box::new(backend),
                Rc::new(RefCell::new(UiState::default())),
                updates,
            );
            let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let bounds = app.program.desktop_rect();
            let mut view = FileWindow::offer(bounds, offer(), commands);
            crate::tests::with_context(|ctx| {
                view.handle_event(&mut Event::Command(ACCEPT), ctx);
            });
            app.program.desktop_insert(Box::new(view));
            for _ in 0..30 {
                app.program.pump_once();
            }
            let UiCommand::DecideFile { accept, reply, .. } = receiver.try_recv().unwrap() else {
                panic!("wrong command")
            };
            assert!(accept);
            assert!(screen.snapshot().contains("report.txt"));
            let succeeded = result.is_ok();
            reply.send(result).unwrap();
            screen.push_key(Key::Tab, KeyModifiers::default());
            for _ in 0..30 {
                app.program.pump_once();
            }
            if succeeded {
                assert!(!screen.snapshot().contains("report.txt"));
            } else {
                assert!(screen.snapshot().contains("Error: disk full"));
            }
            // Closing a successfully accepted offer must not send a refusal.
            assert!(receiver.try_recv().is_err());
        }
    }

    #[test]
    fn accepting_is_explicit_and_status_is_updated_after_save() {
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut view = FileWindow::offer(Rect::new(0, 0, 100, 30), offer(), commands);
        assert!(receiver.try_recv().is_err());
        crate::tests::with_context(|ctx| {
            view.handle_event(&mut Event::Command(ACCEPT), ctx);
            let UiCommand::DecideFile { id, accept, reply } = receiver.try_recv().unwrap() else {
                panic!("wrong command")
            };
            assert_eq!(id, "message:0");
            assert!(accept);
            view.handle_event(&mut Event::Command(ACCEPT), ctx);
            assert!(receiver.try_recv().is_err());
            reply
                .send(Ok("Saved to /tmp/files/report.txt".into()))
                .unwrap();
            view.handle_event(&mut Event::Nothing, ctx);
            assert!(view.offer.is_none());
            let text = view
                .window
                .find_mut(view.text)
                .unwrap()
                .as_any_mut()
                .unwrap()
                .downcast_mut::<FileText>()
                .unwrap();
            assert!(text.text.text().contains("Saved to /tmp/files/report.txt"));
        });
        drop(view);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn closing_an_offer_rejects_it_and_failed_save_can_be_retried() {
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut view = FileWindow::offer(Rect::new(0, 0, 100, 30), offer(), commands);
        crate::tests::with_context(|ctx| {
            view.handle_event(&mut Event::Command(ACCEPT), ctx);
            let UiCommand::DecideFile { reply, .. } = receiver.try_recv().unwrap() else {
                panic!("wrong command")
            };
            reply.send(Err("disk full".into())).unwrap();
            view.handle_event(&mut Event::Nothing, ctx);
            assert!(view.offer.is_some());
            assert!(view.response.is_none());
        });
        drop(view);
        assert!(matches!(
            receiver.try_recv(),
            Ok(UiCommand::DecideFile { accept: false, .. })
        ));
    }

    #[test]
    fn prompt_shows_name_size_and_does_not_block_other_windows() {
        let (backend, screen) = tv::HeadlessBackend::new(100, 30);
        let (_sender, updates) = update_channel();
        let state = Rc::new(RefCell::new(UiState::default()));
        let mut app = TuiApp::new(Box::new(backend), state, updates);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let bounds = app.program.desktop_rect();
        app.program
            .desktop_insert(Box::new(FileWindow::offer(bounds, offer(), commands)));
        for _ in 0..30 {
            app.program.pump_once();
        }
        assert!(screen.snapshot().contains("report.txt"));
        assert!(screen.snapshot().contains("12345 bytes"));
        assert!(receiver.try_recv().is_err());
        screen.push_key(
            Key::Char('3'),
            KeyModifiers {
                alt: true,
                ..Default::default()
            },
        );
        for _ in 0..30 {
            app.program.pump_once();
        }
        assert_eq!(app.layout.borrow().active.as_deref(), Some("directory"));
    }

    #[test]
    fn enter_does_not_accept_an_offer_by_default() {
        let (backend, screen) = tv::HeadlessBackend::new(100, 30);
        let (_sender, updates) = update_channel();
        let mut app = TuiApp::new(
            Box::new(backend),
            Rc::new(RefCell::new(UiState::default())),
            updates,
        );
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let bounds = app.program.desktop_rect();
        app.program
            .desktop_insert(Box::new(FileWindow::offer(bounds, offer(), commands)));
        screen.push_key(Key::Enter, KeyModifiers::default());
        for _ in 0..40 {
            app.program.pump_once();
        }
        if let Ok(command) = receiver.try_recv() {
            assert!(matches!(
                command,
                UiCommand::DecideFile { accept: false, .. }
            ));
        }
    }
}
