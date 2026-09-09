// Shared identity-preserving list behavior for Directory and Conversations.
use super::*;

pub(super) struct StateList {
    pub(super) list: ListBox,
    state: Shared,
    pane: Pane,
    seeded: bool,
    row_ids: Vec<String>,
    pub(super) filters: Option<directory::Filters>,
}

impl StateList {
    pub(super) fn new(bounds: Rect, state: Shared, pane: Pane) -> Self {
        Self::with_scrollbar(bounds, state, pane, None)
    }

    pub(super) fn with_scrollbar(
        bounds: Rect,
        state: Shared,
        pane: Pane,
        scrollbar: Option<ViewId>,
    ) -> Self {
        let mut view = Self {
            list: ListBox::new(bounds, 1, None, scrollbar),
            state,
            pane,
            seeded: false,
            row_ids: Vec::new(),
            filters: None,
        };
        view.state_mut().grow_mode = GrowMode {
            hi_x: true,
            hi_y: true,
            ..Default::default()
        };
        view
    }

    pub(super) fn lines(&self) -> Vec<String> {
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
                .filter(|entry| self.filters.as_ref().is_none_or(|f| f.includes(entry.kind)))
                .map(|entry| entry.label.clone())
                .collect(),
        }
    }

    pub(super) fn focused_destination(&self) -> Option<String> {
        let FieldValue::Int(index) = self.list.value()? else {
            return None;
        };
        self.row_ids.get(index as usize).cloned()
    }

    pub(super) fn ids(&self) -> Vec<String> {
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
                .filter(|entry| self.filters.as_ref().is_none_or(|f| f.includes(entry.kind)))
                .map(|row| row.destination_hash.clone())
                .collect(),
        }
    }

    pub(super) fn focused_directory_kind(&self) -> Option<DirectoryKind> {
        let destination = self.focused_destination()?;
        self.state
            .borrow()
            .directory
            .iter()
            .find(|entry| entry.destination_hash == destination)
            .map(|entry| entry.kind)
    }
}

#[delegate(to = list)]
impl View for StateList {
    fn apply_scroll_sync(&mut self, h: Option<i32>, v: Option<i32>, ctx: &mut Context) {
        self.list.apply_scroll_sync(h, v, ctx);
        // TVision defers ListBox's write-back to the next pump event. Flush it
        // before another mouse event can change the scrollbar's value again.
        ctx.put_event(Event::Nothing);
        if self.list.state().state.focused {
            if let Some(destination) = self.focused_destination() {
                let mut state = self.state.borrow_mut();
                state.selected_destination_hash = Some(destination.clone());
                if matches!(self.pane, Pane::Directory) {
                    state.selected_directory_hash = Some(destination);
                }
            }
        }
    }
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
                tv::widgets::list_viewer::update_steps(&self.list, context);
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
        if open && self.focused_destination().is_some() {
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
