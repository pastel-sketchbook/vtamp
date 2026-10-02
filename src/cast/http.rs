//! Icecast-style HTTP delivery of the cast for players and browsers. One GET on
//! the token path answers with headers and then the raw Ogg Opus stream until
//! the listener disconnects. The server speaks plain HTTP on the address it was
//! given, loopback unless asked otherwise; TLS and any further authentication
//! belong to a reverse proxy or a private network in front of it.
use super::Hub;
use crate::platform::{self, Paths};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, broadcast},
};

const MAX_LISTENERS: usize = 64;
const MAX_HEAD: usize = 8 * 1024;

#[derive(Serialize, Deserialize)]
struct Settings {
    token: String,
}

/// The share token, created once and kept in `cast.json` in the data directory.
pub fn token(paths: &Paths) -> Result<String> {
    let path = paths.data.join("cast.json");
    match std::fs::read(&path) {
        Ok(bytes) => {
            let settings: Settings = serde_json::from_slice(&bytes)
                .context("Invalid cast.json; delete it to issue a new token")?;
            if settings.token.is_empty()
                || !settings.token.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                bail!("cast.json holds an invalid token; delete it to issue a new one");
            }
            Ok(settings.token)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let token = format!("{:032x}", rand::RngExt::random::<u128>(&mut rand::rng()));
            platform::private_dir(&paths.data)?;
            platform::atomic_json(
                &path,
                &Settings {
                    token: token.clone(),
                },
            )?;
            Ok(token)
        }
        Err(error) => Err(error).context("Cannot read cast.json"),
    }
}

pub struct HttpCast {
    pub url: String,
    path: String,
    listener: TcpListener,
}

impl HttpCast {
    /// Bind now, so the reported URL carries the actual port.
    pub async fn bind(address: SocketAddr, token: &str) -> Result<Self> {
        let listener = TcpListener::bind(address)
            .await
            .with_context(|| format!("Cannot listen for HTTP cast listeners on {address}"))?;
        let local = listener.local_addr()?;
        let path = format!("/cast/{token}");
        Ok(Self {
            url: format!("http://{local}{path}"),
            path,
            listener,
        })
    }

    pub async fn serve(self, hub: Arc<Hub>) {
        let permits = Arc::new(Semaphore::new(MAX_LISTENERS));
        let path = Arc::new(self.path);
        loop {
            let (stream, peer) = match self.listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    tracing::warn!("HTTP cast accept failed: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                continue;
            };
            let hub = hub.clone();
            let path = path.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(error) = connection(stream, &path, &hub).await {
                    tracing::debug!(%peer, "HTTP listener left: {error:#}");
                }
            });
        }
    }
}

async fn connection(mut stream: TcpStream, path: &str, hub: &Hub) -> Result<()> {
    let head = tokio::time::timeout(Duration::from_secs(5), read_head(&mut stream)).await??;
    let request = String::from_utf8_lossy(&head);
    let mut parts = request.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("").split('?').next().unwrap_or("");
    if !matches!(method, "GET" | "HEAD") {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            "Allow: GET, HEAD\r\n",
        )
        .await;
    }
    if target != path {
        return respond(&mut stream, "404 Not Found", "").await;
    }
    // Subscribe before answering so no page between the two is missed.
    let (headers, mut chunks) = hub.subscribe();
    let response = "HTTP/1.1 200 OK\r\nContent-Type: audio/ogg\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    write(&mut stream, response.as_bytes()).await?;
    if method == "HEAD" {
        return Ok(());
    }
    if let Some(headers) = headers {
        write(&mut stream, &headers).await?;
    }
    loop {
        let mut byte = [0u8; 1];
        tokio::select! {
            _ = stream.read(&mut byte) => break,
            chunk = chunks.recv() => match chunk {
                Ok(chunk) => write(&mut stream, &chunk).await?,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if let Some(headers) = hub.headers() {
                        write(&mut stream, &headers).await?;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    Ok(())
}

async fn read_head(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut head = Vec::with_capacity(1024);
    let mut buffer = [0u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            bail!("closed before the request completed");
        }
        head.extend_from_slice(&buffer[..read]);
        if let Some(end) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            head.truncate(end);
            return Ok(head);
        }
        if head.len() > MAX_HEAD {
            bail!("request head too large");
        }
    }
}

async fn respond(stream: &mut TcpStream, status: &str, extra: &str) -> Result<()> {
    let response =
        format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n{extra}\r\n");
    write(stream, response.as_bytes()).await
}

async fn write(stream: &mut TcpStream, bytes: &[u8]) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), stream.write_all(bytes))
        .await
        .context("HTTP listener is not reading")??;
    Ok(())
}
