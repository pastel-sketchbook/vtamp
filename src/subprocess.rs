//! Bounded, cancellable external processes. No shell interpolation.
use anyhow::{Context, Result, bail};
use std::{
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub type Cancel = Arc<AtomicBool>;
pub fn cancel() -> Cancel {
    Arc::new(AtomicBool::new(false))
}

pub fn run(
    command: &mut Command,
    input: Option<Vec<u8>>,
    stop: &Cancel,
    timeout: Duration,
    mut line: impl FnMut(&str),
) -> Result<Vec<u8>> {
    if stop.load(Ordering::Relaxed) {
        bail!("Request cancelled");
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().context("Cannot start external tool")?;
    let pid = child.id() as i32;
    let (tx, rx) = mpsc::sync_channel(64);
    let mut readers = Vec::new();
    for (stderr, pipe) in [
        (
            false,
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
        ),
        (
            true,
            Box::new(child.stderr.take().unwrap()) as Box<dyn Read + Send>,
        ),
    ] {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            let mut reader = pipe;
            let mut buf = [0; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send((stderr, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        }));
    }
    drop(tx);
    let writer = input.map(|bytes| {
        let mut stdin = child.stdin.take().unwrap();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        })
    });
    let started = Instant::now();
    let mut output = Vec::new();
    let mut diagnostic = Vec::new();
    let mut pending = Vec::new();
    let mut failure = None;
    let status = loop {
        if stop.load(Ordering::Relaxed) || started.elapsed() > timeout {
            failure = Some(if stop.load(Ordering::Relaxed) {
                "Request cancelled"
            } else {
                "External tool timed out"
            });
            break None;
        }
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok((stderr, bytes)) => {
                if stderr {
                    diagnostic.extend_from_slice(&bytes);
                    if diagnostic.len() > 8192 {
                        diagnostic.drain(..diagnostic.len() - 8192);
                    }
                } else {
                    if output.len() + bytes.len() > 8 * 1024 * 1024 {
                        failure = Some("External tool output exceeds 8 MiB");
                        break None;
                    }
                    output.extend_from_slice(&bytes);
                    pending.extend_from_slice(&bytes);
                    while let Some(i) = pending.iter().position(|b| *b == b'\n' || *b == b'\r') {
                        line(&String::from_utf8_lossy(&pending[..i]));
                        pending.drain(..=i);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(_) => {
                    failure = Some("Cannot wait for external tool");
                    break None;
                }
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Continue draining pipes after the parent exits. Descendants are still bounded
                // by the deadline and killed with the process group below.
            }
        }
    };
    // Kill any descendants that retained pipes, even after the main process exited.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.wait();
    drop(rx);
    for reader in readers {
        let _ = reader.join();
    }
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    if let Some(failure) = failure {
        bail!("{failure}");
    }
    if !pending.is_empty() {
        line(&String::from_utf8_lossy(&pending));
    }
    if !status.is_some_and(|s| s.success()) {
        let message: String = String::from_utf8_lossy(&diagnostic)
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .collect();
        bail!("External tool failed: {}", message.trim());
    }
    Ok(output)
}

pub fn executable(explicit: Option<&Path>, name: &str) -> Result<PathBuf> {
    let valid = |p: &Path| {
        p.is_file()
            && p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    };
    if let Some(p) = explicit {
        if p.is_absolute() && valid(p) {
            return Ok(p.to_owned());
        }
        bail!("{name} path must be an absolute executable file");
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path).filter(|p| p.is_absolute()) {
            let p = dir.join(name);
            if valid(&p) {
                return Ok(p);
            }
        }
    }
    bail!("{name} is not installed or not on PATH")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_pipes_do_not_disable_the_deadline() {
        let start = Instant::now();
        let result = run(
            Command::new("/bin/sh").args(["-c", "exec >/dev/null 2>&1; sleep 10"]),
            None,
            &cancel(),
            Duration::from_millis(100),
            |_| {},
        );
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn cancellation_kills_descendants_holding_pipes() {
        let stop = cancel();
        let child_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            child_stop.store(true, Ordering::Relaxed);
        });
        let start = Instant::now();
        let result = run(
            Command::new("/bin/sh").args(["-c", "sleep 10 & wait"]),
            None,
            &stop,
            Duration::from_secs(5),
            |_| {},
        );
        thread.join().unwrap();
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
