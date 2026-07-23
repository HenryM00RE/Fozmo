use serde::{Serialize, de::DeserializeOwned};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(super) const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub(super) async fn read_json_frame<R, T>(reader: &mut R) -> io::Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid Apple Music IPC frame length: {length}"),
        ));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(super) async fn write_json_frame<W, T>(writer: &mut W, value: &T) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if payload.is_empty() || payload.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Apple Music IPC message exceeds the 1 MiB limit",
        ));
    }
    writer.write_u32(payload.len() as u32).await?;
    writer.write_all(&payload).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Fixture {
        value: String,
    }

    #[tokio::test]
    async fn frame_round_trip_uses_big_endian_length_prefix() {
        let (mut client, mut server) = tokio::io::duplex(128);
        let send = Fixture {
            value: "ready".to_string(),
        };
        let writer = tokio::spawn(async move { write_json_frame(&mut client, &send).await });
        let received: Fixture = read_json_frame(&mut server).await.unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(
            received,
            Fixture {
                value: "ready".to_string()
            }
        );
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_before_allocating_payload() {
        let (mut client, mut server) = tokio::io::duplex(16);
        let writer = tokio::spawn(async move {
            client
                .write_u32((MAX_MESSAGE_BYTES + 1) as u32)
                .await
                .unwrap();
        });
        let result = read_json_frame::<_, Fixture>(&mut server).await;
        writer.await.unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
