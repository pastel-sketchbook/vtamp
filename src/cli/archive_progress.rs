use crate::archive::Progress;
use std::{
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthChar;

pub(super) struct Display {
    operation: &'static str,
    terminal: bool,
    started: Instant,
    last_draw: Instant,
    previous: Option<Progress>,
    active_line: bool,
}

impl Display {
    pub fn new(operation: &'static str) -> Self {
        Self {
            operation,
            terminal: io::stderr().is_terminal()
                && std::env::var("TERM").is_ok_and(|s| s != "dumb"),
            started: Instant::now(),
            last_draw: Instant::now(),
            previous: None,
            active_line: false,
        }
    }

    pub fn update(&mut self, progress: &Progress) {
        let changed_stage = self
            .previous
            .as_ref()
            .is_none_or(|old| old.stage != progress.stage);
        let finished_stage = progress.items_total > 0
            && progress.items_done == progress.items_total
            && self
                .previous
                .as_ref()
                .is_none_or(|old| old.items_done != progress.items_done);
        let interval = if self.terminal {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        };
        if !changed_stage && !finished_stage && self.last_draw.elapsed() < interval {
            return;
        }
        let line = format!(
            "{} · {} · {}s",
            self.operation,
            description(progress),
            self.started.elapsed().as_secs()
        );
        let mut out = io::stderr().lock();
        if self.terminal {
            let width = crossterm::terminal::size()
                .map_or(80, |(width, _)| usize::from(width))
                .saturating_sub(1);
            let _ = write!(out, "\r\x1b[2K{}", truncate(&line, width));
            self.active_line = true;
        } else {
            let _ = writeln!(out, "{line}");
        }
        let _ = out.flush();
        self.last_draw = Instant::now();
        self.previous = Some(progress.clone());
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        if self.active_line {
            let _ = writeln!(io::stderr());
        }
    }
}

pub(super) fn description(progress: &Progress) -> String {
    let (label, unit) = match progress.stage.as_str() {
        "snapshot" => ("Reading Library", ""),
        "starting" => ("Starting restore", ""),
        "hashing" => ("Hashing", "tracks"),
        "compressing" => ("Compressing", "files"),
        "finalizing" => ("Finalizing archive", ""),
        "reading_manifest" => ("Reading archive", ""),
        "extracting" => ("Extracting and verifying", "files"),
        "validating" => ("Validating media", "tracks"),
        "checking_library" => ("Checking existing Library", "tracks"),
        "planning" => ("Checking duplicates and references", "tracks"),
        "preparing" => ("Preparing restore", "tracks"),
        "publishing" => ("Restoring files", "tracks"),
        "committing" => ("Saving Library", ""),
        "cleaning_up" => ("Cleaning up", ""),
        "rolling_back" => ("Rolling back", ""),
        "completed" => ("Completed", ""),
        "partial" => ("Completed with missing references", ""),
        "failed" => ("Failed", ""),
        _ => ("Waiting for server", ""),
    };
    let mut line = label.to_owned();
    if !unit.is_empty() {
        line.push_str(&format!(
            " {}/{} {unit}",
            progress.items_done, progress.items_total
        ));
    }
    if let Some(total) = progress.bytes_total {
        let percent = if total == 0 {
            100.0
        } else {
            (progress.bytes_done as f64 / total as f64 * 100.0).min(100.0)
        };
        line.push_str(&format!(
            " · {} / {} ({percent:.0}%)",
            bytes(progress.bytes_done),
            bytes(total)
        ));
    } else if progress.bytes_done > 0 {
        line.push_str(&format!(" · {}", bytes(progress.bytes_done)));
    }
    if let Some(name) = &progress.current {
        let name: String = name.chars().filter(|c| !c.is_control()).take(160).collect();
        line.push_str(&format!(" · {name}"));
    }
    line
}

fn bytes(value: u64) -> String {
    for (unit, divisor) in [("GiB", 1u64 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)] {
        if value >= divisor {
            return format!("{:.1} {unit}", value as f64 / divisor as f64);
        }
    }
    format!("{value} B")
}

fn truncate(line: &str, width: usize) -> String {
    let mut used = 0;
    line.chars()
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= width
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_labels_real_units_unknown_totals_and_sanitizes_metadata() {
        let progress = Progress {
            stage: "compressing".into(),
            items_done: 2,
            items_total: 4,
            bytes_done: 1024 * 1024,
            bytes_total: Some(2 * 1024 * 1024),
            current: Some("김동률\n\u{1b}track".into()),
        };
        assert_eq!(
            description(&progress),
            "Compressing 2/4 files · 1.0 MiB / 2.0 MiB (50%) · 김동률track"
        );
        let hash = Progress {
            stage: "hashing".into(),
            bytes_total: None,
            ..progress
        };
        assert!(description(&hash).starts_with("Hashing 2/4 tracks · 1.0 MiB"));
        assert!(!description(&hash).contains('%'));
        assert_eq!(truncate("김동률abcdef", 7), "김동률a");
        assert!(
            !description(&Progress {
                stage: "compressing".into(),
                bytes_total: Some(0),
                ..Default::default()
            })
            .contains("NaN")
        );
    }
}
