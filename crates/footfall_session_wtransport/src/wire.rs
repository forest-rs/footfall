// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Request and response framing over WebTransport streams.

use super::*;

pub(crate) const DEFAULT_PATH: &str = "/footfall/session";
pub(crate) const DEFAULT_LOCAL_PORT: u16 = 43_091;
pub(crate) const DISCOVER_MAGIC: &[u8; 8] = b"FFALLD01";
pub(crate) const SNAPSHOT_MAGIC: &[u8; 8] = b"FFALLS01";
pub(crate) const SUBSCRIBE_MAGIC: &[u8; 8] = b"FFALLU01";
/// Shortest interval a server waits between subscription snapshots, so a
/// client cannot ask it to spin.
pub(crate) const MIN_SUBSCRIPTION_INTERVAL: Duration = Duration::from_millis(10);
pub(crate) const CODEC_VERSION: ProtocolVersion = ProtocolVersion::new(1, 0);
pub(crate) const RESPONSE_DISCOVERY: u8 = 0;
pub(crate) const RESPONSE_SNAPSHOT: u8 = 1;
pub(crate) const RESPONSE_ERROR: u8 = 2;

pub(crate) async fn write_u16(send: &mut SendStream, value: u16) -> io::Result<()> {
    send.write_all(&value.to_le_bytes())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

pub(crate) async fn read_u16(recv: &mut RecvStream) -> io::Result<u16> {
    let mut bytes = [0_u8; 2];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(u16::from_le_bytes(bytes))
}

pub(crate) async fn write_u32(send: &mut SendStream, value: u32) -> io::Result<()> {
    send.write_all(&value.to_le_bytes())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

pub(crate) async fn write_u64(send: &mut SendStream, value: u64) -> io::Result<()> {
    send.write_all(&value.to_le_bytes())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

pub(crate) async fn read_u32(recv: &mut RecvStream) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(u32::from_le_bytes(bytes))
}

pub(crate) async fn read_u64(recv: &mut RecvStream) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(u64::from_le_bytes(bytes))
}

pub(crate) async fn finish_response_stream(send: &mut SendStream) -> io::Result<()> {
    map_finish_error(send.finish().await)
}

pub(crate) async fn finish_request_stream(send: &mut SendStream) -> io::Result<()> {
    map_finish_error(send.finish().await)
}

pub(crate) async fn write_u8_stream(send: &mut SendStream, value: u8) -> io::Result<()> {
    send.write_all(&[value])
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

pub(crate) async fn read_u8_stream(recv: &mut RecvStream) -> io::Result<u8> {
    let mut bytes = [0_u8; 1];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(bytes[0])
}

pub(crate) async fn write_stream_string(send: &mut SendStream, value: &str) -> io::Result<()> {
    let bytes = value.as_bytes();
    validate_wire_len(bytes.len(), MAX_SESSION_STRING_BYTES, "session string")?;
    write_u32(
        send,
        u32::try_from(bytes.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "string exceeds u32 length")
        })?,
    )
    .await?;
    send.write_all(bytes)
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

pub(crate) async fn read_stream_string(recv: &mut RecvStream) -> io::Result<String> {
    let len = usize::try_from(read_u32(recv).await?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid string length"))?;
    validate_wire_len(len, MAX_SESSION_STRING_BYTES, "session string")?;
    let mut bytes = vec![0_u8; len];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(crate) async fn encode_session_announcement_stream(
    send: &mut SendStream,
    session: &SessionAnnouncement,
) -> io::Result<()> {
    write_stream_string(send, &session.id).await?;
    write_stream_string(send, &session.process_name).await?;
    match &session.session_name {
        Some(session_name) => {
            write_u8_stream(send, 1).await?;
            write_stream_string(send, session_name).await?;
        }
        None => write_u8_stream(send, 0).await?,
    }
    Ok(())
}

pub(crate) async fn decode_session_announcement_stream(
    recv: &mut RecvStream,
) -> io::Result<SessionAnnouncement> {
    let id = read_stream_string(recv).await?;
    let process_name = read_stream_string(recv).await?;
    let session_name = match read_u8_stream(recv).await? {
        0 => None,
        1 => Some(read_stream_string(recv).await?),
        tag => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid session-name tag: {tag}"),
            ));
        }
    };
    Ok(SessionAnnouncement {
        id,
        process_name,
        session_name,
    })
}

/// Reads an `option<cursor>`.
pub(crate) async fn read_cursor(recv: &mut RecvStream) -> io::Result<Option<EventCursor>> {
    match read_u8_stream(recv).await? {
        0 => Ok(None),
        1 => Ok(Some(EventCursor::new(
            read_u64(recv).await?,
            read_u64(recv).await?,
        ))),
        tag => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid cursor presence tag: {tag}"),
        )),
    }
}

/// Appends an `option<cursor>`.
pub(crate) fn encode_cursor(bytes: &mut Vec<u8>, cursor: Option<EventCursor>) {
    match cursor {
        Some(cursor) => {
            encode_u8(bytes, 1);
            encode_u64(bytes, cursor.instance_id());
            encode_u64(bytes, cursor.sequence());
        }
        None => encode_u8(bytes, 0),
    }
}

/// Wire tag for an error code. Codes this side does not know are sent as
/// `Failed`, which every peer understands.
pub(crate) const fn encode_error_code(code: SessionErrorCode) -> u8 {
    match code {
        SessionErrorCode::InvalidRequest => 0,
        SessionErrorCode::NotFound => 1,
        SessionErrorCode::Unsupported => 2,
        SessionErrorCode::Incompatible => 4,
        _ => 3,
    }
}

/// Error code for a wire tag; tags from a newer peer read as `Failed`.
pub(crate) const fn decode_error_code(tag: u8) -> SessionErrorCode {
    match tag {
        0 => SessionErrorCode::InvalidRequest,
        1 => SessionErrorCode::NotFound,
        2 => SessionErrorCode::Unsupported,
        4 => SessionErrorCode::Incompatible,
        _ => SessionErrorCode::Failed,
    }
}

pub(crate) fn map_finish_error(result: Result<(), StreamWriteError>) -> io::Result<()> {
    match result {
        Ok(()) | Err(StreamWriteError::Stopped(_)) => Ok(()),
        Err(error) => Err(io::Error::other(error.to_string())),
    }
}

pub(crate) fn encode_u8(bytes: &mut Vec<u8>, value: u8) {
    bytes.push(value);
}

pub(crate) fn encode_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn encode_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn encode_string(bytes: &mut Vec<u8>, value: &str) -> io::Result<()> {
    validate_wire_len(value.len(), MAX_SESSION_STRING_BYTES, "session string")?;
    let len = u32::try_from(value.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "string exceeds u32 length"))?;
    encode_u32(bytes, len);
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

pub(crate) fn validate_wire_len(len: usize, max: usize, what: &str) -> io::Result<()> {
    if len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{what} is {len} bytes, more than the limit of {max}"),
        ));
    }
    Ok(())
}

pub(crate) fn validate_wire_count(count: u64, max: u64, what: &str) -> io::Result<()> {
    if count > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{count} {what}, more than the limit of {max}"),
        ));
    }
    Ok(())
}
