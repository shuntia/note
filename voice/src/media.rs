use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use note_voice_proto::LiveState;
use std::borrow::Cow;
use std::time::Duration;
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;

use crate::matrix::Matrix;

const IN_RATE: i32 = 16_000;
const IN_FRAME: usize = 160;
const OUT_RATE: u32 = 48_000;
pub const OUT_FRAME: usize = 480;
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
    /// Resolves when the user leaves, the room disconnects, or the user never joins.
    async fn left(&self) -> Gone;
    async fn leave(&self);
    /// What the call is doing, for a caller who can see it; called on each change.
    fn show(&self, _state: LiveState) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gone {
    Left,
    Failed(String),
}

/// Joins a room's call for a session; a user not in it within `wait` ends the media.
#[async_trait::async_trait]
pub trait MediaJoin: Send + Sync {
    async fn join(
        &self,
        matrix: &Matrix,
        livekit_service_url: &str,
        room_id: &str,
        mxid: &str,
        wait: Duration,
    ) -> Result<Box<dyn MediaIo>>;
}

/// Element X's `LiveKit` identity is `<mxid>:<device>`.
pub struct LiveKitJoin;

#[async_trait::async_trait]
impl MediaJoin for LiveKitJoin {
    async fn join(
        &self,
        matrix: &Matrix,
        livekit_service_url: &str,
        room_id: &str,
        mxid: &str,
        wait: Duration,
    ) -> Result<Box<dyn MediaIo>> {
        let (url, jwt) = matrix.livekit_jwt(livekit_service_url, room_id).await?;
        let who = Who { user_prefix: format!("{mxid}:"), bot_prefix: format!("{}:", matrix.user_id) };
        Ok(Box::new(LiveKitMedia::join(&url, &jwt, &who, wait).await?))
    }
}

/// `LiveKit` identity prefixes of the user and of the bot's own devices.
pub struct Who {
    pub user_prefix: String,
    pub bot_prefix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Match {
    User,
    Other,
    Bot,
}

/// The bot's prefix wins, so a user prefix that happens to share it never makes the bot its own user.
fn classify(identity: &str, who: &Who) -> Match {
    if identity.starts_with(&who.bot_prefix) {
        Match::Bot
    } else if identity.starts_with(&who.user_prefix) {
        Match::User
    } else {
        Match::Other
    }
}

pub struct LiveKitMedia {
    room: Room,
    source: NativeAudioSource,
    frames: Mutex<mpsc::Receiver<Vec<f32>>>,
    gone: watch::Receiver<Option<Gone>>,
    follower: JoinHandle<()>,
}

impl LiveKitMedia {
    /// Connects and publishes the bot's track. The user's audio is picked up whenever it appears; a user
    /// who has not joined within `wait` ends the media with `Gone::Failed`.
    /// Needs the process's rustls provider installed, since the build links two.
    pub async fn join(url: &str, jwt: &str, who: &Who, wait: Duration) -> Result<LiveKitMedia> {
        let (room, events) = tokio::time::timeout(CONNECT_WAIT, Room::connect(url, jwt, RoomOptions::default()))
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
        eprintln!(
            "voice: in LiveKit room {} ({}) as {}",
            room.name(),
            room.sid().await,
            room.local_participant().identity().as_str()
        );
        let present: Vec<RemoteParticipant> = room.remote_participants().into_values().collect();
        let (frames_tx, frames) = mpsc::channel(50);
        let (gone_tx, gone) = watch::channel(None);
        let follower = Follower {
            who: Who { user_prefix: who.user_prefix.clone(), bot_prefix: who.bot_prefix.clone() },
            present: Vec::new(),
            user: None,
            reading: None,
            frames: frames_tx,
        };
        let follower = tokio::spawn(follower.run(events, present, wait, gone_tx));
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

    async fn left(&self) -> Gone {
        let mut gone = self.gone.clone();
        let got = gone.wait_for(Option::is_some).await.map(|g| g.clone());
        got.ok().flatten().unwrap_or(Gone::Left)
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

fn log_publication(what: &str, participant: &RemoteParticipant, publication: &RemoteTrackPublication) {
    eprintln!(
        "voice: LiveKit {what}: {} {:?} {:?} {} muted={} subscribed={} encryption={:?}",
        participant.identity().as_str(),
        publication.kind(),
        publication.source(),
        publication.sid(),
        publication.is_muted(),
        publication.is_subscribed(),
        publication.encryption_type(),
    );
}

/// Finds the user among the room's participants and keeps `frames` fed from the user's current audio track.
struct Follower {
    who: Who,
    present: Vec<RemoteParticipant>,
    user: Option<ParticipantIdentity>,
    reading: Option<(TrackSid, JoinHandle<()>)>,
    frames: mpsc::Sender<Vec<f32>>,
}

impl Follower {
    /// Runs until the user leaves or the room is gone; with no user by `wait`, the first participant that is
    /// not the bot is taken as the user, and with none the media ends as `the caller never joined`.
    async fn run(
        mut self,
        mut events: mpsc::UnboundedReceiver<RoomEvent>,
        present: Vec<RemoteParticipant>,
        wait: Duration,
        gone: watch::Sender<Option<Gone>>,
    ) {
        for participant in present {
            eprintln!("voice: LiveKit participant {} ({}) was already in the room", participant.identity().as_str(), participant.sid());
            for publication in participant.track_publications().values() {
                log_publication("existing track", &participant, publication);
            }
            self.arrived(participant);
        }
        let deadline = tokio::time::sleep(wait);
        tokio::pin!(deadline);
        let mut waiting = true;
        let end = loop {
            let event = tokio::select! {
                () = &mut deadline, if waiting => {
                    waiting = false;
                    if self.user.is_none() && !self.fall_back(wait) {
                        break Gone::Failed("the caller never joined".into());
                    }
                    continue;
                }
                event = events.recv() => event,
            };
            match event {
                None | Some(RoomEvent::Disconnected { .. }) => {
                    eprintln!("voice: the LiveKit room disconnected");
                    break Gone::Left;
                }
                Some(RoomEvent::ParticipantConnected(participant)) => {
                    eprintln!("voice: LiveKit participant {} ({}) connected", participant.identity().as_str(), participant.sid());
                    self.arrived(participant);
                }
                Some(RoomEvent::ParticipantDisconnected(participant)) => {
                    eprintln!("voice: LiveKit participant {} disconnected", participant.identity().as_str());
                    self.present.retain(|p| p.identity() != participant.identity());
                    if self.user.as_ref() == Some(&participant.identity()) {
                        break Gone::Left;
                    }
                }
                Some(RoomEvent::TrackPublished { publication, participant }) => {
                    log_publication("track published", &participant, &publication);
                    if self.is_user(&participant) {
                        subscribe(&publication);
                    }
                }
                Some(RoomEvent::TrackSubscribed { track, publication, participant }) => {
                    log_publication("track subscribed", &participant, &publication);
                    if let (RemoteTrack::Audio(track), true) = (track, self.is_user(&participant)) {
                        self.read(track);
                    }
                }
                Some(RoomEvent::TrackSubscriptionFailed { participant, error, track_sid }) => {
                    eprintln!("voice: LiveKit subscription to {} {track_sid} failed: {error}", participant.identity().as_str());
                }
                Some(RoomEvent::TrackUnsubscribed { publication, participant, .. }) => {
                    log_publication("track unsubscribed", &participant, &publication);
                    if self.reading.as_ref().is_some_and(|(sid, _)| *sid == publication.sid()) {
                        if let Some((_, task)) = self.reading.take() {
                            task.abort();
                        }
                    }
                }
                Some(RoomEvent::TrackMuted { participant, publication }) => {
                    eprintln!("voice: LiveKit {} muted {:?} {}", participant.identity().as_str(), publication.kind(), publication.sid());
                }
                Some(RoomEvent::TrackUnmuted { participant, publication }) => {
                    eprintln!("voice: LiveKit {} unmuted {:?} {}", participant.identity().as_str(), publication.kind(), publication.sid());
                    if let Some(remote) = self.present.iter().find(|p| p.identity() == participant.identity()).cloned() {
                        if self.is_user(&remote) {
                            self.adopt(&remote);
                        }
                    }
                }
                Some(_) => {}
            }
        };
        if let Some((_, task)) = self.reading.take() {
            task.abort();
        }
        let _ = gone.send(Some(end));
    }

    fn is_user(&self, participant: &RemoteParticipant) -> bool {
        self.user.as_ref() == Some(&participant.identity())
    }

    fn arrived(&mut self, participant: RemoteParticipant) {
        if self.user.is_none() && classify(participant.identity().as_str(), &self.who) == Match::User {
            eprintln!("voice: {} is the user (identity prefix {})", participant.identity().as_str(), self.who.user_prefix);
            self.user = Some(participant.identity());
            self.adopt(&participant);
        }
        self.present.push(participant);
    }

    /// Takes the first participant present that is not the bot; false when there is none.
    fn fall_back(&mut self, wait: Duration) -> bool {
        let Some(other) =
            self.present.iter().find(|p| classify(p.identity().as_str(), &self.who) == Match::Other).cloned()
        else {
            return false;
        };
        eprintln!(
            "voice: no {}* participant within {wait:?}; {} is the user (first other participant)",
            self.who.user_prefix,
            other.identity().as_str()
        );
        self.user = Some(other.identity());
        self.adopt(&other);
        true
    }

    /// Reads the user's subscribed audio, and subscribes to audio not yet subscribed.
    fn adopt(&mut self, participant: &RemoteParticipant) {
        for publication in participant.track_publications().values() {
            if publication.kind() != TrackKind::Audio {
                continue;
            }
            match publication.track() {
                Some(RemoteTrack::Audio(track)) => {
                    if self.reading.as_ref().is_none_or(|(sid, _)| *sid != track.sid()) {
                        self.read(track);
                    }
                }
                _ => subscribe(publication),
            }
        }
    }

    fn read(&mut self, track: RemoteAudioTrack) {
        if let Some((_, task)) = self.reading.take() {
            task.abort();
        }
        eprintln!("voice: reading the user's audio track {}", track.sid());
        self.reading = Some((track.sid(), tokio::spawn(read(track, self.frames.clone()))));
    }
}

fn subscribe(publication: &RemoteTrackPublication) {
    if publication.kind() == TrackKind::Audio && !publication.is_subscribed() {
        publication.set_subscribed(true);
    }
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
pub(crate) struct Rechunker {
    pending: Vec<f32>,
}

impl Rechunker {
    /// Returns every whole 10 ms frame now available; a remainder waits for the next push.
    pub(crate) fn push(&mut self, samples: &[i16]) -> Vec<Vec<f32>> {
        self.pending.extend(samples.iter().map(|&s| f32::from(s) / 32768.0));
        let whole = self.pending.len() / IN_FRAME * IN_FRAME;
        self.pending.drain(..whole).collect::<Vec<_>>().chunks(IN_FRAME).map(<[f32]>::to_vec).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_is_matched_by_prefix_and_the_bot_never_is() {
        let who = Who { user_prefix: "@shuntia:matrix.example.org:".into(), bot_prefix: "@note:matrix.example.org:".into() };
        assert_eq!(classify("@shuntia:matrix.example.org:ALICEPHONE", &who), Match::User);
        assert_eq!(classify("@note:matrix.example.org:BOTDEV", &who), Match::Bot);
        assert_eq!(classify("a1b2c3", &who), Match::Other);
        assert_eq!(classify("@shuntia:matrix.example.org.evil:X", &who), Match::Other);
        let same = Who { user_prefix: "@note:t:".into(), bot_prefix: "@note:t:".into() };
        assert_eq!(classify("@note:t:DEV", &same), Match::Bot);
    }

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
        let who = Who { user_prefix: "@nobody:".into(), bot_prefix: format!("{}:", m.user_id) };
        let media = LiveKitMedia::join(&url, &jwt, &who, Duration::from_secs(2)).await.unwrap();
        assert_eq!(media.left().await, Gone::Failed("the caller never joined".into()));
        media.leave().await;
    }
}
