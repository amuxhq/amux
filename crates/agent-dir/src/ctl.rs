//! Frames on ctl.sock and pty.sock: a varint length prefix and a protobuf
//! message ([`CtlFrame`], `PtyFrame`), the same framing the journal uses for
//! steps.

use std::io;

use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use wire::CtlFrame;

/// A frame larger than this is a broken peer, not a frame to wait for.
pub const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;

pub async fn write_frame<W: AsyncWrite + Unpin>(out: &mut W, frame: &CtlFrame) -> io::Result<()> {
    write_message(out, frame).await
}

/// The next frame, or None at a clean end of stream.
pub async fn read_frame<R: AsyncRead + Unpin>(input: &mut R) -> io::Result<Option<CtlFrame>> {
    read_message(input).await
}

pub async fn write_message<W: AsyncWrite + Unpin, M: Message>(
    out: &mut W,
    message: &M,
) -> io::Result<()> {
    out.write_all(&message.encode_length_delimited_to_vec())
        .await?;
    out.flush().await
}

/// The next message, or None at a clean end of stream.
pub async fn read_message<R: AsyncRead + Unpin, M: Message + Default>(
    input: &mut R,
) -> io::Result<Option<M>> {
    let mut len: u64 = 0;
    for index in 0..10 {
        let byte = match input.read_u8().await {
            Ok(byte) => byte,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof && index == 0 => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        len |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            if len > MAX_FRAME_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("a {len} byte frame"),
                ));
            }
            let mut body = vec![0; len as usize];
            input.read_exact(&mut body).await?;
            return M::decode(body.as_slice())
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "a frame length longer than ten bytes",
    ))
}

#[cfg(test)]
mod tests {
    use wire::{AgentHello, ctl_frame};

    use super::*;

    #[tokio::test]
    async fn frames_round_trip_and_a_clean_end_is_none() {
        let hello = CtlFrame {
            of: Some(ctl_frame::Of::Hello(AgentHello {
                agent_id: vec![7; 16],
                agent_version: "v".into(),
            })),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &hello).await.unwrap();
        write_frame(&mut bytes, &hello).await.unwrap();
        let mut input = bytes.as_slice();
        assert_eq!(read_frame(&mut input).await.unwrap(), Some(hello.clone()));
        assert_eq!(read_frame(&mut input).await.unwrap(), Some(hello));
        assert_eq!(read_frame(&mut input).await.unwrap(), None);
    }
}
