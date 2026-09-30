use anyhow::{Result, bail};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// Length-prefixed JSON prevents an unterminated client request from growing memory indefinitely.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

pub async fn write<W: AsyncWrite + Unpin, T: Serialize>(writer: &mut W, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        bail!("IPC frame exceeds 16 MiB");
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
pub async fn read<R: AsyncRead + Unpin, T: DeserializeOwned>(reader: &mut R) -> Result<T> {
    let len = reader.read_u32().await? as usize;
    if len > MAX_FRAME {
        bail!("IPC frame exceeds 16 MiB");
    }
    let mut data = vec![0; len];
    reader.read_exact(&mut data).await?;
    Ok(serde_json::from_slice(&data)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fragmented_frames_and_size_limit() {
        let (mut a, mut b) = tokio::io::duplex(8);
        let send = tokio::spawn(async move {
            write(&mut a, &vec!["a"; 50]).await.unwrap();
        });
        let received: Vec<String> = read(&mut b).await.unwrap();
        assert_eq!(received.len(), 50);
        send.await.unwrap();
        let (mut a, mut b) = tokio::io::duplex(8);
        a.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        assert!(
            read::<_, serde_json::Value>(&mut b)
                .await
                .unwrap_err()
                .to_string()
                .contains("exceeds")
        );
    }
}
