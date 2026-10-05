//! Crossterm 0.29 delivers Kitty APC replies as key events (Alt-_, G, ...,
//! Alt-backslash), sometimes with a separate Escape at a read boundary. Keep
//! these control strings out of App::key without opening a second tty reader.
use super::{PROBE_TIMEOUT, Reply};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use std::{
    mem,
    time::{Duration, Instant},
};

// Only the ambiguous Escape/APC prefix waits this long; ordinary keys, paste,
// resize and focus events are forwarded immediately, even during a probe.
const PREFIX_TIMEOUT: Duration = Duration::from_millis(25);
const MAX_REPLY: usize = 1024;

#[derive(Default)]
pub(crate) struct ReplyFilter {
    prefix_events: Vec<Event>,
    text: String,
    until: Option<Instant>,
    discard: bool,
    escape: bool,
}

impl ReplyFilter {
    pub fn deadline(&self) -> Option<Instant> {
        self.until
    }

    pub fn expire(&mut self, now: Instant) -> Vec<Event> {
        if self.until.is_some_and(|until| now >= until) {
            let events = mem::take(&mut self.prefix_events);
            self.reset();
            events
        } else {
            vec![]
        }
    }

    pub fn push(&mut self, event: Event, now: Instant) -> (Vec<Event>, Option<Reply>) {
        let mut events = self.expire(now);
        let fragment = match &event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, m) if m.is_empty() => Some("\x1b".to_owned()),
                    (KeyCode::Char(ch), m) if (m - KeyModifiers::SHIFT).is_empty() => {
                        Some(ch.to_string())
                    }
                    (KeyCode::Char(ch), m) if (m - KeyModifiers::SHIFT) == KeyModifiers::ALT => {
                        Some(format!("\x1b{ch}"))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let Some(fragment) = fragment else {
            if !self.prefix_events.is_empty() && matches!(event, Event::Key(_) | Event::Paste(_)) {
                events.append(&mut self.prefix_events);
                self.reset();
            }
            events.push(event);
            return (events, None);
        };
        if self.until.is_none() {
            if fragment == "\x1b" || fragment == "\x1b_" {
                self.text = fragment;
                self.prefix_events.push(event);
                self.until = Some(now + PREFIX_TIMEOUT);
            } else {
                events.push(event);
            }
            return (events, None);
        }
        if !self.text.starts_with("\x1b_G") && !self.discard {
            let candidate = format!("{}{fragment}", self.text);
            if "\x1b_G".starts_with(&candidate) {
                self.text = candidate;
                self.prefix_events.push(event);
                if self.text == "\x1b_G" {
                    self.prefix_events.clear();
                    self.until = Some(now + PROBE_TIMEOUT);
                }
            } else {
                events.append(&mut self.prefix_events);
                self.reset();
                let (rest, reply) = self.push(event, now);
                events.extend(rest);
                return (events, reply);
            }
            return (events, None);
        }
        for ch in fragment.chars() {
            let end = self.escape && ch == '\\';
            self.escape = ch == '\x1b';
            if !self.discard {
                if self.text.len() + ch.len_utf8() <= MAX_REPLY {
                    self.text.push(ch);
                } else {
                    self.text.clear();
                    self.discard = true;
                }
            }
            if end {
                let reply = if self.discard {
                    None
                } else {
                    parse(&self.text)
                };
                self.reset();
                return (events, reply);
            }
        }
        (events, None)
    }

    fn reset(&mut self) {
        self.prefix_events.clear();
        self.text.clear();
        self.until = None;
        self.discard = false;
        self.escape = false;
    }
}

fn parse(text: &str) -> Option<Reply> {
    let body = text.strip_prefix("\x1b_G")?.strip_suffix("\x1b\\")?;
    let (header, answer) = body.split_once(';')?;
    let id = header
        .split(',')
        .find_map(|field| field.strip_prefix("i="))?
        .parse()
        .ok()?;
    Some(Reply {
        id,
        ok: answer == "OK",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn alt(ch: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::ALT))
    }

    #[test]
    fn response_never_becomes_navigation_or_queue_keys() {
        for split in [false, true] {
            let mut filter = ReplyFilter::default();
            let now = Instant::now();
            let mut input = vec![key(KeyCode::Char('j'))];
            if split {
                input.extend([key(KeyCode::Esc), key(KeyCode::Char('_'))]);
            } else {
                input.push(alt('_'));
            }
            input.extend("Gi=500;ENOTSUP".chars().map(|ch| key(KeyCode::Char(ch))));
            input.push(Event::Paste("한글\x1b_Gi=500;OK\x1b\\".into()));
            input.push(Event::FocusGained);
            if split {
                input.extend([key(KeyCode::Esc), key(KeyCode::Char('\\'))]);
            } else {
                input.push(alt('\\'));
            }
            input.push(key(KeyCode::Char('k')));
            let mut forwarded = vec![];
            let mut replies = vec![];
            for event in input {
                let (events, reply) = filter.push(event, now);
                forwarded.extend(events);
                replies.extend(reply);
            }
            assert_eq!(
                forwarded,
                vec![
                    key(KeyCode::Char('j')),
                    Event::Paste("한글\x1b_Gi=500;OK\x1b\\".into()),
                    Event::FocusGained,
                    key(KeyCode::Char('k'))
                ]
            );
            assert_eq!(replies.len(), 1);
            assert_eq!(replies[0].id, 500);
            assert!(!replies[0].ok);
        }
    }

    #[test]
    fn ordinary_escape_alt_and_text_are_preserved() {
        let now = Instant::now();
        let mut filter = ReplyFilter::default();
        assert!(filter.push(key(KeyCode::Esc), now).0.is_empty());
        assert_eq!(filter.expire(now + PREFIX_TIMEOUT), vec![key(KeyCode::Esc)]);
        assert!(filter.push(alt('_'), now).0.is_empty());
        assert_eq!(
            filter.push(key(KeyCode::Char('x')), now).0,
            vec![alt('_'), key(KeyCode::Char('x'))]
        );
        for event in [
            key(KeyCode::Char('한')),
            key(KeyCode::Enter),
            alt('x'),
            Event::Resize(40, 12),
        ] {
            assert_eq!(filter.push(event.clone(), now).0, vec![event]);
        }
    }

    #[test]
    fn malformed_oversized_and_late_frames_are_consumed() {
        let now = Instant::now();
        let mut filter = ReplyFilter::default();
        for text in [
            "Gi=not-a-number;OK".to_owned(),
            format!("Gi=42;{}nxdq", "x".repeat(2048)),
        ] {
            assert!(filter.push(alt('_'), now).0.is_empty());
            for ch in text.chars() {
                assert!(filter.push(key(KeyCode::Char(ch)), now).0.is_empty());
            }
            let (events, reply) = filter.push(alt('\\'), now);
            assert!(events.is_empty());
            assert!(reply.is_none());
        }
        let late = now + Duration::from_secs(2);
        filter.push(alt('_'), late);
        for ch in "Gi=42;OK".chars() {
            filter.push(key(KeyCode::Char(ch)), late);
        }
        assert_eq!(filter.push(alt('\\'), late).1.unwrap().id, 42);
        filter.push(alt('_'), now);
        for ch in "Gi=42;".chars() {
            filter.push(key(KeyCode::Char(ch)), now);
        }
        assert!(filter.expire(now + PROBE_TIMEOUT).is_empty());
        assert_eq!(
            filter.push(key(KeyCode::Char('q')), late).0,
            vec![key(KeyCode::Char('q'))]
        );
    }

    // A child process gives Crossterm its own tty/global reader. This tests the
    // actual dependency's event parsing, including escape/read boundaries.
    #[test]
    fn pty_reader_child() {
        if std::env::var_os("VTAMP_TEST_GRAPHICS_PTY").is_none() {
            return;
        }
        use std::io::Write;
        crossterm::terminal::enable_raw_mode().unwrap();
        println!("GRAPHICS_READY");
        std::io::stdout().flush().unwrap();
        let mut filter = ReplyFilter::default();
        let mut forwarded = vec![];
        let mut replies = vec![];
        let end = Instant::now() + Duration::from_secs(4);
        while Instant::now() < end {
            forwarded.extend(filter.expire(Instant::now()));
            if crossterm::event::poll(Duration::from_millis(5)).unwrap() {
                let (events, reply) =
                    filter.push(crossterm::event::read().unwrap(), Instant::now());
                forwarded.extend(events);
                replies.extend(reply);
                if forwarded.last() == Some(&key(KeyCode::Char('!'))) {
                    break;
                }
            }
        }
        crossterm::terminal::disable_raw_mode().unwrap();
        assert_eq!(
            replies.iter().map(|r| (r.id, r.ok)).collect::<Vec<_>>(),
            vec![(500, true), (501, false), (400, true)]
        );
        assert_eq!(
            forwarded,
            vec![
                key(KeyCode::Char('j')),
                Event::Paste("한글".into()),
                key(KeyCode::Char('k')),
                key(KeyCode::Char('!'))
            ]
        );
    }

    #[test]
    fn real_crossterm_pty_preserves_keys_and_paste_around_fragmented_replies() {
        use std::{
            fs::File,
            io::{BufRead, BufReader, Write},
            os::fd::FromRawFd,
            process::{Command, Stdio},
            thread,
        };
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty initializes both owned descriptors on success.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let (mut master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "artwork::redetect::replies::tests::pty_reader_child",
                "--nocapture",
            ])
            .env("VTAMP_TEST_GRAPHICS_PTY", "1")
            .stdin(slave)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert!(
                output.read_line(&mut line).unwrap() > 0,
                "reader failed to start"
            );
            if line.contains("GRAPHICS_READY") {
                break;
            }
        }
        master
            .write_all(b"j\x1b_Gi=500;OK\x1b\\\x1b[200~\xed\x95\x9c\xea\xb8\x80\x1b[201~")
            .unwrap();
        for bytes in [b"\x1b".as_slice(), b"_Gi=501;", b"ENOTSUP", b"\x1b", b"\\k"] {
            master.write_all(bytes).unwrap();
            thread::sleep(Duration::from_millis(5));
        }
        // An old complete reply arriving after the probe timeout is still
        // consumed; only the detector decides whether its ID is current.
        thread::sleep(PROBE_TIMEOUT + Duration::from_millis(20));
        master.write_all(b"\x1b_Gi=400;OK\x1b\\!").unwrap();
        let status = child.wait().unwrap();
        if !status.success() {
            let mut remaining = String::new();
            std::io::Read::read_to_string(&mut output, &mut remaining).unwrap();
            panic!("PTY reader failed: {remaining}");
        }
    }
}
