use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Counters apply to the current stage; byte totals are unknown while hashing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub stage: String,
    pub items_done: usize,
    pub items_total: usize,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub current: Option<String>,
}

pub(crate) struct Tracker<'a> {
    pub value: Progress,
    sink: Option<&'a mut dyn FnMut(&Progress)>,
    last: Instant,
}

impl<'a> Tracker<'a> {
    pub fn new(sink: &'a mut dyn FnMut(&Progress)) -> Self {
        Self {
            value: Progress::default(),
            sink: Some(sink),
            last: Instant::now(),
        }
    }

    pub fn silent() -> Self {
        Self {
            value: Progress::default(),
            sink: None,
            last: Instant::now(),
        }
    }

    pub fn begin(&mut self, stage: &str, items_total: usize, bytes_total: Option<u64>) {
        self.value = Progress {
            stage: stage.into(),
            items_total,
            bytes_total,
            ..Default::default()
        };
        self.emit(true);
    }

    pub fn item(&mut self, index: usize, name: &str) {
        self.value.items_done = index;
        self.value.current = Some(name.chars().filter(|c| !c.is_control()).take(160).collect());
        self.emit(false);
    }

    pub fn advance(&mut self, bytes: usize) {
        self.value.bytes_done = self.value.bytes_done.saturating_add(bytes as u64);
        self.emit(false);
    }

    pub fn end(&mut self) {
        self.value.items_done = self.value.items_total;
        self.value.current = None;
        self.emit(true);
    }

    pub fn emit(&mut self, force: bool) {
        self.emit_at(force, Instant::now());
    }

    fn emit_at(&mut self, force: bool, now: Instant) {
        if force || now.duration_since(self.last) >= Duration::from_millis(100) {
            if let Some(sink) = &mut self.sink {
                sink(&self.value);
            }
            self.last = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_throttles_bytes_but_immediately_publishes_stage_boundaries() {
        let mut events = vec![];
        {
            let mut sink = |p: &Progress| events.push(p.clone());
            let mut tracker = Tracker::new(&mut sink);
            tracker.begin("compressing", 2, Some(1000));
            let start = tracker.last;
            tracker.value.bytes_done = 200;
            tracker.emit_at(false, start + Duration::from_millis(99));
            tracker.value.bytes_done = 400;
            tracker.emit_at(false, start + Duration::from_millis(100));
            tracker.value.bytes_done = 1000;
            // Return the synthetic clock before calling methods using the real clock.
            tracker.last = start;
            tracker.end();
            tracker.begin("finalizing", 0, None);
        }
        assert_eq!(events.len(), 4);
        assert_eq!(events[1].bytes_done, 400);
        assert_eq!(events[1].items_done, 0);
        assert_eq!(events[2].items_done, 2);
        assert_eq!(events[3].stage, "finalizing");
    }
}
