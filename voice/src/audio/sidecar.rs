use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::engines::VoiceInfo;
use super::tts::{is_cjk, resample, Audio, Chunker, Next, SpeechBackend, SpeechStream, TextInput};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SidecarConfig {
    pub id: String,
    pub url: String,
}

const PROBE_EVERY: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
struct Info {
    label: String,
    input: InfoInput,
    sample_rate: u32,
    voices: Vec<InfoVoice>,
    #[serde(default)]
    slow: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum InfoInput {
    Incremental,
    Chunks,
}

#[derive(Deserialize)]
struct InfoVoice {
    id: String,
    label: String,
    /// The languages the voice speaks; absent for any.
    #[serde(default)]
    languages: Vec<String>,
}

/// The configured sidecars, of which only those answering `/info` are offered.
pub struct Sidecars {
    configs: Vec<SidecarConfig>,
    live: Mutex<Vec<Arc<Sidecar>>>,
    up: Mutex<HashMap<String, bool>>,
    http: reqwest::Client,
}

impl Sidecars {
    pub fn new(configs: Vec<SidecarConfig>) -> Self {
        let http = reqwest::Client::builder().timeout(PROBE_TIMEOUT).no_proxy().build().expect("an HTTP client");
        Sidecars { configs, live: Mutex::default(), up: Mutex::default(), http }
    }

    pub fn live(&self) -> Vec<Arc<dyn SpeechBackend>> {
        self.live.lock().expect("sidecars lock").iter().map(|s| s.clone() as Arc<dyn SpeechBackend>).collect()
    }

    /// Asks each sidecar for its `/info`; one that does not answer is left out until it does.
    pub async fn probe(&self) {
        let found = futures_util::future::join_all(self.configs.iter().map(|c| probe(&self.http, c))).await;
        let mut live = Vec::new();
        let mut up = self.up.lock().expect("sidecars lock");
        for (config, got) in self.configs.iter().zip(found) {
            let was = up.insert(config.id.clone(), got.is_ok());
            match got {
                Ok(sidecar) => {
                    if was != Some(true) {
                        eprintln!("voice: the {} speech sidecar is up", config.id);
                    }
                    live.push(Arc::new(sidecar));
                }
                Err(e) if was != Some(false) => eprintln!("voice: the {} speech sidecar is down: {e:#}", config.id),
                Err(_) => {}
            }
        }
        *self.live.lock().expect("sidecars lock") = live;
    }

    /// Probes now and every 30 s, so a sidecar started later is offered; `probed` runs after each probe.
    pub fn watch(self: &Arc<Self>, probed: impl Fn() + Send + Sync + 'static) {
        if self.configs.is_empty() {
            return;
        }
        let sidecars = self.clone();
        tokio::spawn(async move {
            loop {
                sidecars.probe().await;
                probed();
                tokio::time::sleep(PROBE_EVERY).await;
            }
        });
    }
}

async fn probe(http: &reqwest::Client, config: &SidecarConfig) -> anyhow::Result<Sidecar> {
    let base = config.url.trim_end_matches('/');
    let info: Info = http.get(format!("{base}/info")).send().await?.error_for_status()?.json().await?;
    if info.sample_rate == 0 {
        anyhow::bail!("a sample rate of 0");
    }
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_owned()
    };
    Ok(Sidecar {
        id: config.id.clone(),
        label: info.label,
        input: match info.input {
            InfoInput::Incremental => TextInput::Incremental,
            InfoInput::Chunks => TextInput::Chunks,
        },
        rate: info.sample_rate,
        slow: info.slow,
        voices: info
            .voices
            .into_iter()
            .map(|v| VoiceInfo { id: v.id, label: v.label, languages: v.languages })
            .collect(),
        stream_url: format!("{ws}/stream"),
        rt: Handle::current(),
    })
}

pub struct Sidecar {
    id: String,
    label: String,
    input: TextInput,
    rate: u32,
    slow: bool,
    voices: Vec<VoiceInfo>,
    stream_url: String,
    rt: Handle,
}

impl SpeechBackend for Sidecar {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn slow(&self) -> bool {
        self.slow
    }

    fn input(&self) -> TextInput {
        self.input
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        self.voices.clone()
    }

    fn open(&self, voice: &str) -> anyhow::Result<Box<dyn SpeechStream>> {
        let (out, out_rx) = mpsc::unbounded_channel();
        let (events_tx, events) = std::sync::mpsc::channel();
        self.rt.spawn(converse(self.stream_url.clone(), voice.to_owned(), out_rx, events_tx));
        Ok(Box::new(SidecarStream {
            out,
            events,
            chunker: (self.input == TextInput::Chunks).then(Chunker::default),
            in_flight: None,
            last_push: Instant::now(),
            marks: Marks::default(),
            pcm: Vec::new(),
            rate: self.rate,
            spaced: true,
            end_sent: false,
            ended: false,
            cancelled: false,
            heard_any: false,
            owed_since: Instant::now(),
        }))
    }
}

enum Msg {
    Text(String),
    End,
    Cancel,
}

impl Msg {
    fn json(&self) -> String {
        match self {
            Msg::Text(text) => serde_json::json!({ "type": "text", "text": text }),
            Msg::End => serde_json::json!({ "type": "end" }),
            Msg::Cancel => serde_json::json!({ "type": "cancel" }),
        }
        .to_string()
    }
}

enum Event {
    Pcm(Vec<u8>),
    Mark(u32),
    Done,
    Failed(String),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum FromSidecar {
    Mark { chars: u32 },
    Done,
    Error { message: String },
    #[serde(other)]
    Other,
}

async fn converse(
    url: String,
    voice: String,
    mut out: mpsc::UnboundedReceiver<Msg>,
    events: std::sync::mpsc::Sender<Event>,
) {
    if let Err(e) = talk(&url, &voice, &mut out, &events).await {
        let _ = events.send(Event::Failed(format!("{e:#}")));
    }
}

async fn talk(
    url: &str,
    voice: &str,
    out: &mut mpsc::UnboundedReceiver<Msg>,
    events: &std::sync::mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let (ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url))
        .await
        .map_err(|_| anyhow!("connecting to {url} timed out"))??;
    let (mut sink, mut source) = ws.split();
    sink.send(Message::text(serde_json::json!({ "type": "open", "voice": voice }).to_string())).await?;
    loop {
        tokio::select! {
            msg = out.recv() => {
                let Some(msg) = msg else {
                    let _ = sink.close().await;
                    return Ok(());
                };
                if matches!(msg, Msg::Cancel) {
                    let _ = sink.send(Message::text(msg.json())).await;
                    let _ = sink.close().await;
                    return Ok(());
                }
                sink.send(Message::text(msg.json())).await?;
            }
            got = source.next() => match got {
                Some(Ok(Message::Binary(pcm))) => {
                    let _ = events.send(Event::Pcm(pcm.to_vec()));
                }
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<FromSidecar>(&text)? {
                    FromSidecar::Mark { chars } => {
                        let _ = events.send(Event::Mark(chars));
                    }
                    FromSidecar::Done => {
                        let _ = events.send(Event::Done);
                        let _ = sink.close().await;
                        return Ok(());
                    }
                    FromSidecar::Error { message } => anyhow::bail!("the sidecar failed: {message}"),
                    FromSidecar::Other => {}
                },
                Some(Ok(Message::Close(_))) | None => anyhow::bail!("the sidecar closed the stream"),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
            }
        }
    }
}

/// Maps the sidecar's marks, which count the text it was sent, onto the pushed characters.
#[derive(Default)]
struct Marks {
    /// Per message sent: its length, and the pushed characters it carries.
    sent: VecDeque<(u32, u32)>,
    into: u32,
    credited: u32,
}

impl Marks {
    fn sent(&mut self, sent: u32, counted: u32) {
        self.sent.push_back((sent, counted));
    }

    fn is_empty(&self) -> bool {
        self.sent.is_empty()
    }

    fn mark(&mut self, mut n: u32) -> u32 {
        let mut out = 0;
        while let Some(&(sent, counted)) = self.sent.front() {
            if n == 0 && sent > self.into {
                break;
            }
            let take = n.min(sent - self.into);
            self.into += take;
            n -= take;
            let target = if sent == 0 {
                counted
            } else {
                (u64::from(counted) * u64::from(self.into) / u64::from(sent)) as u32
            };
            out += target - self.credited;
            self.credited = target;
            if self.into < sent {
                break;
            }
            self.sent.pop_front();
            self.into = 0;
            self.credited = 0;
        }
        out
    }

    fn rest(&mut self) -> u32 {
        let rest = self.sent.drain(..).map(|(_, counted)| counted).sum::<u32>() - self.credited;
        self.into = 0;
        self.credited = 0;
        rest
    }
}

/// The longest a sidecar may keep silent while it owes audio: before its first, and between later events.
const FIRST_AUDIO: Duration = Duration::from_secs(3);
const STALLED: Duration = Duration::from_secs(3);

struct SidecarStream {
    out: mpsc::UnboundedSender<Msg>,
    events: std::sync::mpsc::Receiver<Event>,
    /// Set for a sidecar that takes whole chunks.
    chunker: Option<Chunker>,
    /// The pushed characters of the chunk sent and not yet marked.
    in_flight: Option<u32>,
    last_push: Instant,
    marks: Marks,
    /// s16le received and not yet passed on.
    pcm: Vec<u8>,
    rate: u32,
    /// The text sent so far ends in whitespace or CJK script, or none was sent.
    spaced: bool,
    end_sent: bool,
    ended: bool,
    cancelled: bool,
    heard_any: bool,
    /// Since when the sidecar has owed audio without sending anything.
    owed_since: Instant,
}

impl SidecarStream {
    fn send(&self, msg: Msg) -> anyhow::Result<()> {
        self.out.send(msg).map_err(|_| anyhow!("the sidecar stream has stopped"))
    }

    /// The sidecar has text, or an end, it has not answered yet.
    fn owes(&self) -> bool {
        !self.marks.is_empty() || self.in_flight.is_some() || self.end_sent
    }

    fn send_owed(&mut self, msg: Msg) -> anyhow::Result<()> {
        if !self.owes() {
            self.owed_since = Instant::now();
        }
        self.send(msg)
    }

    fn take(&mut self, chars: u32) -> Audio {
        let whole = self.pcm.len() / 2 * 2;
        let samples: Vec<i16> = self.pcm.drain(..whole).as_slice().as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        Audio { pcm: resample(&samples, self.rate), chars }
    }

    /// For a chunks sidecar, sends the next chunk once the last one is marked.
    fn feed(&mut self, ahead: Duration) -> anyhow::Result<Option<Audio>> {
        let Some(chunker) = self.chunker.as_mut() else { return Ok(None) };
        let chunk = if self.in_flight.is_none() { chunker.next(ahead, self.last_push.elapsed()) } else { None };
        let done = chunker.done();
        if let Some(chunk) = chunk {
            let text = chunk.text.trim();
            if text.is_empty() {
                return Ok(Some(Audio { pcm: Vec::new(), chars: chunk.chars }));
            }
            let msg = Msg::Text(text.to_owned());
            self.send_owed(msg)?;
            self.in_flight = Some(chunk.chars);
        }
        if done && self.in_flight.is_none() && !self.end_sent {
            self.send_owed(Msg::End)?;
            self.end_sent = true;
        }
        Ok(None)
    }

    /// A sidecar silent past its deadline is given up on: the stream ends.
    fn check_silence(&mut self) -> anyhow::Result<()> {
        let limit = if self.heard_any { STALLED } else { FIRST_AUDIO };
        if self.owes() && self.owed_since.elapsed() > limit {
            self.ended = true;
            anyhow::bail!("the sidecar sent nothing for {limit:?}");
        }
        Ok(())
    }
}

impl SpeechStream for SidecarStream {
    fn push(&mut self, text: &str) -> anyhow::Result<()> {
        if self.cancelled || text.is_empty() {
            return Ok(());
        }
        if let Some(chunker) = self.chunker.as_mut() {
            chunker.push(text);
            self.last_push = Instant::now();
            return Ok(());
        }
        let unspaced = |c: char| c.is_whitespace() || is_cjk(c);
        let sent = if self.spaced || text.starts_with(unspaced) { text.to_owned() } else { format!(" {text}") };
        self.spaced = text.ends_with(unspaced);
        let msg_len = sent.chars().count() as u32;
        self.send_owed(Msg::Text(sent))?;
        self.marks.sent(msg_len, text.chars().count() as u32);
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        if let Some(chunker) = self.chunker.as_mut() {
            chunker.finish();
            return Ok(());
        }
        if self.end_sent {
            return Ok(());
        }
        self.send_owed(Msg::End)?;
        self.end_sent = true;
        Ok(())
    }

    /// Audio is passed on as it comes, its characters once the sidecar marks them. A sidecar that owes
    /// audio and stays silent past its deadline fails the stream.
    fn next(&mut self, ahead: Duration) -> anyhow::Result<Next> {
        if self.cancelled || self.ended {
            return Ok(Next::Done);
        }
        if let Some(silent) = self.feed(ahead)? {
            return Ok(Next::Audio(silent));
        }
        loop {
            let event = self.events.try_recv();
            if event.is_ok() {
                self.heard_any = true;
                self.owed_since = Instant::now();
            }
            match event {
                Ok(Event::Pcm(bytes)) => self.pcm.extend(bytes),
                Ok(Event::Mark(n)) => {
                    let chars = if self.chunker.is_some() { self.in_flight.take().unwrap_or(0) } else { self.marks.mark(n) };
                    if self.pcm.len() >= 2 || chars > 0 {
                        return Ok(Next::Audio(self.take(chars)));
                    }
                }
                Ok(Event::Done) => {
                    self.ended = true;
                    let chars = self.in_flight.take().unwrap_or(0) + self.marks.rest();
                    if self.pcm.len() >= 2 || chars > 0 {
                        return Ok(Next::Audio(self.take(chars)));
                    }
                    return Ok(Next::Done);
                }
                Ok(Event::Failed(message)) => {
                    self.ended = true;
                    anyhow::bail!(message);
                }
                Err(TryRecvError::Empty) if self.pcm.len() >= 2 => return Ok(Next::Audio(self.take(0))),
                Err(TryRecvError::Empty) => {
                    self.check_silence()?;
                    return Ok(Next::Pending);
                }
                Err(TryRecvError::Disconnected) => {
                    self.ended = true;
                    anyhow::bail!("the sidecar stream stopped");
                }
            }
        }
    }

    fn cancel(&mut self) {
        if !self.cancelled {
            self.cancelled = true;
            let _ = self.send(Msg::Cancel);
        }
    }

    fn alive(&self) -> bool {
        !self.ended && !self.cancelled
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::routing::get;

    use super::super::playout::Playout;
    use super::super::speech::SpeechQueue;
    use super::super::tts::{ChunkedBackend, Renderer, Speaker};
    use super::*;

    /// A sidecar at 24 kHz that speaks each text as one sample per character, valued 100 × its length,
    /// in two binary frames, then marks it; a text of "fail." errors. `mute` answers nothing,
    /// `undercount` marks one character short, `render_ms` delays each text's audio and `mark_after_ms`
    /// its mark.
    struct Fake {
        input: &'static str,
        up: AtomicBool,
        mute: AtomicBool,
        undercount: AtomicBool,
        mark_after_ms: AtomicU64,
        render_ms: AtomicU64,
        seen: Mutex<Vec<serde_json::Value>>,
    }

    impl Fake {
        fn seen(&self, kind: &str) -> Vec<serde_json::Value> {
            self.seen.lock().unwrap().iter().filter(|v| v["type"] == kind).cloned().collect()
        }
    }

    async fn info(State(fake): State<Arc<Fake>>) -> axum::response::Response {
        if !fake.up.load(Ordering::SeqCst) {
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        axum::Json(serde_json::json!({
            "id": "fake", "label": "Natural", "input": fake.input, "sample_rate": 24000, "slow": true,
            "voices": [{ "id": "alba", "label": "Alba" }, { "id": "cosette", "label": "Cosette", "languages": ["ja"] }],
        }))
        .into_response()
    }

    async fn stream(ws: WebSocketUpgrade, State(fake): State<Arc<Fake>>) -> axum::response::Response {
        ws.on_upgrade(move |socket| serve(socket, fake))
    }

    async fn serve(mut socket: WebSocket, fake: Arc<Fake>) {
        while let Some(Ok(msg)) = socket.recv().await {
            let WsMessage::Text(text) = msg else { continue };
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            fake.seen.lock().unwrap().push(v.clone());
            let reply = |v: serde_json::Value| WsMessage::Text(v.to_string().into());
            if fake.mute.load(Ordering::SeqCst) {
                continue;
            }
            match v["type"].as_str().unwrap() {
                "text" => {
                    let text = v["text"].as_str().unwrap();
                    if text.trim() == "fail." {
                        let _ = socket.send(reply(serde_json::json!({ "type": "error", "message": "no" }))).await;
                        return;
                    }
                    let n = text.chars().count();
                    let pcm: Vec<u8> = (0..n).flat_map(|_| (n as i16 * 100).to_le_bytes()).collect();
                    tokio::time::sleep(Duration::from_millis(fake.render_ms.load(Ordering::SeqCst))).await;
                    let (a, b) = pcm.split_at(pcm.len() / 2 + 1);
                    for msg in [WsMessage::Binary(a.to_vec().into()), WsMessage::Binary(b.to_vec().into())] {
                        if socket.send(msg).await.is_err() {
                            return;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(fake.mark_after_ms.load(Ordering::SeqCst))).await;
                    let marked = n - usize::from(fake.undercount.load(Ordering::SeqCst));
                    if socket.send(reply(serde_json::json!({ "type": "mark", "chars": marked }))).await.is_err() {
                        return;
                    }
                }
                "end" => {
                    let _ = socket.send(reply(serde_json::json!({ "type": "done" }))).await;
                    return;
                }
                "cancel" => return,
                _ => {}
            }
        }
    }

    async fn fake(input: &'static str) -> (Arc<Fake>, SidecarConfig) {
        let fake = Arc::new(Fake {
            input,
            up: AtomicBool::new(true),
            mute: AtomicBool::new(false),
            undercount: AtomicBool::new(false),
            mark_after_ms: AtomicU64::new(0),
            render_ms: AtomicU64::new(0),
            seen: Mutex::default(),
        });
        let app = axum::Router::new().route("/info", get(info)).route("/stream", get(stream)).with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (fake, SidecarConfig { id: "fake".into(), url })
    }

    async fn live(config: SidecarConfig) -> Arc<dyn SpeechBackend> {
        let sidecars = Sidecars::new(vec![config]);
        sidecars.probe().await;
        sidecars.live().pop().expect("the sidecar is up")
    }

    async fn all(stream: &mut dyn SpeechStream) -> Vec<Audio> {
        let mut pieces = Vec::new();
        for _ in 0..2000 {
            match stream.next(Duration::ZERO).unwrap() {
                Next::Audio(audio) => pieces.push(audio),
                Next::Done => return pieces,
                Next::Pending => tokio::time::sleep(Duration::from_millis(1)).await,
            }
        }
        panic!("the stream never ended: {}", pieces.len());
    }

    fn marked(pieces: &[Audio]) -> Vec<u32> {
        pieces.iter().map(|a| a.chars).filter(|&c| c > 0).collect()
    }

    fn pcm(pieces: &[Audio]) -> Vec<i16> {
        pieces.iter().flat_map(|a| a.pcm.iter().copied()).collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn marks_map_to_the_pushed_chars_and_audio_resamples_to_48k() {
        let (fake, config) = fake("incremental").await;
        let mut stream = live(config).await.open("alba").unwrap();
        stream.push("Hello there.").unwrap();
        stream.push("Bye.").unwrap();
        stream.finish().unwrap();
        let pieces = all(&mut *stream).await;
        assert_eq!(marked(&pieces), [12, 4], "the joining space is spoken but not counted");
        let mut expected = vec![1200; 24];
        expected.extend([500; 10]);
        assert_eq!(pcm(&pieces), expected);
        assert_eq!(fake.seen("open")[0]["voice"], "alba");
        assert_eq!(fake.seen("text")[1]["text"], " Bye.");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_chunks_sidecar_is_sent_whole_chunks_one_at_a_time() {
        let (fake, config) = fake("chunks").await;
        fake.undercount.store(true, Ordering::SeqCst);
        let mut stream = live(config).await.open("").unwrap();
        stream.push("Sure, I can do that.").unwrap();
        stream.push("It is on Friday.").unwrap();
        stream.finish().unwrap();
        let pieces = all(&mut *stream).await;
        assert_eq!(marked(&pieces), [20, 16], "each mark completes its chunk, whatever it counts");
        let texts: Vec<_> = fake.seen("text").iter().map(|v| v["text"].as_str().unwrap().to_owned()).collect();
        assert_eq!(texts, ["Sure, I can do that.", "It is on Friday."]);
        assert_eq!(fake.seen("end").len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_chunks_sidecar_two_seconds_per_chunk_is_waited_for() {
        let (fake, config) = fake("chunks").await;
        fake.render_ms.store(2000, Ordering::SeqCst);
        let mut stream = live(config).await.open("").unwrap();
        stream.push("Sure, I can do that.").unwrap();
        stream.push("It is on Friday.").unwrap();
        stream.finish().unwrap();
        let mut pieces = Vec::new();
        for _ in 0..10_000 {
            match stream.next(Duration::ZERO).expect("a slow sidecar is not a failed one") {
                Next::Audio(audio) => pieces.push(audio),
                Next::Done => break,
                Next::Pending => tokio::time::sleep(Duration::from_millis(1)).await,
            }
        }
        assert_eq!(marked(&pieces), [20, 16]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn audio_is_passed_on_before_its_mark() {
        let (fake, config) = fake("incremental").await;
        fake.mark_after_ms.store(300, Ordering::SeqCst);
        let mut stream = live(config).await.open("alba").unwrap();
        stream.push("Hello there.").unwrap();
        stream.finish().unwrap();
        let first = loop {
            match stream.next(Duration::ZERO).unwrap() {
                Next::Audio(audio) => break audio,
                _ => tokio::time::sleep(Duration::from_millis(1)).await,
            }
        };
        assert!(!first.pcm.is_empty() && first.chars == 0, "{first:?}");
        let rest = all(&mut *stream).await;
        assert_eq!(marked(&rest), [12]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sidecar_that_connects_then_hangs_hands_the_reply_to_kokoro() {
        let (fake, config) = fake("incremental").await;
        fake.mute.store(true, Ordering::SeqCst);
        let recorder = Arc::new(Recorder::default());
        let kokoro: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("kokoro", "Kokoro", recorder.clone(), Vec::new()));
        let mut q = SpeechQueue::new(Speaker::new(live(config).await, "alba"), kokoro);
        let mut p = Playout::default();
        q.speak(1, 0, "Hello there.".into());
        q.speak_done(1);
        q.play(1);
        let start = Instant::now();
        while start.elapsed() < FIRST_AUDIO * 2 {
            q.pump(&mut p);
            while p.next_frame().is_some() {}
            if q.take_finished(&mut p) == [1] {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(*recorder.0.lock().unwrap(), ["Hello there."]);
        assert!(start.elapsed() > FIRST_AUDIO, "{:?}", start.elapsed());
        assert_eq!(p.heard(1), 12);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancel_tells_the_sidecar_and_ends_the_stream() {
        let (fake, config) = fake("incremental").await;
        let mut stream = live(config).await.open("alba").unwrap();
        stream.push("Hello there.").unwrap();
        while !matches!(stream.next(Duration::ZERO).unwrap(), Next::Audio(_)) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        stream.cancel();
        assert!(matches!(stream.next(Duration::ZERO).unwrap(), Next::Done));
        for _ in 0..1000 {
            if !fake.seen("cancel").is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert!(matches!(stream.next(Duration::ZERO).unwrap(), Next::Done), "the close after a cancel is no error");
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("the sidecar never heard the cancel");
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Renderer for Recorder {
        fn render(&self, text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            self.0.lock().unwrap().push(text.into());
            Ok(vec![7; text.len()])
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_error_hands_the_rest_of_the_reply_to_kokoro() {
        let (_fake, config) = fake("incremental").await;
        let recorder = Arc::new(Recorder::default());
        let kokoro: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("kokoro", "Kokoro", recorder.clone(), Vec::new()));
        let mut q = SpeechQueue::new(Speaker::new(live(config).await, "alba"), kokoro);
        let mut p = Playout::default();
        for (idx, text) in (0..).zip(["Hello there.", "fail.", "Bye."]) {
            q.speak(1, idx, text.into());
        }
        q.speak_done(1);
        q.play(1);
        let mut first = Vec::new();
        for _ in 0..2000 {
            q.pump(&mut p);
            while let Some(frame) = p.next_frame() {
                first.push(frame[0]);
            }
            if q.take_finished(&mut p) == [1] {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(first.first(), Some(&1200), "the sidecar spoke first");
        assert_eq!(*recorder.0.lock().unwrap(), ["fail. Bye."]);
        assert_eq!(p.heard(1), 12 + 5 + 4, "every pushed character is accounted for");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sidecar_down_at_startup_is_left_out_until_it_answers() {
        let (fake, config) = fake("incremental").await;
        fake.up.store(false, Ordering::SeqCst);
        let unreachable = SidecarConfig { id: "gone".into(), url: "http://127.0.0.1:9".into() };
        let sidecars = Sidecars::new(vec![unreachable, config]);
        sidecars.probe().await;
        assert!(sidecars.live().is_empty());
        fake.up.store(true, Ordering::SeqCst);
        sidecars.probe().await;
        let live = sidecars.live();
        assert_eq!(live.iter().map(|s| s.id().to_owned()).collect::<Vec<_>>(), ["fake"]);
        assert_eq!((live[0].label(), live[0].slow()), ("Natural", true));
        assert_eq!(live[0].voices().iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), ["alba", "cosette"]);
        assert_eq!(live[0].voices().iter().map(|v| v.languages.clone()).collect::<Vec<_>>(), [vec![], vec!["ja".to_string()]]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn japanese_pushes_reach_an_incremental_sidecar_without_a_joining_space() {
        let (fake, config) = fake("incremental").await;
        let mut stream = live(config).await.open("cosette").unwrap();
        stream.push("はい、").unwrap();
        stream.push("明日にしたよ。").unwrap();
        stream.push("Okay.").unwrap();
        stream.finish().unwrap();
        let pieces = all(&mut *stream).await;
        assert_eq!(marked(&pieces), [3, 7, 5]);
        let texts: Vec<_> = fake.seen("text").iter().map(|v| v["text"].as_str().unwrap().to_owned()).collect();
        assert_eq!(texts, ["はい、", "明日にしたよ。", "Okay."]);
    }

    #[test]
    fn marks_spread_over_the_text_sent_and_settle_exactly_at_each_end() {
        let mut m = Marks::default();
        m.sent(6, 5);
        m.sent(4, 4);
        assert_eq!(m.mark(3), 2);
        assert_eq!(m.mark(3), 3, "the first message ends whole");
        assert_eq!(m.mark(10), 4, "a mark past the text sent counts no further");
        m.sent(8, 8);
        m.mark(2);
        assert_eq!(m.rest(), 6);
        assert!(m.is_empty());
    }
}
