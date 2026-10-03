use crate::frame::Frame;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME: usize = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame of {0} bytes exceeds the {MAX_FRAME}-byte limit")]
    TooLarge(usize),
    #[error("malformed frame: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> Result<(), CodecError> {
    let bytes = serde_json::to_vec(frame)?;
    if bytes.len() > MAX_FRAME {
        return Err(CodecError::TooLarge(bytes.len()));
    }
    w.write_u32(bytes.len() as u32).await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

/// `None` is a clean end of stream between frames.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Frame>, CodecError> {
    let len = match r.read_u32().await {
        Ok(n) => n as usize,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if len > MAX_FRAME {
        return Err(CodecError::TooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(Some(serde_json::from_slice(&buf)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::*;
    use tokio::io::AsyncWriteExt;

    fn every_frame() -> Vec<Frame> {
        vec![
            Frame::Hello { proto: PROTO_VERSION, role: Role::Voice, instance: "i-1".into() },
            Frame::Ping { n: 7 },
            Frame::Pong { n: 7 },
            Frame::Request { id: 1, body: Request::OpenDm { link_id: 3, mxid: "@a:b".into() } },
            Frame::Request { id: 2, body: Request::DmJoined { link_id: 3, room_id: "!r:b".into() } },
            Frame::Response { id: 1, result: Ok(Reply::Dm { room_id: "!r:b".into() }) },
            Frame::Response { id: 2, result: Ok(Reply::Done) },
            Frame::Request {
                id: 4,
                body: Request::IncomingCall { room_id: "!r:b".into(), mxid: "@a:b".into(), key: "$ev".into() },
            },
            Frame::Response { id: 4, result: Ok(Reply::Call { call_id: "c-2".into() }) },
            Frame::Response {
                id: 3,
                result: Err(Refusal::new(RefusalCode::Timeout, "no answer in time")),
            },
            Frame::Call {
                call_id: "c-1".into(),
                dir: Dir::ToVoice,
                seq: 1,
                body: CallBody::Start {
                    user_id: 1,
                    room_id: "!r:b".into(),
                    mxid: "@a:b".into(),
                    title: "Check-in".into(),
                    ring_secs: 30,
                    ring_by_ms: 1_790_000_000_000,
                    voice: VoiceProfile::default(),
                    direction: Direction::Inbound,
                },
            },
            Frame::Call { call_id: "c-1".into(), dir: Dir::ToVoice, seq: 2, body: CallBody::HangUp },
            Frame::Call { call_id: "c-1".into(), dir: Dir::ToNote, seq: 1, body: CallBody::Ringing },
            Frame::Call {
                call_id: "c-1".into(),
                dir: Dir::ToNote,
                seq: 2,
                body: CallBody::Outcome { outcome: Outcome::Failed { reason: "late".into() } },
            },
            Frame::Call { call_id: "c-1".into(), dir: Dir::ToNote, seq: 3, body: CallBody::Ended },
            Frame::Ack { call_id: "c-1".into(), dir: Dir::ToNote, seq: 3 },
            Frame::Resume { call_id: "c-1".into(), dir: Dir::ToVoice, after: 1 },
        ]
    }

    #[tokio::test]
    async fn every_frame_round_trips() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        for f in every_frame() {
            write_frame(&mut a, &f).await.unwrap();
            assert_eq!(read_frame(&mut b).await.unwrap(), Some(f));
        }
    }

    #[tokio::test]
    async fn clean_eof_between_frames_is_none() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a);
        assert!(read_frame(&mut b).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_oversized_length_is_refused_before_reading_the_body() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        match read_frame(&mut b).await {
            Err(CodecError::TooLarge(n)) => assert_eq!(n, MAX_FRAME + 1),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_oversized_frame_is_not_written() {
        let (mut a, _b) = tokio::io::duplex(64);
        let huge = Frame::Hello { proto: 1, role: Role::Note, instance: "x".repeat(MAX_FRAME) };
        assert!(matches!(write_frame(&mut a, &huge).await, Err(CodecError::TooLarge(_))));
    }

    #[tokio::test]
    async fn malformed_json_is_an_error() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_u32(3).await.unwrap();
        a.write_all(b"{x}").await.unwrap();
        assert!(matches!(read_frame(&mut b).await, Err(CodecError::Json(_))));
    }

    #[test]
    fn call_ids_are_restricted_to_safe_file_names() {
        assert!(valid_call_id("0b9c7a4e-1f1e-4c1b-9a8e-3d2f1c0b9a8e"));
        assert!(!valid_call_id(""));
        assert!(!valid_call_id("../etc"));
        assert!(!valid_call_id(&"a".repeat(65)));
    }
}
