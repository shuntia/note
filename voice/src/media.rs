use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use std::borrow::Cow;
use std::time::Duration;
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;

use crate::matrix::Matrix;

const IN_RATE: i32 = 16_000;
const IN_FRAME: usize = 160;
const OUT_RATE: u32 = 48_000;
const OUT_FRAME: usize = 480;
const CONNECT_WAIT: Duration = Duration::from_secs(10);

#[async_trait::async_trait]
pub trait MediaIo: Send + Sync {
    /// The next 10 ms of the user's audio at 16 kHz mono; None once the user's track is gone for good.
    /// One caller at a time.
    async fn recv(&self) -> Option<Vec<f32>>;
    /// Queues one 10 ms frame at 48 kHz mono; waits while the outbound buffer is full.
    async fn send(&self, frame: &[i16; OUT_FRAME]) -> Result<()>;
    /// Drops audio queued in the outbound buffer (barge-in).
    fn clear(&self);
    /// Resolves when the participant whose audio is read leaves, or the room disconnects.
    async fn left(&self);
    async fn leave(&self);
}

/// Joins a room's call for a session.
#[async_trait::async_trait]
pub trait MediaJoin: Send + Sync {
    async fn join(&self, matrix: &Matrix, livekit_service_url: &str, room_id: &str, mxid: &str) -> Result<Box<dyn MediaIo>>;
}

/// Element X's `LiveKit` identity is `<mxid>:<device>`.
pub struct LiveKitJoin {
    pub wait: Duration,
}

#[async_trait::async_trait]
impl MediaJoin for LiveKitJoin {
    async fn join(&self, matrix: &Matrix, livekit_service_url: &str, room_id: &str, mxid: &str) -> Result<Box<dyn MediaIo>> {
        let (url, jwt) = matrix.livekit_jwt(livekit_service_url, room_id).await?;
        Ok(Box::new(LiveKitMedia::join(&url, &jwt, &format!("{mxid}:"), self.wait).await?))
    }
}

pub struct LiveKitMedia {
    room: Room,
    source: NativeAudioSource,
    frames: Mutex<mpsc::Receiver<Vec<f32>>>,
    gone: watch::Receiver<bool>,
    follower: JoinHandle<()>,
}

impl LiveKitMedia {
    /// Connects, publishes the bot's track, and waits up to `wait` for `user_identity_prefix`'s audio track.
    /// Needs the process's rustls provider installed, since the build links two.
    pub async fn join(url: &str, jwt: &str, user_identity_prefix: &str, wait: Duration) -> Result<LiveKitMedia> {
        let (room, mut events) = tokio::time::timeout(CONNECT_WAIT, Room::connect(url, jwt, RoomOptions::default()))
            .await
            .map_err(|_| anyhow!("joining LiveKit timed out after {CONNECT_WAIT:?}"))?
            .context("joining LiveKit")?;
        let options = AudioSourceOptions { echo_cancellation: false, noise_suppression: false, auto_gain_control: false };
        let source = NativeAudioSource::new(options, OUT_RATE, 1, 100);
        let track = LocalAudioTrack::create_audio_track("note", RtcAudioSource::Native(source.clone()));
        if let Err(e) = room.local_participant().publish_track(LocalTrack::Audio(track), TrackPublishOptions::default()).await {
            close(&room).await;
            return Err(anyhow::Error::from(e).context("publishing the bot's track"));
        }
        let prefix = user_identity_prefix.to_string();
        let (user, user_track) = match tokio::time::timeout(wait, user_audio(&mut events, &prefix)).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                close(&room).await;
                bail!("the room closed before {prefix}* published audio");
            }
            Err(_) => {
                close(&room).await;
                bail!("no audio track from {prefix}* within {wait:?}");
            }
        };
        let (frames_tx, frames) = mpsc::channel(50);
        let (gone_tx, gone) = watch::channel(false);
        let follower = tokio::spawn(follow(events, prefix, user, user_track, frames_tx, gone_tx));
        Ok(LiveKitMedia { room, source, frames: Mutex::new(frames), gone, follower })
    }
}

impl Drop for LiveKitMedia {
    fn drop(&mut self) {
        self.follower.abort();
    }
}

#[async_trait::async_trait]
impl MediaIo for LiveKitMedia {
    async fn recv(&self) -> Option<Vec<f32>> {
        self.frames.lock().await.recv().await
    }

    async fn send(&self, frame: &[i16; OUT_FRAME]) -> Result<()> {
        let frame = AudioFrame {
            data: Cow::Borrowed(&frame[..]),
            sample_rate: OUT_RATE,
            num_channels: 1,
            samples_per_channel: OUT_FRAME as u32,
        };
        self.source.capture_frame(&frame).await.context("queueing outbound audio")
    }

    fn clear(&self) {
        self.source.clear_buffer();
    }

    async fn left(&self) {
        let _ = self.gone.clone().wait_for(|gone| *gone).await;
    }

    async fn leave(&self) {
        close(&self.room).await;
    }
}

async fn close(room: &Room) {
    if let Err(e) = room.close().await {
        eprintln!("voice: leaving LiveKit failed: {e}");
    }
}

fn is_user(participant: &RemoteParticipant, prefix: &str) -> bool {
    participant.identity().as_str().starts_with(prefix)
}

async fn user_audio(
    events: &mut mpsc::UnboundedReceiver<RoomEvent>,
    prefix: &str,
) -> Option<(ParticipantIdentity, RemoteAudioTrack)> {
    while let Some(event) = events.recv().await {
        match event {
            RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(track), participant, .. }
                if is_user(&participant, prefix) =>
            {
                return Some((participant.identity(), track))
            }
            RoomEvent::Disconnected { .. } => return None,
            _ => {}
        }
    }
    None
}

/// Feeds the user's current audio track into `frames` until the participant it comes from or the room is gone;
/// a resubscribed track replaces the old one.
async fn follow(
    mut events: mpsc::UnboundedReceiver<RoomEvent>,
    prefix: String,
    mut user: ParticipantIdentity,
    first: RemoteAudioTrack,
    frames: mpsc::Sender<Vec<f32>>,
    gone: watch::Sender<bool>,
) {
    let mut reading = Some((first.sid(), tokio::spawn(read(first, frames.clone()))));
    while let Some(event) = events.recv().await {
        match event {
            RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(track), participant, .. }
                if is_user(&participant, &prefix) =>
            {
                if let Some((_, task)) = reading.take() {
                    task.abort();
                }
                user = participant.identity();
                reading = Some((track.sid(), tokio::spawn(read(track, frames.clone()))));
            }
            RoomEvent::TrackUnsubscribed { track: RemoteTrack::Audio(track), .. }
                if reading.as_ref().is_some_and(|(sid, _)| *sid == track.sid()) =>
            {
                if let Some((_, task)) = reading.take() {
                    task.abort();
                }
            }
            RoomEvent::ParticipantDisconnected(participant) if participant.identity() == user => break,
            RoomEvent::Disconnected { .. } => break,
            _ => {}
        }
    }
    if let Some((_, task)) = reading {
        task.abort();
    }
    let _ = gone.send(true);
}

async fn read(track: RemoteAudioTrack, frames: mpsc::Sender<Vec<f32>>) {
    let mut stream = NativeAudioStream::new(track.rtc_track(), IN_RATE, 1);
    let mut chunks = Rechunker::default();
    while let Some(frame) = stream.next().await {
        for chunk in chunks.push(&frame.data) {
            if frames.send(chunk).await.is_err() {
                return;
            }
        }
    }
}

#[derive(Default)]
struct Rechunker {
    pending: Vec<f32>,
}

impl Rechunker {
    /// Returns every whole 10 ms frame now available; a remainder waits for the next push.
    fn push(&mut self, samples: &[i16]) -> Vec<Vec<f32>> {
        self.pending.extend(samples.iter().map(|&s| f32::from(s) / 32768.0));
        let whole = self.pending.len() / IN_FRAME * IN_FRAME;
        self.pending.drain(..whole).collect::<Vec<_>>().chunks(IN_FRAME).map(<[f32]>::to_vec).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rechunking_turns_each_480_sample_frame_into_three_in_order() {
        let mut r = Rechunker::default();
        let a: Vec<i16> = (0..480).map(|i| i as i16).collect();
        let b: Vec<i16> = (480..960).map(|i| i as i16).collect();
        let mut out = r.push(&a);
        out.extend(r.push(&b));
        assert_eq!(out.len(), 6);
        assert!(out.iter().all(|f| f.len() == 160));
        let flat: Vec<f32> = out.concat();
        let want: Vec<f32> = (0..960).map(|i| i as f32 / 32768.0).collect();
        assert_eq!(flat, want);
    }

    #[tokio::test]
    #[ignore = "needs NOTE_VOICE_LIVE=<bot json> and the live homeserver"]
    async fn the_bot_joins_and_publishes() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let creds: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(std::env::var("NOTE_VOICE_LIVE").unwrap()).unwrap()).unwrap();
        let m = Matrix::connect("https://matrix.example.org", creds["access_token"].as_str().unwrap()).await.unwrap();
        let (url, jwt) = m.livekit_jwt("https://matrix-rtc.example.org", "!note-voice-selftest").await.unwrap();
        let err = LiveKitMedia::join(&url, &jwt, "@nobody:", Duration::from_secs(2)).await.err().unwrap();
        assert!(err.to_string().contains("no audio track"), "{err}");
    }
}
