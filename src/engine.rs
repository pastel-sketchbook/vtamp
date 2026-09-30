use crate::{
    audio::{OutputUnavailable, PlaybackBackend},
    model::*,
};
use anyhow::{Result, bail};
use rand::seq::SliceRandom;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const OUTPUT_RETRY_INTERVAL: Duration = Duration::from_secs(1);

pub struct Engine<B: PlaybackBackend> {
    pub state: State,
    backend: B,
    loaded: bool,
    upcoming: VecDeque<String>,
    history: Vec<String>,
    output_retry: Option<Instant>,
}

impl<B: PlaybackBackend> Engine<B> {
    pub fn new(state: State, backend: B) -> Self {
        let mut engine = Self {
            state,
            backend,
            loaded: false,
            upcoming: VecDeque::new(),
            history: vec![],
            output_retry: None,
        };
        if engine.state.shuffle {
            engine.refill_shuffle();
        }
        engine
    }
    pub fn add(&mut self, tracks: Vec<Track>) -> Result<Option<String>> {
        self.add_with_rng(tracks, &mut rand::rng())
    }
    fn add_with_rng(
        &mut self,
        tracks: Vec<Track>,
        rng: &mut impl rand::Rng,
    ) -> Result<Option<String>> {
        if self.state.queue.len() + tracks.len() > 10_000 {
            bail!("Queue limit is 10,000 entries");
        }
        let items: Vec<_> = tracks.into_iter().map(QueueItem::new).collect();
        let first = items.first().map(|q| q.id.clone());
        self.upcoming.extend(items.iter().map(|q| q.id.clone()));
        if self.state.shuffle && !items.is_empty() {
            // Mix additions into the unplayed pool, without revisiting played entries.
            self.upcoming.make_contiguous().shuffle(rng);
        }
        self.state.queue.extend(items);
        Ok(first)
    }
    pub fn play_track(&mut self, track: Track) -> Result<()> {
        let existing = self
            .state
            .current()
            .filter(|item| item.track.id == track.id)
            .or_else(|| {
                self.state
                    .queue
                    .iter()
                    .find(|item| item.track.id == track.id)
            })
            .map(|item| item.id.clone());
        let id = match existing {
            Some(id) => Some(id),
            None => self.add(vec![track])?,
        };
        self.apply(&Command::Play {
            paths: vec![],
            track: None,
            queue_item: id,
        })
    }
    pub fn apply(&mut self, command: &Command) -> Result<()> {
        match command {
            Command::Play {
                queue_item: Some(id),
                ..
            } => {
                let index = self.index(id)?;
                self.play_at(index, 0, false)?;
            }
            Command::Resume | Command::Play { .. } => self.resume()?,
            Command::Pause => self.pause(),
            Command::Toggle => {
                if self.state.status == PlaybackStatus::Playing {
                    self.pause();
                } else {
                    self.resume()?;
                }
            }
            Command::Stop => self.stop(),
            Command::Next => self.advance(false)?,
            Command::Prev => self.previous()?,
            Command::Seek {
                milliseconds,
                relative,
            } => {
                let duration = self
                    .state
                    .current()
                    .map(|q| q.track.duration_ms)
                    .ok_or_else(|| anyhow::anyhow!("Queue is empty"))?;
                let base = if *relative {
                    self.state.position_ms as i128
                } else {
                    0
                };
                let target = (base + *milliseconds as i128).clamp(0, duration as i128) as u64;
                if self.loaded
                    && let Err(error) = self.backend.seek(target)
                {
                    if error.is::<OutputUnavailable>() {
                        self.backend.stop();
                        self.loaded = false;
                        self.output_retry = Some(Instant::now());
                    } else {
                        return Err(error);
                    }
                }
                self.state.position_ms = target;
            }
            Command::Volume { value: Some(value) } => {
                if *value > 100 {
                    bail!("Volume must be between 0 and 100");
                }
                self.backend.volume(*value);
                self.state.volume = *value;
            }
            Command::Shuffle { enabled } => {
                self.state.shuffle = *enabled;
                self.refill_shuffle();
            }
            Command::Repeat { mode } => self.state.repeat = *mode,
            Command::QueueRemove { id } => {
                let index = self.index(id)?;
                let current = self.state.current_id.as_ref() == Some(id);
                let was_playing = self.state.status == PlaybackStatus::Playing;
                self.state.queue.remove(index);
                self.upcoming.retain(|i| i != id);
                self.history.retain(|i| i != id);
                if current {
                    self.stop();
                    self.state.current_id = None;
                    if !self.state.queue.is_empty() {
                        self.play_at(index.min(self.state.queue.len() - 1), 0, !was_playing)?;
                    }
                }
            }
            Command::QueueMove { id, index } => {
                if *index >= self.state.queue.len() {
                    bail!("Destination index is outside the queue");
                }
                let old = self.index(id)?;
                let item = self.state.queue.remove(old);
                self.state.queue.insert(*index, item);
            }
            Command::QueueClear => {
                self.stop();
                self.state.queue.clear();
                self.state.current_id = None;
                self.upcoming.clear();
                self.history.clear();
            }
            _ => bail!("Not a playback command"),
        }
        Ok(())
    }
    fn index(&self, id: &str) -> Result<usize> {
        self.state
            .queue
            .iter()
            .position(|q| q.id == id)
            .ok_or_else(|| anyhow::anyhow!("Queue item not found: {id}"))
    }
    fn pause(&mut self) {
        if self.state.status == PlaybackStatus::Playing {
            self.backend.pause();
            if self.loaded {
                self.state.position_ms = self.backend.position();
            }
            self.state.status = PlaybackStatus::Paused;
        }
    }
    fn resume(&mut self) -> Result<()> {
        if self.state.status == PlaybackStatus::Playing {
            return Ok(());
        }
        if self.state.queue.is_empty() {
            bail!("Queue is empty. Add music with vtamp queue add PATH");
        }
        if self.output_retry.is_some() {
            self.state.status = PlaybackStatus::Playing;
            self.output_retry = Some(Instant::now());
            return Ok(());
        }
        if self.loaded {
            self.backend.resume();
            self.state.status = PlaybackStatus::Playing;
        } else {
            let index = self.state.current_index().unwrap_or(0);
            self.play_at(index, self.state.position_ms, false)?;
        }
        Ok(())
    }
    pub fn stop(&mut self) {
        self.output_retry = None;
        self.backend.stop();
        self.loaded = false;
        self.state.status = PlaybackStatus::Stopped;
        self.state.position_ms = 0;
    }
    fn play_at(&mut self, index: usize, position_ms: u64, paused: bool) -> Result<()> {
        self.output_retry = None;
        let old = self.state.current_id.clone();
        let mut last_error = None;
        // Every candidate is attempted at most once, even with repeat-all enabled.
        for candidate in index..self.state.queue.len() {
            let item = &self.state.queue[candidate];
            let position = if candidate == index { position_ms } else { 0 };
            match self
                .backend
                .load(&item.track.path, position, self.state.volume, paused)
            {
                Err(error) if !error.is::<OutputUnavailable>() => {
                    last_error = Some(format!("{}: {error:#}", item.track.title));
                }
                result => {
                    let output_error = result.err();
                    if output_error.is_some() {
                        self.backend.stop();
                    }
                    if let Some(id) = old
                        && id != item.id
                    {
                        self.history.push(id);
                    }
                    self.state.current_id = Some(item.id.clone());
                    self.upcoming.retain(|id| id != &item.id);
                    self.state.position_ms = position;
                    self.state.status = if paused {
                        PlaybackStatus::Paused
                    } else {
                        PlaybackStatus::Playing
                    };
                    self.loaded = output_error.is_none();
                    self.output_retry = output_error
                        .as_ref()
                        .map(|_| Instant::now() + OUTPUT_RETRY_INTERVAL);
                    self.state.last_error = output_error
                        .map(|error| format!("Waiting for audio output; retrying: {error:#}"))
                        .or(last_error);
                    return Ok(());
                }
            }
        }
        self.stop();
        self.state.last_error = last_error.clone();
        bail!(
            "{}",
            last_error.unwrap_or_else(|| "No playable tracks".into())
        )
    }
    fn refill_shuffle(&mut self) {
        let mut ids: Vec<_> = self
            .state
            .queue
            .iter()
            .filter(|q| Some(&q.id) != self.state.current_id.as_ref())
            .map(|q| q.id.clone())
            .collect();
        ids.shuffle(&mut rand::rng());
        self.upcoming = ids.into();
    }
    fn advance(&mut self, natural: bool) -> Result<()> {
        if self.state.queue.is_empty() {
            self.stop();
            return Ok(());
        }
        if natural && self.state.repeat == Repeat::One {
            return self.play_at(self.state.current_index().unwrap_or(0), 0, false);
        }
        let next = if self.state.shuffle {
            if self.upcoming.is_empty() && self.state.repeat == Repeat::All {
                self.refill_shuffle();
            }
            self.upcoming
                .pop_front()
                .and_then(|id| self.index(&id).ok())
                .or_else(|| {
                    (self.state.repeat == Repeat::All && self.state.queue.len() == 1).then_some(0)
                })
        } else {
            match self.state.current_index() {
                None => Some(0),
                Some(i) if i + 1 < self.state.queue.len() => Some(i + 1),
                _ if self.state.repeat == Repeat::All => Some(0),
                _ => None,
            }
        };
        if let Some(index) = next {
            self.play_at(index, 0, false)?;
        } else {
            self.stop();
        }
        Ok(())
    }
    fn previous(&mut self) -> Result<()> {
        if self.state.queue.is_empty() {
            return Ok(());
        }
        let previous = if self.state.shuffle {
            self.history
                .pop()
                .and_then(|id| self.index(&id).ok())
                .unwrap_or(0)
        } else {
            self.state.current_index().unwrap_or(0).saturating_sub(1)
        };
        let history = self.history.clone();
        self.play_at(previous, 0, false)?;
        self.history = history;
        Ok(())
    }
    pub fn tick(&mut self) -> bool {
        self.tick_at(Instant::now())
    }
    fn tick_at(&mut self, now: Instant) -> bool {
        // output_event may discard the old player, so save its position first.
        if self.loaded {
            self.state.position_ms = self.backend.position();
        }
        if let Some(reason) = self.backend.output_event()
            && (self.loaded || self.output_retry.is_some())
        {
            tracing::info!("Reopening audio output: {reason}");
            self.loaded = false;
            self.output_retry.get_or_insert(now);
        }
        if let Some(retry_at) = self.output_retry {
            if now < retry_at {
                return false;
            }
            let Some(item) = self.state.current() else {
                self.output_retry = None;
                return false;
            };
            // Do not use play_at: an unavailable output must never skip songs or
            // reset shuffle/history, and a paused track must remain paused.
            match self.backend.load(
                &item.track.path,
                self.state.position_ms,
                self.state.volume,
                self.state.status != PlaybackStatus::Playing,
            ) {
                Ok(()) => {
                    self.loaded = true;
                    self.output_retry = None;
                    self.state.last_error = None;
                    tracing::info!("Audio output restored");
                    return true;
                }
                Err(error) => {
                    self.output_retry = Some(now + OUTPUT_RETRY_INTERVAL);
                    let message = format!("Waiting for audio output; retrying: {error:#}");
                    let changed = self.state.last_error.as_ref() != Some(&message);
                    self.state.last_error = Some(message);
                    return changed;
                }
            }
        }
        if self.state.status != PlaybackStatus::Playing {
            return false;
        }
        self.state.position_ms = self.backend.position();
        if self.backend.finished() {
            if let Err(e) = self.advance(true) {
                self.state.last_error = Some(e.to_string());
            }
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    #[derive(Default)]
    struct Fake {
        ended: Arc<AtomicBool>,
        position: u64,
        loads: usize,
        output_event: Option<String>,
        unavailable: bool,
        last_load: Option<(String, u64, u8, bool)>,
    }
    impl PlaybackBackend for Fake {
        fn load(&mut self, p: &Path, pos: u64, volume: u8, paused: bool) -> Result<()> {
            self.loads += 1;
            self.last_load = Some((p.to_string_lossy().into_owned(), pos, volume, paused));
            if self.unavailable {
                return Err(
                    anyhow::anyhow!("No output device available").context(OutputUnavailable)
                );
            }
            if p.to_string_lossy().contains("bad") {
                bail!("damaged");
            }
            self.position = pos;
            self.ended.store(false, Ordering::SeqCst);
            Ok(())
        }
        fn pause(&mut self) {}
        fn resume(&mut self) {}
        fn stop(&mut self) {}
        fn volume(&mut self, _: u8) {}
        fn seek(&mut self, p: u64) -> Result<()> {
            if self.unavailable {
                self.position = 0;
                return Err(
                    anyhow::anyhow!("Output disappeared during seek").context(OutputUnavailable)
                );
            }
            self.position = p;
            Ok(())
        }
        fn position(&self) -> u64 {
            self.position
        }
        fn finished(&self) -> bool {
            self.ended.load(Ordering::SeqCst)
        }
        fn output_event(&mut self) -> Option<String> {
            let event = self.output_event.take();
            if event.is_some() {
                self.position = 0;
            }
            event
        }
    }
    fn track(name: &str) -> Track {
        Track {
            id: name.into(),
            path: name.into(),
            title: name.into(),
            artist: "artist".into(),
            album: "album".into(),
            track_number: 0,
            duration_ms: 60000,
            cover: None,
        }
    }
    fn engine() -> Engine<Fake> {
        let mut engine = Engine::new(State::default(), Fake::default());
        engine
            .add(vec![track("a"), track("b"), track("c")])
            .unwrap();
        engine
    }
    #[test]
    fn output_change_restores_same_track_position_volume_and_pause_state() {
        for paused in [false, true] {
            let mut e = engine();
            e.apply(&Command::Shuffle { enabled: true }).unwrap();
            e.apply(&Command::Repeat { mode: Repeat::All }).unwrap();
            e.apply(&Command::Volume { value: Some(36) }).unwrap();
            e.apply(&Command::Resume).unwrap();
            e.backend.position = 12345;
            if paused {
                e.apply(&Command::Pause).unwrap();
            }
            let queue = e.state.queue.clone();
            let current = e.state.current_id.clone();
            let upcoming = e.upcoming.clone();
            let history = e.history.clone();
            e.backend.output_event = Some("Default audio output changed".into());

            assert!(e.tick());
            assert_eq!(e.backend.last_load, Some(("a".into(), 12345, 36, paused)));
            assert_eq!(e.state.position_ms, 12345);
            assert_eq!(e.state.current_id, current);
            assert_eq!(e.state.queue, queue);
            assert_eq!(e.upcoming, upcoming);
            assert_eq!(e.history, history);
            assert_eq!(
                e.state.status,
                if paused {
                    PlaybackStatus::Paused
                } else {
                    PlaybackStatus::Playing
                }
            );
            assert!(e.loaded);
            assert!(e.state.last_error.is_none());
        }
    }

    #[test]
    fn unavailable_output_retries_without_skipping_and_honors_controls() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.backend.position = 12345;
        e.backend.output_event = Some("Disconnected".into());
        e.backend.unavailable = true;
        let now = Instant::now();
        assert!(e.tick_at(now));
        assert_eq!(e.backend.loads, 2);
        assert_eq!(e.state.current().unwrap().track.id, "a");
        assert_eq!(e.state.position_ms, 12345);
        assert!(e.state.last_error.as_ref().unwrap().contains("retrying"));
        assert!(!e.tick_at(now + Duration::from_millis(500)));
        assert_eq!(e.backend.loads, 2);
        // Still unavailable after the retry interval: exactly one more attempt.
        assert!(!e.tick_at(now + OUTPUT_RETRY_INTERVAL));
        assert_eq!(e.backend.loads, 3);
        assert_eq!(e.state.position_ms, 12345);

        e.apply(&Command::Pause).unwrap();
        assert_eq!(e.state.position_ms, 12345);
        e.apply(&Command::Seek {
            milliseconds: 2000,
            relative: true,
        })
        .unwrap();
        e.apply(&Command::Volume { value: Some(25) }).unwrap();
        e.backend.unavailable = false;
        assert!(e.tick_at(now + OUTPUT_RETRY_INTERVAL * 2));
        assert_eq!(e.backend.last_load, Some(("a".into(), 14345, 25, true)));
        assert_eq!(e.state.status, PlaybackStatus::Paused);
        assert_eq!(e.state.queue.len(), 3);
        assert!(e.state.last_error.is_none());
    }

    #[test]
    fn output_loss_during_seek_preserves_requested_position_for_recovery() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.backend.unavailable = true;
        e.apply(&Command::Seek {
            milliseconds: 12000,
            relative: false,
        })
        .unwrap();
        assert!(!e.loaded);
        assert_eq!(e.state.position_ms, 12000);
        e.apply(&Command::Pause).unwrap();
        e.backend.unavailable = false;
        assert!(e.tick());
        assert_eq!(e.backend.last_load, Some(("a".into(), 12000, 70, true)));
        assert_eq!(e.state.status, PlaybackStatus::Paused);
    }

    #[test]
    fn play_during_output_loss_waits_on_requested_track_instead_of_skipping() {
        let mut e = engine();
        e.backend.unavailable = true;
        e.play_track(track("b")).unwrap();
        assert_eq!(e.backend.loads, 1);
        assert_eq!(e.state.current().unwrap().track.id, "b");
        assert_eq!(e.state.queue.len(), 3);
        assert!(e.output_retry.is_some());
        e.backend.unavailable = false;
        assert!(e.tick_at(Instant::now() + OUTPUT_RETRY_INTERVAL));
        assert_eq!(e.backend.last_load, Some(("b".into(), 0, 70, false)));
    }

    #[test]
    fn stop_cancels_output_recovery() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.backend.output_event = Some("Disconnected".into());
        e.backend.unavailable = true;
        let now = Instant::now();
        e.tick_at(now);
        e.apply(&Command::Stop).unwrap();
        e.backend.unavailable = false;
        assert!(!e.tick_at(now + OUTPUT_RETRY_INTERVAL));
        assert_eq!(e.backend.loads, 2);
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
        assert_eq!(e.state.position_ms, 0);
    }

    #[test]
    fn pause_resume_seek_and_stop_are_consistent() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::Resume).unwrap();
        assert_eq!(e.backend.loads, 1);
        e.apply(&Command::Seek {
            milliseconds: 12000,
            relative: false,
        })
        .unwrap();
        e.apply(&Command::Pause).unwrap();
        e.apply(&Command::Pause).unwrap();
        assert_eq!(e.state.position_ms, 12000);
        e.apply(&Command::Seek {
            milliseconds: -30000,
            relative: true,
        })
        .unwrap();
        assert_eq!(e.state.position_ms, 0);
        e.apply(&Command::Stop).unwrap();
        assert_eq!(e.state.queue.len(), 3);
    }
    #[test]
    fn natural_repeat_one_does_not_trap_manual_next() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::Repeat { mode: Repeat::One }).unwrap();
        e.backend.ended.store(true, Ordering::SeqCst);
        assert!(e.tick());
        assert_eq!(e.state.current().unwrap().track.id, "a");
        e.apply(&Command::Next).unwrap();
        assert_eq!(e.state.current().unwrap().track.id, "b");
    }
    #[test]
    fn failed_tracks_are_bounded_and_skipped() {
        let mut e = Engine::new(State::default(), Fake::default());
        e.add(vec![track("bad1"), track("bad2"), track("good")])
            .unwrap();
        e.apply(&Command::Resume).unwrap();
        assert_eq!(e.backend.loads, 3);
        assert_eq!(e.state.current().unwrap().track.id, "good");
        e.apply(&Command::QueueClear).unwrap();
        e.add(vec![track("bad1"), track("bad2")]).unwrap();
        e.apply(&Command::Repeat { mode: Repeat::All }).unwrap();
        assert!(e.apply(&Command::Resume).is_err());
        assert_eq!(e.backend.loads, 5);
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
    }
    #[test]
    fn shuffle_visits_every_entry_once() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::Shuffle { enabled: true }).unwrap();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..3 {
            seen.insert(e.state.current_id.clone().unwrap());
            e.apply(&Command::Next).unwrap();
        }
        assert_eq!(seen.len(), 3);
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
    }
    #[test]
    fn shuffle_randomizes_tracks_added_after_enabling_it() {
        use rand::{SeedableRng, rngs::StdRng};

        for natural in [false, true] {
            let mut e = Engine::new(State::default(), Fake::default());
            e.apply(&Command::Shuffle { enabled: true }).unwrap();
            let tracks: Vec<_> = (0..16).map(|i| track(&format!("song-{i}"))).collect();
            e.add_with_rng(tracks, &mut StdRng::seed_from_u64(42))
                .unwrap();
            let queue = e.state.queue.clone();
            e.apply(&Command::Resume).unwrap();
            let mut played = vec![];
            while e.state.status == PlaybackStatus::Playing {
                assert!(
                    played.len() < queue.len(),
                    "shuffle must finish a traversal"
                );
                played.push(e.state.current_id.clone().unwrap());
                if natural {
                    e.backend.ended.store(true, Ordering::SeqCst);
                    assert!(e.tick());
                } else {
                    e.apply(&Command::Next).unwrap();
                }
            }
            let ordered: Vec<_> = queue.iter().map(|q| q.id.clone()).collect();
            assert_ne!(
                played, ordered,
                "new entries must not play in insertion order"
            );
            played.sort();
            let mut expected = ordered;
            expected.sort();
            assert_eq!(played, expected, "each entry must play exactly once");
            assert_eq!(
                e.state.queue, queue,
                "shuffle must not reorder the visible queue"
            );
        }
    }

    #[test]
    fn adding_during_shuffle_mixes_only_unplayed_entries() {
        use rand::{SeedableRng, rngs::StdRng};

        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::Shuffle { enabled: true }).unwrap();
        e.apply(&Command::Next).unwrap();
        let current = e.state.current_id.clone();
        let history = e.history.clone();
        let remaining: Vec<_> = e.upcoming.iter().cloned().collect();
        e.add_with_rng(
            (0..16).map(|i| track(&format!("added-{i}"))).collect(),
            &mut StdRng::seed_from_u64(42),
        )
        .unwrap();
        let mut insertion_order = remaining;
        insertion_order.extend(e.state.queue[3..].iter().map(|q| q.id.clone()));
        let mut upcoming: Vec<_> = e.upcoming.iter().cloned().collect();
        assert_ne!(upcoming, insertion_order);
        upcoming.sort();
        insertion_order.sort();
        assert_eq!(
            upcoming, insertion_order,
            "do not replay already visited entries"
        );
        assert_eq!(e.state.current_id, current);
        assert_eq!(e.history, history);
    }
    #[test]
    fn library_play_reuses_current_duplicate_then_first_match() {
        let mut e = engine();
        let first = e.state.queue[0].id.clone();
        let duplicate = e.add(vec![track("a")]).unwrap().unwrap();
        e.apply(&Command::Play {
            paths: vec![],
            track: None,
            queue_item: Some(duplicate.clone()),
        })
        .unwrap();
        let queue = e.state.queue.clone();

        e.play_track(track("a")).unwrap();
        assert_eq!(e.state.current_id.as_ref(), Some(&duplicate));
        assert_eq!(e.state.status, PlaybackStatus::Playing);
        assert_eq!(e.state.queue, queue);

        e.play_track(track("b")).unwrap();
        e.play_track(track("a")).unwrap();
        assert_eq!(e.state.current_id.as_ref(), Some(&first));
        assert_eq!(e.state.queue, queue);
    }

    #[test]
    fn library_play_appends_missing_track_once_and_explicit_add_allows_duplicates() {
        let mut e = engine();
        e.play_track(track("new")).unwrap();
        let added = e.state.current_id.clone();
        assert_eq!(e.state.queue.len(), 4);
        assert_eq!(e.state.queue[3].id, added.clone().unwrap());
        assert_eq!(e.state.current().unwrap().track.id, "new");

        for _ in 0..3 {
            e.play_track(track("new")).unwrap();
        }
        assert_eq!(e.state.queue.len(), 4);
        assert_eq!(e.state.current_id, added);

        e.add(vec![track("new")]).unwrap();
        assert_eq!(e.state.queue.len(), 5);
        assert_ne!(e.state.queue[3].id, e.state.queue[4].id);
        assert_eq!(e.state.current_id, added);
    }

    #[test]
    fn duplicate_tracks_have_independent_queue_identity() {
        let mut e = engine();
        e.add(vec![track("a")]).unwrap();
        let original = e.state.queue[0].id.clone();
        let duplicate = e.state.queue[3].id.clone();
        assert_ne!(original, duplicate);
        e.apply(&Command::QueueMove {
            id: original.clone(),
            index: 2,
        })
        .unwrap();
        e.apply(&Command::QueueRemove { id: original }).unwrap();
        assert!(e.state.queue.iter().any(|q| q.id == duplicate));
    }
}
