//! Deferred tmux graphics discovery. Only EventStream reads terminal input;
//! this worker owns tmux subprocesses and the pane's passthrough lease.
mod replies;
pub(crate) use replies::ReplyFilter;

use super::*;
use std::{sync::mpsc, thread};
use tokio::sync::mpsc as async_mpsc;

const INTERVAL: Duration = Duration::from_millis(500);

pub(crate) struct Reply {
    pub id: u32,
    pub ok: bool,
}

pub(crate) enum Update {
    Query { id: u32, expires: Instant },
    Graphics { artwork: Artwork },
}

enum Control {
    Focus(bool),
    Reply(Reply),
    Stop,
}

pub(crate) struct Detector {
    pub updates: async_mpsc::UnboundedReceiver<Update>,
    controls: mpsc::SyncSender<Control>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Detector {
    pub fn start(art: Art) -> Self {
        let (updates, receiver) = async_mpsc::unbounded_channel();
        let (controls, commands) = mpsc::sync_channel(32);
        let worker = (matches!(art, Art::Auto) && in_tmux())
            .then(|| thread::spawn(move || worker(commands, updates)));
        Self {
            updates: receiver,
            controls,
            worker,
        }
    }

    pub fn enabled(&self) -> bool {
        self.worker.is_some()
    }

    pub fn focus(&self, gained: bool) {
        let _ = self.controls.try_send(Control::Focus(gained));
    }

    pub fn reply(&self, reply: Reply) {
        let _ = self.controls.try_send(Control::Reply(reply));
    }

    /// Write on the presentation thread so a probe cannot split a frame upload.
    /// No DA1/DSR/cell queries: Crossterm does not expose their raw replies.
    pub fn query(id: u32) -> String {
        format!(
            "\x1bPtmux;\x1b\x1b_Gi={id},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\x1b\\\x1b\x1b_Gi={},s=1,v=1,a=q,t=d,f=24,o=z;eJxjYGAAAAADAAE=\x1b\x1b\\\x1b\\",
            id + 1,
        )
    }
}

impl Drop for Detector {
    fn drop(&mut self) {
        // Shutdown is the only join on the TUI thread. Passthrough restoration
        // finishes before returning to the shell, even after a signal or error.
        let _ = self.controls.send(Control::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct Schedule {
    active: bool,
    retry: Option<Instant>,
    retried: bool,
    finished: bool,
}

impl Schedule {
    fn inspect(&mut self, active: bool, now: Instant) -> bool {
        if self.finished {
            return false;
        }
        if !active {
            self.active = false;
            self.retry = None;
            self.retried = false;
            return false;
        }
        let became_active = !self.active;
        self.active = true;
        if became_active {
            return true;
        }
        if self.retry.is_some_and(|at| now >= at) {
            self.retry = None;
            self.retried = true;
            return true;
        }
        false
    }

    fn complete(&mut self, kitty: bool, timed_out: bool, now: Instant) {
        self.finished = kitty;
        if timed_out && !self.retried {
            self.retry = Some(now + INTERVAL);
        }
    }
}

struct Pending {
    id: u32,
    expires: Instant,
    kitty: Option<bool>,
    compression: Option<bool>,
    caps: Capabilities,
}

impl Pending {
    fn accept(&mut self, reply: Reply) {
        if reply.id == self.id {
            self.kitty = Some(reply.ok);
        }
        if reply.id == self.id + 1 {
            self.compression = Some(reply.ok);
        }
    }

    fn ready(&self, now: Instant) -> bool {
        now >= self.expires
            || self.kitty == Some(false)
            || (self.kitty.is_some() && self.compression.is_some())
    }
}

fn pane_report(pane: &str) -> Option<String> {
    tmux_query(&[
        "display-message",
        "-p",
        "-t",
        pane,
        "#{session_attached} #{window_active} #{pane_active} #{sixel_support}",
    ])
}

fn snapshot(report: &str, clients: &str) -> Capabilities {
    let mut caps = Capabilities {
        sixel: report.split_whitespace().nth(3) == Some("1"),
        ..Default::default()
    };
    caps.restrict_to_tmux_clients(clients);
    // Prefer the pane's pixel dimensions; older tmux versions expose only the
    // clients' cell sizes. Never infer pixels from TERM or a terminal name.
    if let Ok(size) = crossterm::terminal::window_size()
        && size.columns > 0
        && size.rows > 0
    {
        let (w, h) = (size.width / size.columns, size.height / size.rows);
        if w > 0 && h > 0 {
            caps.font_size = Some(FontSize::new(w, h));
        }
    }
    caps.font_size = caps.font_size.or(client_font_size(clients));
    caps
}

fn client_font_size(clients: &str) -> Option<FontSize> {
    let mut font = None;
    for client in clients.lines() {
        let fields: Vec<_> = client.split('\t').collect();
        if let (Some(w), Some(h)) = (
            fields.get(1).and_then(|s| s.parse::<u16>().ok()),
            fields.get(2).and_then(|s| s.parse::<u16>().ok()),
        ) && w > 0
            && h > 0
        {
            // tmux uses the smallest cell dimensions among attached clients.
            font = Some(font.map_or(FontSize::new(w, h), |old: FontSize| {
                FontSize::new(old.width.min(w), old.height.min(h))
            }));
        }
    }
    font
}

fn worker(commands: mpsc::Receiver<Control>, updates: async_mpsc::UnboundedSender<Update>) {
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return;
    };
    let mut schedule = Schedule::default();
    let mut next_check = Instant::now();
    let mut pending: Option<Pending> = None;
    let mut passthrough = None;
    loop {
        let now = Instant::now();
        let wake = pending.as_ref().map_or(next_check, |p| p.expires);
        let command = if schedule.finished {
            commands
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        } else {
            commands.recv_timeout(wake.saturating_duration_since(now))
        };
        match command {
            Ok(Control::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Control::Focus(gained)) => {
                if !gained && !schedule.finished {
                    schedule.inspect(false, Instant::now());
                    pending = None;
                    passthrough = None;
                }
                next_check = Instant::now();
            }
            Ok(Control::Reply(reply)) => {
                if let Some(p) = &mut pending {
                    p.accept(reply);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
        if schedule.finished {
            continue;
        }
        let now = Instant::now();
        if pending.as_ref().is_some_and(|p| p.ready(now)) {
            let mut p = pending.take().unwrap();
            // A window switch during the query invalidates its result.
            if !pane_report(&pane).is_some_and(|r| pane_active(&r)) {
                passthrough = None;
                schedule.inspect(false, now);
                next_check = now + INTERVAL;
                continue;
            }
            p.caps.kitty = p.kitty == Some(true);
            p.caps.compress = p.compression == Some(true);
            let kitty = p.caps.kitty && p.caps.font_size.is_some();
            schedule.complete(kitty, p.kitty.is_none() || p.caps.font_size.is_none(), now);
            if !kitty {
                passthrough = None;
            }
            if updates
                .send(Update::Graphics {
                    artwork: Artwork::native(Art::Auto, true, p.caps),
                })
                .is_err()
            {
                break;
            }
            next_check = now + INTERVAL;
        }
        if pending.is_some() || schedule.finished || Instant::now() < next_check {
            continue;
        }
        let report = pane_report(&pane).unwrap_or_default();
        let now = Instant::now();
        next_check = now + INTERVAL;
        if !schedule.inspect(pane_active(&report), now) {
            continue;
        }
        let clients = tmux_client_features().unwrap_or_default();
        let caps = snapshot(&report, &clients);
        passthrough = TmuxPassthrough::enable();
        // Enabling the lease and collecting client features take time. Check
        // eligibility again immediately before asking the UI to send anything.
        if !pane_report(&pane).is_some_and(|r| pane_active(&r)) {
            passthrough = None;
            schedule.inspect(false, Instant::now());
            continue;
        }
        if passthrough.is_none() || caps.font_size.is_none() {
            passthrough = None;
            schedule.complete(false, true, Instant::now());
            if updates
                .send(Update::Graphics {
                    artwork: Artwork::native(Art::Auto, true, caps),
                })
                .is_err()
            {
                break;
            }
            continue;
        }
        let id = rand::random::<u32>().clamp(1, u32::MAX - 1);
        let expires = Instant::now() + PROBE_TIMEOUT;
        pending = Some(Pending {
            id,
            expires,
            kitty: None,
            compression: None,
            caps,
        });
        if updates.send(Update::Query { id, expires }).is_err() {
            break;
        }
    }
    drop(passthrough);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_requires_attached_active_window_and_pane() {
        for report in ["", "1", "0 1 1", "1 0 1", "1 1 0", "x 1 1"] {
            assert!(!pane_active(report), "{report:?}");
        }
        assert!(pane_active("1 1 1 1"));
        assert!(pane_active("2 1 1"));
    }

    #[test]
    fn parked_start_retries_once_then_waits_for_another_activation() {
        let now = Instant::now();
        let mut s = Schedule::default();
        assert!(!s.inspect(false, now));
        assert!(s.inspect(true, now));
        s.complete(false, true, now);
        assert!(!s.inspect(true, now + INTERVAL / 2));
        assert!(s.inspect(true, now + INTERVAL));
        s.complete(false, true, now + INTERVAL);
        assert!(!s.inspect(true, now + INTERVAL * 5));
        assert!(!s.inspect(false, now));
        assert!(s.inspect(true, now));
        s.complete(true, false, now);
        assert!(!s.inspect(false, now));
        assert!(!s.inspect(true, now));
    }

    #[test]
    fn negative_answer_does_not_busy_retry() {
        let now = Instant::now();
        let mut s = Schedule::default();
        assert!(s.inspect(true, now));
        s.complete(false, false, now);
        assert!(!s.inspect(true, now + INTERVAL * 10));
    }

    #[test]
    fn font_size_requires_positive_dimensions_and_uses_smallest_client() {
        let metrics = |clients| client_font_size(clients).map(|f| (f.width, f.height));
        assert_eq!(metrics("RGB,sync\t17\t34\nsync\t10\t20"), Some((10, 20)));
        assert_eq!(metrics("sync\t17\t34\nRGB\t10\t20"), Some((10, 20)));
        for clients in ["", "RGB\t0\t0", "RGB\t17\t", "RGB\t999999\t20"] {
            assert_eq!(metrics(clients), None);
        }
    }

    #[test]
    fn old_replies_cannot_promote_a_new_probe() {
        let now = Instant::now();
        let mut p = Pending {
            id: 100,
            expires: now + PROBE_TIMEOUT,
            kitty: None,
            compression: None,
            caps: Capabilities::default(),
        };
        p.accept(Reply { id: 98, ok: true });
        assert!(!p.ready(now));
        p.accept(Reply { id: 100, ok: true });
        assert!(!p.ready(now));
        p.accept(Reply { id: 101, ok: false });
        assert!(p.ready(now));
        assert_eq!(p.kitty, Some(true));
        assert_eq!(p.compression, Some(false));
    }
}
