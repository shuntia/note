use super::clauses::{speakable, Clauses};
use crate::providers::{ChatRequest, LLMProvider, Message, StreamOpts, StreamSink, ToolCall};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

#[derive(Debug, Clone, Copy)]
pub struct TurnSpec {
    pub reply: u64,
    pub draft: bool,
}

#[derive(Debug, Clone)]
pub enum TurnEvent {
    /// A clause to speak, already `speakable()`.
    Clause { reply: u64, idx: u32, text: String },
    /// A complete tool call, in stream order, with its index in the round.
    Call {
        reply: u64,
        index: usize,
        call: ToolCall,
    },
    /// The stream is over (or was stopped).
    End {
        reply: u64,
        text: String,
        calls: Vec<ToolCall>,
        stopped: bool,
        error: Option<crate::failure::Failure>,
    },
}

/// Streams one round on its own thread; events go to `tx`; setting `stop` ends the stream at the next delta.
/// A failure before any delta is retried once, unless retrying cannot help. Text pending before a tool call is spoken first.
/// `End.calls` holds only the calls sent as events.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    llm: Arc<dyn LLMProvider>,
    system: Arc<String>,
    messages: Vec<Message>,
    tools: Arc<Vec<serde_json::Value>>,
    spec: TurnSpec,
    opts: StreamOpts,
    stop: Arc<AtomicBool>,
    tx: impl Fn(TurnEvent) + Send + 'static,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let req = ChatRequest {
            system: &system,
            messages: &messages,
            tools: &tools,
            background: false,
        };
        let mut sink = TurnSink::new(spec.reply, &stop, &tx);
        let mut result = llm.chat_stream(&req, &opts, &mut sink);
        let worth_retrying = result.as_ref().is_err_and(|e| !crate::failure::Reason::of(e).lasts());
        if worth_retrying && !sink.any_delta && !stop.load(Ordering::SeqCst) {
            result = llm.chat_stream(&req, &opts, &mut sink);
        }
        let stopped = stop.load(Ordering::SeqCst);
        if !stopped {
            if let Some(rest) = sink.clauses.finish() {
                sink.speak(&rest);
            }
        }
        tx(TurnEvent::End {
            reply: spec.reply,
            text: sink.text,
            calls: sink.calls,
            stopped,
            error: result.err().map(|e| crate::failure::Failure::of(&e)),
        });
    })
}

struct TurnSink<'a, F> {
    reply: u64,
    stop: &'a AtomicBool,
    tx: &'a F,
    clauses: Clauses,
    next_idx: u32,
    text: String,
    calls: Vec<ToolCall>,
    any_delta: bool,
}

impl<'a, F: Fn(TurnEvent)> TurnSink<'a, F> {
    fn new(reply: u64, stop: &'a AtomicBool, tx: &'a F) -> Self {
        Self {
            reply,
            stop,
            tx,
            clauses: Clauses::default(),
            next_idx: 0,
            text: String::new(),
            calls: Vec::new(),
            any_delta: false,
        }
    }

    fn speak(&mut self, clause: &str) {
        let text = speakable(clause);
        if text.is_empty() {
            return;
        }
        (self.tx)(TurnEvent::Clause {
            reply: self.reply,
            idx: self.next_idx,
            text,
        });
        self.next_idx += 1;
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

impl<F: Fn(TurnEvent)> StreamSink for TurnSink<'_, F> {
    fn text(&mut self, delta: &str) -> bool {
        self.any_delta = true;
        self.text.push_str(delta);
        if self.stopped() {
            return false;
        }
        for clause in self.clauses.push(delta) {
            self.speak(&clause);
        }
        !self.stopped()
    }

    fn tool_call(&mut self, call: &ToolCall) -> bool {
        self.any_delta = true;
        if self.stopped() {
            return false;
        }
        if let Some(rest) = self.clauses.finish() {
            self.speak(&rest);
        }
        (self.tx)(TurnEvent::Call {
            reply: self.reply,
            index: self.calls.len(),
            call: call.clone(),
        });
        self.calls.push(call.clone());
        !self.stopped()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::mock::{MockLLM, StreamPiece};
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(5);

    fn start(
        llm: Arc<MockLLM>,
        stop: Arc<AtomicBool>,
    ) -> (mpsc::Receiver<TurnEvent>, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel();
        let handle = spawn(
            llm,
            Arc::new("sys".into()),
            vec![Message::User("hi".into())],
            Arc::new(vec![]),
            TurnSpec {
                reply: 7,
                draft: false,
            },
            StreamOpts {
                first_token: Duration::from_secs(1),
            },
            stop,
            move |e| {
                let _ = tx.send(e);
            },
        );
        (rx, handle)
    }

    fn until_end(rx: &mpsc::Receiver<TurnEvent>) -> Vec<TurnEvent> {
        let mut out = Vec::new();
        loop {
            let e = rx.recv_timeout(WAIT).expect("turn event");
            let end = matches!(e, TurnEvent::End { .. });
            out.push(e);
            if end {
                return out;
            }
        }
    }

    fn search() -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: "web_search".into(),
            args: r#"{"q":"x"}"#.into(),
        }
    }

    #[test]
    fn clauses_and_calls_stream_in_order() {
        let llm = Arc::new(MockLLM::streamed(vec![vec![
            StreamPiece::Text("Let me check, "),
            StreamPiece::Text("one sec."),
            StreamPiece::Call(search()),
        ]]));
        let (rx, handle) = start(llm, Arc::new(AtomicBool::new(false)));
        let events = until_end(&rx);
        handle.join().unwrap();
        assert_eq!(events.len(), 4, "{events:?}");
        assert!(
            matches!(&events[0], TurnEvent::Clause { reply: 7, idx: 0, text } if text == "Let me check,")
        );
        assert!(
            matches!(&events[1], TurnEvent::Clause { reply: 7, idx: 1, text } if text == "one sec.")
        );
        assert!(
            matches!(&events[2], TurnEvent::Call { reply: 7, index: 0, call } if call.name == "web_search")
        );
        let TurnEvent::End {
            reply,
            text,
            calls,
            stopped,
            error,
        } = &events[3]
        else {
            unreachable!()
        };
        assert_eq!(
            (*reply, text.as_str(), calls.len(), *stopped, error),
            (7, "Let me check, one sec.", 1, false, &None)
        );
    }

    #[test]
    fn stop_ends_the_stream_and_keeps_what_arrived() {
        let llm = Arc::new(MockLLM::streamed(vec![vec![
            StreamPiece::Text("Let me check, "),
            StreamPiece::Wait(Duration::from_millis(200)),
            StreamPiece::Text("one sec."),
            StreamPiece::Wait(Duration::from_millis(200)),
            StreamPiece::Call(search()),
        ]]));
        let stop = Arc::new(AtomicBool::new(false));
        let (rx, handle) = start(llm, stop.clone());
        let first = rx.recv_timeout(WAIT).expect("first clause");
        assert!(
            matches!(&first, TurnEvent::Clause { idx: 0, .. }),
            "{first:?}"
        );
        stop.store(true, Ordering::SeqCst);
        let rest = until_end(&rx);
        handle.join().unwrap();
        assert_eq!(rest.len(), 1, "{rest:?}");
        let TurnEvent::End {
            text,
            calls,
            stopped,
            error,
            ..
        } = &rest[0]
        else {
            unreachable!()
        };
        assert!(*stopped);
        assert!(calls.is_empty());
        assert!(error.is_none());
        assert!(text.starts_with("Let me check,"), "{text}");
    }

    #[test]
    fn a_stalled_model_retries_once_then_apologises() {
        let llm = Arc::new(MockLLM::streamed(vec![
            vec![StreamPiece::Fail("stalled")],
            vec![StreamPiece::Fail("stalled")],
        ]));
        let (rx, handle) = start(llm.clone(), Arc::new(AtomicBool::new(false)));
        let events = until_end(&rx);
        handle.join().unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        let TurnEvent::End {
            text,
            error,
            stopped,
            ..
        } = &events[0]
        else {
            unreachable!()
        };
        assert!(text.is_empty());
        assert!(!stopped);
        assert!(error.is_some());
        assert_eq!(llm.seen().len(), 2);
    }

    #[test]
    fn a_failure_before_any_delta_recovers_on_the_retry() {
        let llm = Arc::new(MockLLM::streamed(vec![
            vec![StreamPiece::Fail("stalled")],
            vec![StreamPiece::Text("Here.")],
        ]));
        let (rx, handle) = start(llm.clone(), Arc::new(AtomicBool::new(false)));
        let events = until_end(&rx);
        handle.join().unwrap();
        assert!(
            matches!(&events[0], TurnEvent::Clause { idx: 0, text, .. } if text == "Here."),
            "{events:?}"
        );
        assert!(matches!(&events[1], TurnEvent::End { error: None, .. }));
        assert_eq!(llm.seen().len(), 2);
    }

    #[test]
    fn a_failure_after_deltas_ends_with_what_arrived() {
        let llm = Arc::new(MockLLM::streamed(vec![
            vec![StreamPiece::Text("Okay, so"), StreamPiece::Fail("dropped")],
            vec![StreamPiece::Text("never")],
        ]));
        let (rx, handle) = start(llm.clone(), Arc::new(AtomicBool::new(false)));
        let events = until_end(&rx);
        handle.join().unwrap();
        let TurnEvent::End { text, error, .. } = events.last().unwrap() else {
            unreachable!()
        };
        assert_eq!(text, "Okay, so");
        assert!(error.is_some());
        assert_eq!(llm.seen().len(), 1);
    }

    #[test]
    fn a_clause_with_nothing_speakable_takes_no_index() {
        let llm = Arc::new(MockLLM::streamed(vec![vec![
            StreamPiece::Text("** "),
            StreamPiece::Text("\n\nDone."),
        ]]));
        let (rx, handle) = start(llm, Arc::new(AtomicBool::new(false)));
        let events = until_end(&rx);
        handle.join().unwrap();
        let clauses: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                TurnEvent::Clause { idx, text, .. } => Some((*idx, text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(clauses, vec![(0, "Done.")]);
    }
}
