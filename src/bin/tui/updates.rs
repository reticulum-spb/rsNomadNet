use super::*;
use std::sync::{Arc, Mutex};

// Only the newest render state is retained. Command acknowledgements are
// accumulated until the UI consumes them, even when snapshots are coalesced.
#[derive(Clone)]
pub(super) struct UpdateSender(Arc<Mutex<Option<UiState>>>);
pub(super) struct UpdateReceiver(Arc<Mutex<Option<UiState>>>);

pub(super) fn update_channel() -> (UpdateSender, UpdateReceiver) {
    let slot = Arc::new(Mutex::new(None));
    (UpdateSender(slot.clone()), UpdateReceiver(slot))
}

impl UpdateSender {
    pub(super) fn send(&self, mut update: UiState) -> Result<(), ()> {
        let mut slot = self.0.lock().expect("UI update mutex poisoned");
        if let Some(previous) = slot.take() {
            let mut views = previous.directory_views;
            views.extend(update.directory_views);
            update.directory_views = views;
            let mut results = previous.send_results;
            results.extend(update.send_results);
            update.send_results = results;
        }
        *slot = Some(update);
        Ok(())
    }
}

impl UpdateReceiver {
    pub(super) fn try_recv(&self) -> Result<UiState, ()> {
        self.0
            .lock()
            .expect("UI update mutex poisoned")
            .take()
            .ok_or(())
    }
}
