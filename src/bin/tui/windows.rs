use super::*;
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct WindowRegistry {
    keys: HashSet<String>,
    focus: Option<String>,
}

impl WindowRegistry {
    pub(super) fn focus_rrc(&mut self) -> bool {
        if let Some(key) = self
            .keys
            .iter()
            .find(|key| key.starts_with("rrc:"))
            .cloned()
        {
            self.focus = Some(key);
            true
        } else {
            false
        }
    }
    pub(super) fn focus_existing(&mut self, key: &str) -> bool {
        if self.keys.contains(key) {
            self.focus = Some(key.into());
            true
        } else {
            false
        }
    }
}

pub(super) struct ManagedWindow {
    inner: Box<dyn View>,
    key: String,
    registry: Rc<RefCell<WindowRegistry>>,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
}

impl ManagedWindow {
    pub(super) fn browser_url(&mut self) -> Option<String> {
        self.inner
            .as_any_mut()?
            .downcast_mut::<browser::BrowserWindow>()
            .map(|window| window.current_url())
    }

    pub(super) fn new(
        mut inner: Box<dyn View>,
        key: String,
        registry: Rc<RefCell<WindowRegistry>>,
        commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    ) -> Self {
        registry.borrow_mut().keys.insert(key.clone());
        inner.state_mut().options.tileable = true;
        Self {
            inner,
            key,
            registry,
            commands,
        }
    }
}

impl Drop for ManagedWindow {
    fn drop(&mut self) {
        self.registry.borrow_mut().keys.remove(&self.key);
        if let Some(destination_hash) = self.key.strip_prefix("lxmf:") {
            let _ = self.commands.send(UiCommand::CloseConversation {
                destination_hash: destination_hash.into(),
            });
        }
    }
}

#[delegate(to = inner)]
impl View for ManagedWindow {
    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
    fn handle_event(&mut self, event: &mut Event, context: &mut Context) {
        let mut registry = self.registry.borrow_mut();
        if registry.focus.as_deref() == Some(&self.key) {
            if let Some(id) = self.inner.state().id() {
                context.request_focus(id);
                registry.focus = None;
            }
        }
        drop(registry);
        self.inner.handle_event(event, context);
    }
}
