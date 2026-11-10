//! The main-thread inbox for native document requests. Paths received before the app exists
//! wait for its first input frame; later requests wake it. No project is opened in a callback.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// Bound both one native event and the inbox while the user is busy in a modal dialog.
pub const MAX_FILES: usize = 1024;

/// Decoded native input, including requests which could not be delivered.
#[derive(Default)]
pub struct Batch {
    pub paths: Vec<PathBuf>,
    pub unreadable: usize,
    pub excess: usize,
}

#[derive(Default)]
struct Pending {
    batch: Batch,
    context: Option<egui::Context>,
}

/// Clones share one main-thread queue; the native handler and the app each keep one.
#[derive(Clone, Default)]
pub struct Inbox(Rc<RefCell<Pending>>);

impl Inbox {
    /// Attach the app's wakeup once eframe has created it. Earlier input stays queued.
    pub fn set_context(&self, context: &egui::Context) {
        self.0.borrow_mut().context = Some(context.clone());
        context.request_repaint();
    }

    /// Retain bounded input and report every omitted item, then wake outside the queue borrow.
    pub fn receive(&self, batch: Batch) {
        let context = {
            let mut pending = self.0.borrow_mut();
            let room = MAX_FILES.saturating_sub(pending.batch.paths.len());
            let excess = batch.paths.len().saturating_sub(room).saturating_add(batch.excess);
            pending.batch.paths.extend(batch.paths.into_iter().take(room));
            pending.batch.unreadable = pending.batch.unreadable.saturating_add(batch.unreadable);
            pending.batch.excess = pending.batch.excess.saturating_add(excess);
            pending.context.clone()
        };
        if let Some(context) = context {
            context.request_repaint();
        }
    }

    /// Deliver once through the existing desktop-drop path, returning counts for UI notices.
    pub fn append(&self, input: &mut egui::RawInput) -> (usize, usize) {
        let batch = std::mem::take(&mut self.0.borrow_mut().batch);
        input.dropped_files.extend(batch.paths.into_iter().map(|path| Arc::new(File(path)) as Arc<dyn egui::DroppedFile + Send + Sync>));
        (batch.unreadable, batch.excess)
    }
}

/// A native path, read only if the existing drop handler needs its bytes (such as a plugin).
#[derive(Debug)]
struct File(PathBuf);

impl egui::DroppedFile for File {
    /// Preserve the native path until the normal interface dispatcher reads it.
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    /// Match a desktop window drop: native receipt itself never loads the file.
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_paths_survive_until_the_app_and_are_delivered_once() {
        let inbox = Inbox::default();
        let receiver = inbox.clone();
        let paths = vec![PathBuf::from("/tmp/Área fiscal/My project.rgis"), PathBuf::from("/tmp/sites.csv")];
        inbox.receive(Batch { paths: paths.clone(), ..Default::default() });
        receiver.set_context(&egui::Context::default());
        let mut input = egui::RawInput::default();
        assert_eq!(receiver.append(&mut input), (0, 0));
        assert_eq!(input.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>(), paths);
        let mut next = egui::RawInput::default();
        assert_eq!(receiver.append(&mut next), (0, 0));
        assert!(next.dropped_files.is_empty());
    }

    #[test]
    fn warm_batches_keep_order_and_existing_window_drops() {
        let inbox = Inbox::default();
        inbox.set_context(&egui::Context::default());
        let mut input = egui::RawInput::default();
        input.dropped_files.push(Arc::new(File("window.geojson".into())));
        for path in ["first.rgisx", "second.csv"] {
            inbox.receive(Batch { paths: vec![path.into()], ..Default::default() });
        }
        assert_eq!(inbox.append(&mut input), (0, 0));
        assert_eq!(
            input.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>(),
            ["window.geojson", "first.rgisx", "second.csv"].map(PathBuf::from)
        );
    }

    #[test]
    fn full_inbox_counts_every_omitted_path_and_decode_failure() {
        let inbox = Inbox::default();
        inbox.receive(Batch { paths: vec![PathBuf::from("a.csv"); MAX_FILES], ..Default::default() });
        inbox.receive(Batch { paths: vec![PathBuf::from("b.csv"); 3], unreadable: 2, excess: 4 });
        let mut input = egui::RawInput::default();
        assert_eq!(inbox.append(&mut input), (2, 7));
        assert_eq!(input.dropped_files.len(), MAX_FILES);
        assert_eq!(inbox.append(&mut egui::RawInput::default()), (0, 0));
    }
}
