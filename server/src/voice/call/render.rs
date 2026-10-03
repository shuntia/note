use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum JobOutcome {
    Done(String),
    Error(String),
    Cancelled,
    TimedOut,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Heard(String),
    Job {
        job: u32,
        tool: String,
        outcome: JobOutcome,
    },
    /// A note from the loop itself, e.g. "the call connected" or "Note restarted".
    System(String),
}

pub struct Running {
    pub job: u32,
    pub tool: String,
    pub elapsed: Duration,
}

pub const RESULT_CAP: usize = 4096;

pub fn block(items: &[Item], running: &[Running]) -> String {
    let mut lines: Vec<String> = items.iter().map(item_line).collect();
    lines.extend(running.iter().map(|r| {
        format!(
            "[running] job {} · {} ({:.1} s)",
            r.job,
            r.tool,
            r.elapsed.as_secs_f64()
        )
    }));
    lines.join("\n")
}

fn item_line(item: &Item) -> String {
    match item {
        Item::Heard(text) => format!("[you] {text}"),
        Item::System(text) => format!("[note] {text}"),
        Item::Job { job, tool, outcome } => match outcome {
            JobOutcome::Done(result) => format!("[job {job} · {tool} · done] {}", capped(result)),
            JobOutcome::Error(message) => format!("[job {job} · {tool} · error] {}", capped(message)),
            JobOutcome::Cancelled => format!("[job {job} · {tool} · cancelled]"),
            JobOutcome::TimedOut => format!("[job {job} · {tool} · timed out]"),
            JobOutcome::Interrupted => format!(
                "[job {job} · {tool} · interrupted] Note restarted before it finished; call it again if it still matters."
            ),
        },
    }
}

fn capped(text: &str) -> String {
    if text.len() <= RESULT_CAP {
        return text.to_string();
    }
    let mut end = RESULT_CAP;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} …(cut)", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_and_words_render_in_arrival_order_then_running() {
        let s = block(
            &[
                Item::Job {
                    job: 3,
                    tool: "web_search".into(),
                    outcome: JobOutcome::Done("{\"x\":1}".into()),
                },
                Item::Heard("can you also move my run".into()),
            ],
            &[Running {
                job: 4,
                tool: "calendar_add".into(),
                elapsed: Duration::from_millis(2100),
            }],
        );
        assert_eq!(
            s,
            "[job 3 · web_search · done] {\"x\":1}\n[you] can you also move my run\n[running] job 4 · calendar_add (2.1 s)"
        );
    }

    #[test]
    fn a_long_result_is_cut_and_says_so() {
        let result = "é".repeat(2500);
        let s = block(
            &[Item::Job {
                job: 1,
                tool: "t".into(),
                outcome: JobOutcome::Done(result),
            }],
            &[],
        );
        let prefix = "[job 1 · t · done] ";
        let rest = s.strip_prefix(prefix).unwrap();
        let kept = rest
            .strip_suffix(" …(cut)")
            .expect("ends with the cut marker");
        assert!(kept.len() <= RESULT_CAP);
        assert!(kept.len() >= RESULT_CAP - 1);
    }

    #[test]
    fn every_outcome_has_its_line() {
        let job = |outcome| Item::Job {
            job: 2,
            tool: "x".into(),
            outcome,
        };
        let s = block(
            &[
                job(JobOutcome::Error("boom".into())),
                job(JobOutcome::Cancelled),
                job(JobOutcome::TimedOut),
                job(JobOutcome::Interrupted),
                Item::System("the call connected".into()),
            ],
            &[],
        );
        assert_eq!(
            s,
            "[job 2 · x · error] boom\n\
             [job 2 · x · cancelled]\n\
             [job 2 · x · timed out]\n\
             [job 2 · x · interrupted] Note restarted before it finished; call it again if it still matters.\n\
             [note] the call connected"
        );
    }
}
