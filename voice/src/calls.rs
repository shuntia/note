use crate::matrix::{Matrix, RoomEvent};
use note_voice_proto::Outcome;
use std::time::Duration;
use tokio::sync::{broadcast, watch};

/// How long the bot's call membership outlives the ring, so a slow answer
/// still finds a live call.
const MEMBER_GRACE_SECS: u64 = 30;

pub struct Ring<'a> {
    pub matrix: &'a Matrix,
    pub room_id: &'a str,
    pub mxid: &'a str,
    pub livekit_url: &'a str,
    pub ring_secs: u32,
}

/// Rings once and returns how it ended; the membership is cleared on every
/// path. `on_ringing` fires once the ring event is out.
pub async fn ring_once(
    r: Ring<'_>,
    mut events: broadcast::Receiver<RoomEvent>,
    mut hang_up: watch::Receiver<bool>,
    on_ringing: impl FnOnce(),
) -> Outcome {
    let outcome = ring_inner(&r, &mut events, &mut hang_up, on_ringing).await;
    if let Err(e) = r.matrix.clear_member(r.room_id).await {
        eprintln!("voice: clearing the call membership in {} failed: {e:#}", r.room_id);
    }
    outcome
}

async fn ring_inner(
    r: &Ring<'_>,
    events: &mut broadcast::Receiver<RoomEvent>,
    hang_up: &mut watch::Receiver<bool>,
    on_ringing: impl FnOnce(),
) -> Outcome {
    let failed = |e: anyhow::Error| Outcome::Failed { reason: format!("{e:#}") };
    let expires = (u64::from(r.ring_secs) + MEMBER_GRACE_SECS) * 1000;
    let member = match r.matrix.put_member(r.room_id, expires, r.livekit_url).await {
        Ok(id) => id,
        Err(e) => return failed(e),
    };
    let notification = match r.matrix.ring(r.room_id, r.mxid, &member, u64::from(r.ring_secs) * 1000).await {
        Ok(id) => id,
        Err(e) => return failed(e),
    };
    on_ringing();
    let deadline = tokio::time::sleep(Duration::from_secs(u64::from(r.ring_secs)));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => return Outcome::Missed,
            changed = hang_up.changed() => {
                if changed.is_err() || *hang_up.borrow() {
                    return Outcome::Failed { reason: "hung up by Note".into() };
                }
            }
            ev = events.recv() => match ev {
                Ok(RoomEvent::CallMember { room, user, active: true }) if room == r.room_id && user == r.mxid => {
                    return Outcome::Answered;
                }
                Ok(RoomEvent::Declined { room, notification: n }) if room == r.room_id && n == notification => {
                    return Outcome::Declined;
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Outcome::Failed { reason: "the sync loop stopped".into() };
                }
            }
        }
    }
}
