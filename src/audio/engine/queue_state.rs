use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::commands::{QueueItem, StreamQueueItem};
use super::metadata::{TrackCover, TrackTags};

/// What the worker should load next. Both file-backed and already-open stream
/// tracks can auto-advance through their respective queues.
pub(super) enum PendingStart {
    File { item: QueueItem, epoch: u64 },
    Stream { item: StreamQueueItem, epoch: u64 },
}

impl PendingStart {
    pub(super) fn epoch(&self) -> u64 {
        match self {
            Self::File { epoch, .. } | Self::Stream { epoch, .. } => *epoch,
        }
    }
}

pub(super) struct WorkerQueues {
    file_queue: VecDeque<QueueItem>,
    stream_queue: VecDeque<StreamQueueItem>,
    stream_queue_len: Arc<AtomicUsize>,
    stream_auto_advance_pending: Arc<AtomicBool>,
}

impl WorkerQueues {
    pub(super) fn new(
        stream_queue_len: Arc<AtomicUsize>,
        stream_auto_advance_pending: Arc<AtomicBool>,
    ) -> Self {
        Self {
            file_queue: VecDeque::new(),
            stream_queue: VecDeque::new(),
            stream_queue_len,
            stream_auto_advance_pending,
        }
    }

    pub(super) fn replace_for_file_start(
        &mut self,
        item: QueueItem,
        new_queue: Vec<QueueItem>,
        epoch: u64,
    ) -> PendingStart {
        self.file_queue = new_queue.into();
        self.clear_stream_queue();
        self.clear_stream_auto_advance_pending();
        PendingStart::File { item, epoch }
    }

    pub(super) fn replace_for_stream_start(
        &mut self,
        item: StreamQueueItem,
        new_queue: Vec<StreamQueueItem>,
        epoch: u64,
    ) -> PendingStart {
        self.file_queue.clear();
        self.stream_queue = new_queue.into();
        self.publish_stream_queue_len();
        self.clear_stream_auto_advance_pending();
        PendingStart::Stream { item, epoch }
    }

    pub(super) fn replace_file_queue(&mut self, new_queue: Vec<QueueItem>) {
        self.file_queue = new_queue.into();
        self.clear_stream_queue();
        self.clear_stream_auto_advance_pending();
    }

    pub(super) fn replace_stream_queue(&mut self, new_queue: Vec<StreamQueueItem>) {
        self.file_queue.clear();
        self.stream_queue = new_queue.into();
        self.publish_stream_queue_len();
    }

    pub(super) fn pop_next_start(&mut self, epoch: u64) -> Option<PendingStart> {
        if let Some(item) = self.stream_queue.pop_front() {
            self.publish_stream_queue_len();
            self.stream_auto_advance_pending
                .store(true, Ordering::Relaxed);
            Some(PendingStart::Stream { item, epoch })
        } else if let Some(item) = self.file_queue.pop_front() {
            self.clear_stream_auto_advance_pending();
            Some(PendingStart::File { item, epoch })
        } else {
            self.clear_stream_auto_advance_pending();
            None
        }
    }

    pub(super) fn repeat_file_start(
        &mut self,
        file_path: String,
        fallback_cover: Option<TrackCover>,
        fallback_tags: Option<TrackTags>,
        epoch: u64,
    ) -> PendingStart {
        self.clear_stream_auto_advance_pending();
        PendingStart::File {
            item: QueueItem {
                file_path,
                fallback_cover,
                fallback_tags,
            },
            epoch,
        }
    }

    pub(super) fn eof_next_start(
        &mut self,
        can_advance: bool,
        repeat_one: bool,
        current_file_path: Option<String>,
        fallback_cover: Option<TrackCover>,
        fallback_tags: Option<TrackTags>,
        epoch: u64,
    ) -> Option<PendingStart> {
        if can_advance && repeat_one {
            if let Some(path) = current_file_path {
                Some(self.repeat_file_start(path, fallback_cover, fallback_tags, epoch))
            } else {
                self.pop_next_start(epoch)
            }
        } else if can_advance {
            self.pop_next_start(epoch)
        } else {
            self.clear_all();
            None
        }
    }

    pub(super) fn clear_all(&mut self) {
        self.file_queue.clear();
        self.clear_stream_queue();
        self.clear_stream_auto_advance_pending();
    }

    pub(super) fn clear_stream_auto_advance_pending(&self) {
        self.stream_auto_advance_pending
            .store(false, Ordering::Relaxed);
    }

    fn clear_stream_queue(&mut self) {
        self.stream_queue.clear();
        self.publish_stream_queue_len();
    }

    fn publish_stream_queue_len(&self) {
        self.stream_queue_len
            .store(self.stream_queue.len(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn queue_item(file_path: &str) -> QueueItem {
        QueueItem {
            file_path: file_path.to_string(),
            fallback_cover: None,
            fallback_tags: None,
        }
    }

    fn stream_item(display_name: &str) -> StreamQueueItem {
        StreamQueueItem {
            source: Box::new(Cursor::new(Vec::<u8>::new())),
            ext_hint: None,
            display_name: display_name.to_string(),
            fallback_cover: None,
            fallback_tags: None,
        }
    }

    fn queues() -> (WorkerQueues, Arc<AtomicUsize>, Arc<AtomicBool>) {
        let stream_queue_len = Arc::new(AtomicUsize::new(0));
        let stream_auto_advance_pending = Arc::new(AtomicBool::new(false));
        (
            WorkerQueues::new(
                Arc::clone(&stream_queue_len),
                Arc::clone(&stream_auto_advance_pending),
            ),
            stream_queue_len,
            stream_auto_advance_pending,
        )
    }

    #[test]
    fn file_start_replaces_stream_queue_and_resets_stream_flags() {
        let (mut queues, stream_queue_len, stream_auto_advance_pending) = queues();

        queues.replace_for_stream_start(stream_item("current"), vec![stream_item("next")], 1);
        assert_eq!(stream_queue_len.load(Ordering::Relaxed), 1);

        queues.replace_for_file_start(queue_item("current.flac"), vec![queue_item("next.flac")], 2);

        assert_eq!(stream_queue_len.load(Ordering::Relaxed), 0);
        assert!(!stream_auto_advance_pending.load(Ordering::Relaxed));
        match queues.pop_next_start(2) {
            Some(PendingStart::File { item, epoch }) => {
                assert_eq!(item.file_path, "next.flac");
                assert_eq!(epoch, 2);
            }
            _ => panic!("expected next file item"),
        }
    }

    #[test]
    fn popping_stream_start_publishes_len_and_pending_flag() {
        let (mut queues, stream_queue_len, stream_auto_advance_pending) = queues();

        queues.replace_for_stream_start(
            stream_item("current"),
            vec![stream_item("next-1"), stream_item("next-2")],
            7,
        );

        match queues.pop_next_start(8) {
            Some(PendingStart::Stream { item, epoch }) => {
                assert_eq!(item.display_name, "next-1");
                assert_eq!(epoch, 8);
            }
            _ => panic!("expected next stream item"),
        }
        assert_eq!(stream_queue_len.load(Ordering::Relaxed), 1);
        assert!(stream_auto_advance_pending.load(Ordering::Relaxed));

        queues.clear_all();
        assert_eq!(stream_queue_len.load(Ordering::Relaxed), 0);
        assert!(!stream_auto_advance_pending.load(Ordering::Relaxed));
    }

    #[test]
    fn queued_stream_prefetch_is_not_pending_until_popped() {
        let (mut queues, stream_queue_len, stream_auto_advance_pending) = queues();

        queues.replace_stream_queue(vec![stream_item("next-stream")]);

        assert_eq!(stream_queue_len.load(Ordering::Relaxed), 1);
        assert!(!stream_auto_advance_pending.load(Ordering::Relaxed));
    }

    #[test]
    fn eof_repeat_one_restarts_current_file_without_consuming_queue() {
        let (mut queues, _stream_queue_len, _stream_auto_advance_pending) = queues();
        queues.replace_file_queue(vec![queue_item("next.flac")]);

        match queues.eof_next_start(true, true, Some("current.flac".to_string()), None, None, 11) {
            Some(PendingStart::File { item, epoch }) => {
                assert_eq!(item.file_path, "current.flac");
                assert_eq!(epoch, 11);
            }
            _ => panic!("expected repeat file start"),
        }

        match queues.pop_next_start(12) {
            Some(PendingStart::File { item, epoch }) => {
                assert_eq!(item.file_path, "next.flac");
                assert_eq!(epoch, 12);
            }
            _ => panic!("expected queued next file to remain"),
        }
    }

    #[test]
    fn eof_repeat_one_stream_falls_back_to_queued_stream() {
        let (mut queues, stream_queue_len, stream_auto_advance_pending) = queues();
        queues.replace_stream_queue(vec![stream_item("next-stream")]);

        match queues.eof_next_start(true, true, None, None, None, 13) {
            Some(PendingStart::Stream { item, epoch }) => {
                assert_eq!(item.display_name, "next-stream");
                assert_eq!(epoch, 13);
            }
            _ => panic!("expected queued stream item"),
        }

        assert_eq!(stream_queue_len.load(Ordering::Relaxed), 0);
        assert!(stream_auto_advance_pending.load(Ordering::Relaxed));
    }

    #[test]
    fn stale_eof_clears_queues_without_advancing() {
        let (mut queues, stream_queue_len, stream_auto_advance_pending) = queues();
        queues.replace_for_stream_start(
            stream_item("current"),
            vec![stream_item("next-stream")],
            3,
        );
        assert_eq!(stream_queue_len.load(Ordering::Relaxed), 1);

        assert!(
            queues
                .eof_next_start(false, false, Some("old.flac".to_string()), None, None, 3)
                .is_none()
        );
        assert_eq!(stream_queue_len.load(Ordering::Relaxed), 0);
        assert!(!stream_auto_advance_pending.load(Ordering::Relaxed));
        assert!(queues.pop_next_start(4).is_none());
    }
}
