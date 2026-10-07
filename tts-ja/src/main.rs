mod audio;
mod engines;
mod sbv2;
mod text;
mod voicevox;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

use engines::{Engines, Settings, Voice, SAMPLE_RATE, VOICES};

const FRAME_SAMPLES: usize = SAMPLE_RATE as usize / 4;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Incoming {
    Open { voice: Option<String> },
    Text { text: String },
    End,
    Cancel,
}

fn env(name: &str) -> Option<String> {
    std::env::var(format!("NOTE_TTS_{name}"))
        .ok()
        .filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let host = env("HOST").unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = env("PORT")
        .map_or(Ok(8891), |p| p.parse())
        .context("NOTE_TTS_PORT")?;
    let settings = Settings {
        sbv2_dir: if env("JA_SBV2").as_deref() == Some("off") {
            None
        } else {
            Some(
                env("JA_SBV2_DIR")
                    .map(PathBuf::from)
                    .context("NOTE_TTS_JA_SBV2_DIR is not set")?,
            )
        },
        voicevox_dir: env("JA_VOICEVOX_DIR")
            .map(PathBuf::from)
            .context("NOTE_TTS_JA_VOICEVOX_DIR is not set")?,
        gpu: env("DEVICE").as_deref() != Some("cpu"),
    };

    ort::init().with_name("note-tts-ja").commit()?;
    let engines = Arc::new(Engines::default());
    let loader = engines.clone();
    std::thread::spawn(move || {
        if let Err(e) = loader.load(&settings) {
            eprintln!("tts-ja: {e:#}");
            std::process::exit(1);
        }
    });

    let app = Router::new()
        .route("/info", get(info))
        .route("/stream", get(stream))
        .with_state(engines);
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn info(State(engines): State<Arc<Engines>>) -> Json<serde_json::Value> {
    let voices: Vec<_> = VOICES
        .iter()
        .map(|v| json!({ "id": v.id, "label": v.label, "languages": ["ja"], "credit": v.credit }))
        .collect();
    Json(json!({
        "id": "ja",
        "label": "Natural",
        "input": "chunks",
        "sample_rate": SAMPLE_RATE,
        "ready": engines.ready(),
        "voices": voices,
    }))
}

async fn stream(ws: WebSocketUpgrade, State(engines): State<Arc<Engines>>) -> Response {
    ws.on_upgrade(move |socket| serve(socket, engines))
}

type Sink = SplitSink<WebSocket, Message>;

/// One stream: `open`, then chunks of text, each answered with its audio and a mark.
async fn serve(socket: WebSocket, engines: Arc<Engines>) {
    let (mut sink, mut source) = socket.split();
    let voice = match opening(&mut source, &engines).await {
        Ok(voice) => voice,
        Err(Some(message)) => return fail(&mut sink, &message).await,
        Err(None) => return,
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let (queue, chunks) = mpsc::unbounded_channel::<Option<String>>();
    let speaker = tokio::spawn(speak(sink, voice, engines, cancel.clone(), chunks));
    while let Some(Ok(msg)) = source.next().await {
        let raw = match msg {
            Message::Text(raw) => raw,
            Message::Close(_) => break,
            _ => continue,
        };
        match serde_json::from_str::<Incoming>(&raw) {
            Ok(Incoming::Text { text }) => drop(queue.send(Some(text))),
            Ok(Incoming::End) => drop(queue.send(None)),
            _ => break,
        }
    }
    cancel.store(true, Ordering::Release);
    let _ = queue.send(None);
    let _ = speaker.await;
}

/// The voice of the stream's `open`; Err(None) when the client left first.
async fn opening(
    source: &mut SplitStream<WebSocket>,
    engines: &Engines,
) -> Result<Voice, Option<String>> {
    loop {
        let raw = match source.next().await {
            Some(Ok(Message::Text(raw))) => raw,
            Some(Ok(Message::Close(_)) | Err(_)) | None => return Err(None),
            Some(Ok(_)) => continue,
        };
        return match serde_json::from_str::<Incoming>(&raw) {
            Ok(Incoming::Open { .. }) if !engines.ready() => {
                Err(Some("model is still loading".into()))
            }
            Ok(Incoming::Open { voice }) => {
                let id = voice.unwrap_or_else(|| VOICES[0].id.into());
                VOICES
                    .iter()
                    .find(|v| v.id == id)
                    .map(|v| v.voice)
                    .ok_or_else(|| Some(format!("unknown voice {id:?}")))
            }
            Ok(Incoming::Cancel) => Err(None),
            Ok(_) => Err(Some("stream not opened".into())),
            Err(_) => Err(Some("malformed message".into())),
        };
    }
}

async fn speak(
    mut sink: Sink,
    voice: Voice,
    engines: Arc<Engines>,
    cancel: Arc<AtomicBool>,
    mut chunks: mpsc::UnboundedReceiver<Option<String>>,
) {
    while let Some(Some(text)) = chunks.recv().await {
        if cancel.load(Ordering::Acquire) {
            return;
        }
        let (rendering, chunk) = (engines.clone(), text.clone());
        let pcm = match tokio::task::spawn_blocking(move || rendering.render(voice, &chunk)).await {
            Ok(Ok(pcm)) => pcm,
            Ok(Err(e)) => return fail(&mut sink, &format!("{e:#}")).await,
            Err(e) => return fail(&mut sink, &format!("render failed: {e}")).await,
        };
        if cancel.load(Ordering::Acquire) {
            return;
        }
        for frame in pcm.chunks(FRAME_SAMPLES) {
            let bytes: Vec<u8> = frame.iter().flat_map(|s| s.to_le_bytes()).collect();
            if sink.send(Message::Binary(bytes.into())).await.is_err() {
                return;
            }
        }
        let mark = json!({ "type": "mark", "chars": text.chars().count() });
        if sink
            .send(Message::Text(mark.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    if !cancel.load(Ordering::Acquire) {
        let _ = sink
            .send(Message::Text(json!({ "type": "done" }).to_string().into()))
            .await;
    }
    let _ = sink.close().await;
}

async fn fail(sink: &mut Sink, message: &str) {
    eprintln!("tts-ja: stream refused: {message}");
    let _ = sink
        .send(Message::Text(
            json!({ "type": "error", "message": message })
                .to_string()
                .into(),
        ))
        .await;
    let _ = sink.close().await;
}
