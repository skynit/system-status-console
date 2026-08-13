use bytes::{BufMut, Bytes, BytesMut};
use serde::{Serialize, de::DeserializeOwned};
use std::{future::Future, io, time::Duration};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::{Instant, timeout_at},
};
use tokio_util::codec::{LengthDelimitedCodec, LengthDelimitedCodecError};

pub const MAX_FRAME_PAYLOAD_BYTES: usize = 65_536;
pub const FRAME_IDLE_TIMEOUT: Duration = Duration::from_secs(2);
const FRAME_PREFIX_BYTES: usize = 4;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct WireBudget {
    max_frames: usize,
    max_wire_bytes: usize,
    observed_frames: usize,
    observed_wire_bytes: usize,
}

impl WireBudget {
    pub const fn new(max_frames: usize, max_wire_bytes: usize) -> Self {
        Self {
            max_frames,
            max_wire_bytes,
            observed_frames: 0,
            observed_wire_bytes: 0,
        }
    }

    pub fn observe_payload_len(&mut self, payload_len: usize) -> Result<(), FrameError> {
        let frames = self
            .observed_frames
            .checked_add(1)
            .ok_or(FrameError::BudgetOverflow)?;
        if frames > self.max_frames {
            return Err(FrameError::FrameLimitExceeded);
        }
        let frame_wire_bytes = FRAME_PREFIX_BYTES
            .checked_add(payload_len)
            .ok_or(FrameError::BudgetOverflow)?;
        let wire_bytes = self
            .observed_wire_bytes
            .checked_add(frame_wire_bytes)
            .ok_or(FrameError::BudgetOverflow)?;
        if wire_bytes > self.max_wire_bytes {
            return Err(FrameError::WireBytesExceeded);
        }
        self.observed_frames = frames;
        self.observed_wire_bytes = wire_bytes;
        Ok(())
    }

    pub const fn observed_frames(&self) -> usize {
        self.observed_frames
    }

    pub const fn observed_wire_bytes(&self) -> usize {
        self.observed_wire_bytes
    }
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("frame I/O was idle for too long")]
    IdleTimeout,
    #[error("request deadline expired")]
    DeadlineExceeded,
    #[error("peer closed before a complete frame")]
    UnexpectedEof,
    #[error("frame payload is empty")]
    Empty,
    #[error("frame payload exceeds {MAX_FRAME_PAYLOAD_BYTES} bytes")]
    Oversize,
    #[error("response frame count exceeds the configured limit")]
    FrameLimitExceeded,
    #[error("response wire bytes exceed the configured limit")]
    WireBytesExceeded,
    #[error("frame budget arithmetic overflowed")]
    BudgetOverflow,
    #[error("frame payload is not valid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

pub fn configured_codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .length_field_length(FRAME_PREFIX_BYTES)
        .big_endian()
        .max_frame_length(MAX_FRAME_PAYLOAD_BYTES)
        .new_codec()
}

pub async fn read_frame<R>(
    reader: &mut R,
    deadline: Instant,
    budget: &mut WireBudget,
) -> Result<Bytes, FrameError>
where
    R: AsyncRead + Unpin,
{
    read_frame_with_idle_timeout(reader, deadline, FRAME_IDLE_TIMEOUT, budget).await
}

pub async fn read_frame_with_idle_timeout<R>(
    reader: &mut R,
    deadline: Instant,
    idle_timeout: Duration,
    budget: &mut WireBudget,
) -> Result<Bytes, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; FRAME_PREFIX_BYTES];
    timed_io(deadline, idle_timeout, reader.read_exact(&mut header))
        .await
        .map_err(map_eof)?;

    let length = u32::from_be_bytes(header) as usize;
    if length == 0 {
        return Err(FrameError::Empty);
    }
    budget.observe_payload_len(length)?;
    if length > MAX_FRAME_PAYLOAD_BYTES {
        return Err(FrameError::Oversize);
    }

    let mut payload = BytesMut::with_capacity(length);
    payload.resize(length, 0);
    timed_io(deadline, idle_timeout, reader.read_exact(&mut payload))
        .await
        .map_err(map_eof)?;
    Ok(payload.freeze())
}

pub async fn write_frame<W>(
    writer: &mut W,
    payload: &[u8],
    deadline: Instant,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    write_frame_with_idle_timeout(writer, payload, deadline, FRAME_IDLE_TIMEOUT).await
}

pub async fn write_frame_with_idle_timeout<W>(
    writer: &mut W,
    payload: &[u8],
    deadline: Instant,
    idle_timeout: Duration,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    if payload.len() > MAX_FRAME_PAYLOAD_BYTES {
        return Err(FrameError::Oversize);
    }

    let mut frame = BytesMut::with_capacity(FRAME_PREFIX_BYTES + payload.len());
    frame.put_u32(payload.len() as u32);
    frame.extend_from_slice(payload);
    timed_io(deadline, idle_timeout, writer.write_all(&frame)).await?;
    timed_io(deadline, idle_timeout, writer.flush()).await?;
    Ok(())
}

pub async fn write_json<W, T>(
    writer: &mut W,
    value: &T,
    deadline: Instant,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(value)?;
    write_frame(writer, &payload, deadline).await
}

pub async fn read_json<R, T>(
    reader: &mut R,
    deadline: Instant,
    budget: &mut WireBudget,
) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    read_json_with_idle_timeout(reader, deadline, FRAME_IDLE_TIMEOUT, budget).await
}

pub(crate) async fn read_json_with_idle_timeout<R, T>(
    reader: &mut R,
    deadline: Instant,
    idle_timeout: Duration,
    budget: &mut WireBudget,
) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let payload = read_frame_with_idle_timeout(reader, deadline, idle_timeout, budget).await?;
    Ok(serde_json::from_slice(&payload)?)
}

async fn timed_io<F, T>(
    absolute_deadline: Instant,
    idle_timeout: Duration,
    operation: F,
) -> Result<T, FrameError>
where
    F: Future<Output = io::Result<T>>,
{
    let now = Instant::now();
    if now >= absolute_deadline {
        return Err(FrameError::DeadlineExceeded);
    }
    let idle_deadline = now + idle_timeout;
    let operation_deadline = absolute_deadline.min(idle_deadline);
    match timeout_at(operation_deadline, operation).await {
        Ok(result) => result.map_err(FrameError::Io),
        Err(_) if operation_deadline == absolute_deadline => Err(FrameError::DeadlineExceeded),
        Err(_) => Err(FrameError::IdleTimeout),
    }
}

fn map_eof(error: FrameError) -> FrameError {
    match error {
        FrameError::Io(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            FrameError::UnexpectedEof
        }
        other => other,
    }
}

impl From<LengthDelimitedCodecError> for FrameError {
    fn from(error: LengthDelimitedCodecError) -> Self {
        Self::Io(io::Error::new(io::ErrorKind::InvalidData, error))
    }
}
