//! A separately opened descriptor: O_NONBLOCK must never leak to stdin or the
//! stdout handle used by Crossterm. Preserve every byte across short writes.
use std::{
    fs::{File, OpenOptions},
    io::{self, IsTerminal, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
};

pub(super) struct Tty(File);
impl Tty {
    pub(super) fn open_tmux() -> io::Result<Option<Self>> {
        if std::env::var_os("TMUX").is_none() || !io::stdout().is_terminal() {
            return Ok(None);
        }
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open("/dev/tty")?;
        super::diagnostics::record("output.mode", || serde_json::json!({"nonblocking":true}));
        Ok(Some(Self(file)))
    }

    pub(super) fn upload(&mut self, bytes: &[u8]) -> io::Result<()> {
        write_ready(&mut self.0, bytes)
    }
}

fn write_ready(writer: &mut (impl Write + AsRawFd), mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let count = bytes.len().min(16 * 1024);
        let result = {
            let _span = super::diagnostics::span(
                "output.write",
                || serde_json::json!({"bytes":count,"nonblocking":true}),
            );
            writer.write(&bytes[..count])
        };
        match result {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => {
                super::diagnostics::record(
                    "output.progress",
                    || serde_json::json!({"bytes":written}),
                );
                bytes = &bytes[written..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let _span = super::diagnostics::span("output.ready_wait", || serde_json::json!({}));
                let mut fd = libc::pollfd {
                    fd: writer.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                loop {
                    let result = unsafe { libc::poll(&mut fd, 1, 1000) };
                    if result < 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        return Err(error);
                    }
                    if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "Terminal output closed",
                        ));
                    }
                    if fd.revents & libc::POLLOUT != 0 {
                        break;
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, os::unix::net::UnixStream, sync::mpsc, time::Duration};

    #[test]
    fn backpressured_output_waits_and_preserves_partial_writes() {
        let (sender, mut receiver) = UnixStream::pair().unwrap();
        sender.set_nonblocking(true).unwrap();
        let (blocked, ready) = mpsc::channel();
        struct Output {
            stream: UnixStream,
            blocked: Option<mpsc::Sender<()>>,
        }
        impl AsRawFd for Output {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                self.stream.as_raw_fd()
            }
        }
        impl Write for Output {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                // Exercise short writes independently of the socket buffer size.
                let result = self.stream.write(&bytes[..bytes.len().min(997)]);
                if result
                    .as_ref()
                    .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock)
                    && let Some(blocked) = self.blocked.take()
                {
                    blocked.send(()).unwrap();
                }
                result
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let reader = std::thread::spawn(move || {
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
            let mut bytes = vec![];
            receiver.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let mut output = Output {
            stream: sender,
            blocked: Some(blocked),
        };
        let bytes: Vec<_> = (0..2_000_000).map(|n| (n % 251) as u8).collect();
        write_ready(&mut output, &bytes).unwrap();
        assert!(output.blocked.is_none(), "must exercise WouldBlock");
        drop(output);
        assert_eq!(reader.join().unwrap(), bytes);
    }

    #[test]
    fn a_closed_output_returns_an_error() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        sender.set_nonblocking(true).unwrap();
        drop(receiver);
        assert!(write_ready(&mut sender, b"image").is_err());
    }
}
