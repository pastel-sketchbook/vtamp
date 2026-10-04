//! Single, cancellable background analyzer. SQLite stays on the server thread.
use super::*;
use crate::loudness::{Analysis, Fingerprint};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    thread::JoinHandle,
};

type Completed = (PathBuf, Fingerprint, Result<Analysis>);
struct Active {
    path: PathBuf,
    fingerprint: Fingerprint,
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}
pub(super) struct Runtime {
    events: broadcast::Receiver<Event>,
    sender: mpsc::SyncSender<Completed>,
    receiver: mpsc::Receiver<Completed>,
    active: Option<Active>,
    catalog: Vec<PathBuf>,
    candidates: HashMap<PathBuf, Option<Fingerprint>>,
    pending: VecDeque<(PathBuf, Fingerprint)>,
    attempted: HashMap<PathBuf, Fingerprint>,
    failures: HashSet<PathBuf>,
    queue_revision: Option<u64>,
    selection: u64,
    current: Option<String>,
    enabled: Option<bool>,
    dirty: bool,
}
impl Runtime {
    pub fn new(
        store: &Store,
        engine: &mut Engine<Box<dyn PlaybackBackend>>,
        events: &broadcast::Sender<Event>,
    ) -> Result<Self> {
        engine.loudness = store.loudness()?;
        let (sender, receiver) = mpsc::sync_channel(1);
        Ok(Self {
            events: events.subscribe(),
            sender,
            receiver,
            active: None,
            catalog: Self::catalog(store)?,
            candidates: HashMap::new(),
            pending: VecDeque::new(),
            attempted: HashMap::new(),
            failures: HashSet::new(),
            queue_revision: None,
            selection: 0,
            current: None,
            enabled: None,
            dirty: true,
        })
    }
    fn catalog(store: &Store) -> Result<Vec<PathBuf>> {
        Ok(store
            .records()?
            .into_iter()
            .filter_map(|r| r.track.playback.file().map(Path::to_owned))
            .collect())
    }
    pub fn retry(&mut self) {
        self.attempted.clear();
        self.failures.clear();
        self.dirty = true;
    }
    pub fn poll(
        &mut self,
        store: &Store,
        engine: &mut Engine<Box<dyn PlaybackBackend>>,
    ) -> Result<bool> {
        loop {
            match self.events.try_recv() {
                Ok(Event::LibraryChanged) | Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    self.catalog = Self::catalog(store)?;
                    self.dirty = true;
                }
                Ok(_) => (),
                Err(_) => break,
            }
        }
        let mut progressed = false;
        if let Ok((path, fingerprint, result)) = self.receiver.try_recv() {
            progressed = true;
            let active = self
                .active
                .take()
                .expect("analysis completion has a worker");
            let cancelled = active.cancel.load(Ordering::Acquire);
            let _ = active.thread.join();
            if !cancelled {
                self.attempted.insert(path.clone(), fingerprint.clone());
                match result {
                    Ok(analysis)
                        if analysis.matches(&path) && self.candidates.contains_key(&path) =>
                    {
                        if let Some(error) = &analysis.error {
                            tracing::warn!(path = %path.display(), %error, "Loudness analysis failed; playing without correction");
                            self.failures.insert(path.clone());
                        }
                        match store.save_loudness(&path, &analysis) {
                            Ok(()) => {
                                engine.loudness.insert(path, analysis);
                            }
                            Err(error) => {
                                tracing::warn!(path = %path.display(), %error, "Cannot save loudness analysis");
                                self.failures.insert(path);
                            }
                        }
                    }
                    Ok(_) => self.dirty = true, // Removed or changed: refresh before retry.
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "Loudness analysis unavailable");
                        self.dirty |=
                            Fingerprint::read(&path).is_ok_and(|current| current != fingerprint);
                        self.failures.insert(path);
                    }
                }
            } else {
                self.dirty = true;
            }
        }
        let enabled = engine.state.normalization.enabled;
        if self.selection != engine.normalization_selection
            || self.enabled != Some(enabled)
            || self.queue_revision != Some(engine.state.queue_revision)
            || self.current != engine.state.current_id
        {
            self.dirty = true;
        }
        let refreshed = self.dirty || progressed;
        if self.dirty {
            self.refresh(engine);
            self.dirty = false;
        }
        if !enabled {
            if let Some(active) = &self.active {
                active.cancel.store(true, Ordering::Release);
            }
        } else if self.active.is_none()
            && let Some((path, fingerprint)) = self.pending.pop_front()
        {
            let sender = self.sender.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            let worker_path = path.clone();
            let worker_fingerprint = fingerprint.clone();
            let thread = std::thread::Builder::new()
                .name("vtamp-loudness".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        crate::loudness::analyze_file(
                            &worker_path,
                            worker_fingerprint.clone(),
                            &worker_cancel,
                        )
                    }))
                    .unwrap_or_else(|_| {
                        Err(anyhow::anyhow!(
                            "Audio decoder panicked during loudness analysis"
                        ))
                    });
                    // There is only one in-flight result; no server shutdown can block this send.
                    let _ = sender.try_send((worker_path, worker_fingerprint, result));
                });
            let thread = match thread {
                Ok(thread) => thread,
                Err(error) => {
                    self.attempted.insert(path.clone(), fingerprint);
                    self.failures.insert(path);
                    self.dirty = true;
                    return Err(error.into());
                }
            };
            self.active = Some(Active {
                path,
                fingerprint,
                cancel,
                thread,
            });
        }
        if !refreshed {
            return Ok(false);
        }
        let before = engine.state.normalization.clone();
        let status = &mut engine.state.normalization;
        status.ready = 0;
        status.unmeasurable = 0;
        status.failed = 0;
        status.pending = 0;
        for (path, fingerprint) in &self.candidates {
            if let Some(analysis) = engine
                .loudness
                .get(path)
                .filter(|a| Some(&a.fingerprint) == fingerprint.as_ref() && a.error.is_none())
            {
                if analysis.measurement.is_some() {
                    status.ready += 1;
                } else {
                    status.unmeasurable += 1;
                }
            } else if self.failures.contains(path) {
                status.failed += 1;
            } else {
                status.pending += 1;
            }
        }
        Ok(*status != before)
    }
    fn refresh(&mut self, engine: &Engine<Box<dyn PlaybackBackend>>) {
        self.selection = engine.normalization_selection;
        self.queue_revision = Some(engine.state.queue_revision);
        self.current = engine.state.current_id.clone();
        self.enabled = Some(engine.state.normalization.enabled);
        self.pending.clear();
        self.candidates.clear();
        let paths = engine
            .state
            .current()
            .into_iter()
            .chain(engine.state.queue.iter())
            .chain(engine.state.direct.iter().map(Box::as_ref))
            .filter_map(|item| item.track.playback.file().map(Path::to_owned))
            .chain(self.catalog.iter().cloned());
        for path in paths {
            if self.candidates.contains_key(&path) {
                continue;
            }
            let fingerprint = match Fingerprint::read(&path) {
                Ok(fingerprint) => fingerprint,
                Err(_) => {
                    self.failures.insert(path.clone());
                    self.candidates.insert(path, None);
                    continue;
                }
            };
            self.candidates
                .insert(path.clone(), Some(fingerprint.clone()));
            let cached = engine
                .loudness
                .get(&path)
                .is_some_and(|a| a.fingerprint == fingerprint && a.error.is_none());
            if cached {
                self.failures.remove(&path);
                continue;
            }
            if self.attempted.get(&path) == Some(&fingerprint) {
                continue;
            }
            self.failures.remove(&path);
            if let Some(active) = &self.active
                && active.path == path
            {
                if active.fingerprint == fingerprint {
                    continue;
                }
                active.cancel.store(true, Ordering::Release);
            }
            self.pending.push_back((path, fingerprint));
        }
        if let Some(active) = &self.active
            && !self.candidates.contains_key(&active.path)
        {
            active.cancel.store(true, Ordering::Release);
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            active.cancel.store(true, Ordering::Release);
            let _ = active.thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settle(runtime: &mut Runtime, store: &Store, engine: &mut Engine<Box<dyn PlaybackBackend>>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            runtime.poll(store, engine).unwrap();
            if runtime.active.is_none() && runtime.pending.is_empty() {
                return;
            }
            assert!(Instant::now() < deadline, "analysis did not settle");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn automatically_analyzes_existing_catalog_reuses_cache_and_retries_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tone.wav");
        crate::loudness::write_tone(&file, 0.1, 2);
        let file = file.canonicalize().unwrap();
        let mut store = Store::open(&dir.path().join("state.db")).unwrap();
        let scan = library::scan(&[dir.path().to_owned()], &[], &dir.path().join("covers"));
        store.replace_catalog(&scan.records).unwrap();
        let id = scan.records[0].track.id.clone();
        let backend = crate::audio::headless::HeadlessBackend::new(
            Arc::new(crate::cast::Hub::default()),
            96000,
        )
        .unwrap();
        let mut engine = Engine::new(
            State {
                volume: 0,
                ..Default::default()
            },
            Box::new(backend) as Box<dyn PlaybackBackend>,
        );
        let (events, _) = broadcast::channel(64);
        let mut runtime = Runtime::new(&store, &mut engine, &events).unwrap();
        settle(&mut runtime, &store, &mut engine);
        assert_eq!(engine.state.normalization.ready, 1);
        assert_eq!(store.records().unwrap()[0].track.id, id);
        let before = store.loudness().unwrap()[&file].fingerprint.clone();
        drop(runtime);
        let mut runtime = Runtime::new(&store, &mut engine, &events).unwrap();
        runtime.poll(&store, &mut engine).unwrap();
        assert!(
            runtime.active.is_none(),
            "cached files must not be decoded again"
        );
        crate::loudness::write_tone(&file, 0.5, 3);
        events.send(Event::LibraryChanged).unwrap();
        settle(&mut runtime, &store, &mut engine);
        assert_ne!(store.loudness().unwrap()[&file].fingerprint, before);
        assert_eq!(engine.state.normalization.ready, 1);
        std::fs::write(&file, b"damaged file").unwrap();
        events.send(Event::LibraryChanged).unwrap();
        settle(&mut runtime, &store, &mut engine);
        assert_eq!(engine.state.normalization.failed, 1);
        assert_eq!(engine.state.normalization.ready, 0);
        events.send(Event::LibraryChanged).unwrap();
        runtime.poll(&store, &mut engine).unwrap();
        assert!(
            runtime.active.is_none(),
            "failure must not become a busy retry loop"
        );
        runtime.retry();
        runtime.poll(&store, &mut engine).unwrap();
        assert!(runtime.active.is_some(), "explicit rescan retries failures");
        settle(&mut runtime, &store, &mut engine);
        assert_eq!(engine.state.normalization.failed, 1);
        assert!(engine.state.last_error.is_none());
    }
    #[test]
    fn disabling_cancels_analysis_and_keeps_it_pending_until_reenabled() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tone.wav");
        crate::loudness::write_tone(&file, 0.1, 10);
        let mut store = Store::open(&dir.path().join("state.db")).unwrap();
        let scan = library::scan(&[dir.path().to_owned()], &[], &dir.path().join("covers"));
        store.replace_catalog(&scan.records).unwrap();
        let backend = crate::audio::headless::HeadlessBackend::new(
            Arc::new(crate::cast::Hub::default()),
            96000,
        )
        .unwrap();
        let mut engine = Engine::new(
            State {
                volume: 0,
                ..Default::default()
            },
            Box::new(backend) as Box<dyn PlaybackBackend>,
        );
        let (events, _) = broadcast::channel(64);
        let mut runtime = Runtime::new(&store, &mut engine, &events).unwrap();
        runtime.poll(&store, &mut engine).unwrap();
        runtime
            .active
            .as_ref()
            .unwrap()
            .cancel
            .store(true, Ordering::Release);
        engine.state.normalization.enabled = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while runtime.active.is_some() {
            runtime.poll(&store, &mut engine).unwrap();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(store.loudness().unwrap().is_empty());
        assert_eq!(engine.state.normalization.pending, 1);
        engine.state.normalization.enabled = true;
        settle(&mut runtime, &store, &mut engine);
        assert_eq!(engine.state.normalization.ready, 1);
    }
}
