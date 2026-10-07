#[derive(Default)]
pub struct Clauses {
    buf: String,
    scanned: usize,
    emitted_any: bool,
}

impl Clauses {
    /// Feeds streamed text; returns clauses ready to speak.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        let mut out = Vec::new();
        while let Some((end, skip)) = self.cut_point() {
            let clause = self.buf[..end].trim().to_string();
            self.buf.drain(..end + skip);
            self.scanned = 0;
            if !clause.is_empty() {
                self.emitted_any = true;
                out.push(clause);
            }
        }
        out
    }

    /// The remainder at the end of the stream.
    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        self.scanned = 0;
        let rest = rest.trim();
        (!rest.is_empty()).then(|| {
            self.emitted_any = true;
            rest.to_string()
        })
    }

    /// The first place to cut the buffer: the clause's byte end and how many bytes after it to drop.
    /// Resumes at the last char that still waited for its next one.
    fn cut_point(&mut self) -> Option<(usize, usize)> {
        let comma_floor = if self.emitted_any {
            LATER_COMMA_FLOOR
        } else {
            FIRST_COMMA_FLOOR
        };
        let mut chars = self.buf[self.scanned..]
            .char_indices()
            .map(|(i, c)| (i + self.scanned, c))
            .peekable();
        while let Some((i, c)) = chars.next() {
            if c == '\n' {
                return Some((i, 1));
            }
            let Some(&(_, next)) = chars.peek() else {
                self.scanned = i;
                break;
            };
            let end = i + c.len_utf8();
            let long_enough = || self.buf[..end].trim_start().chars().count() >= comma_floor;
            let after_stop = || self.buf[..i].chars().next_back().is_some_and(|p| FULL_STOPS.contains(&p));
            let full_width_end = (FULL_STOPS.contains(&c) && !CLOSERS.contains(&next))
                || (CLOSERS.contains(&c) && after_stop())
                || (c == '、' && long_enough());
            if full_width_end {
                return Some((end, 0));
            }
            if !next.is_whitespace() {
                continue;
            }
            let ends_clause = matches!(c, '.' | '!' | '?' | ';' | ':') || (c == ',' && long_enough());
            if ends_clause {
                return Some((end, 0));
            }
        }
        None
    }
}

/// Japanese sentence ends, which no space follows.
const FULL_STOPS: [char; 3] = ['。', '！', '？'];
const CLOSERS: [char; 4] = ['」', '』', '）', ')'];

/// Clauses as one line: a space between two that meet in ASCII, none where
/// either side is Japanese.
pub fn join(clauses: &[String]) -> String {
    let mut out = String::new();
    for clause in clauses {
        let spaced = out.chars().next_back().is_some_and(|c| c.is_ascii())
            && clause.chars().next().is_some_and(|c| c.is_ascii());
        if spaced {
            out.push(' ');
        }
        out.push_str(clause);
    }
    out
}

const FIRST_COMMA_FLOOR: usize = 8;
const LATER_COMMA_FLOOR: usize = 24;

/// Strips `*`, `_`, `#`, backticks and leading "- ", and collapses whitespace.
/// An `_` between word characters reads as a space ("`web_search`" → "web search").
pub fn speakable(text: &str) -> String {
    let chars: Vec<char> = text
        .lines()
        .map(|line| {
            let line = line.trim_start();
            line.strip_prefix("- ").unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    let mut spoken = String::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '_' if i > 0
                && chars[i - 1].is_alphanumeric()
                && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric()) =>
            {
                spoken.push(' ');
            }
            '*' | '_' | '#' | '`' => {}
            _ => spoken.push(c),
        }
    }
    spoken.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(deltas: &[&str]) -> Vec<String> {
        let mut c = Clauses::default();
        let mut out: Vec<String> = deltas.iter().flat_map(|d| c.push(d)).collect();
        out.extend(c.finish());
        out
    }

    #[test]
    fn the_first_clause_comes_early_and_numbers_stay_whole() {
        let mut c = Clauses::default();
        let mut out = c.push("Sure, I moved it to 7.30 tomorrow");
        out.extend(c.push(". Anything else?"));
        out.extend(c.finish());
        assert_eq!(
            out,
            vec!["Sure, I moved it to 7.30 tomorrow.", "Anything else?"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            "\"Sure,\" is under the first clause's 8-char floor, so it joins the next clause"
        );
    }

    #[test]
    fn a_long_enough_first_clause_cuts_at_its_comma() {
        assert_eq!(
            all(&["Alright then, it's done"]),
            vec!["Alright then,", "it's done"]
        );
    }

    #[test]
    fn later_commas_need_twenty_four_chars() {
        assert_eq!(
            all(&["Done. Then, the run moves to Friday morning, as asked"]),
            vec![
                "Done.",
                "Then, the run moves to Friday morning,",
                "as asked"
            ]
        );
    }

    #[test]
    fn newlines_cut_and_thousands_stay_whole() {
        assert_eq!(
            all(&["You have 1,000 steps\n\nGo"]),
            vec!["You have 1,000 steps", "Go"]
        );
    }

    #[test]
    fn punctuation_waits_for_the_next_char() {
        let mut c = Clauses::default();
        assert!(c.push("It is 7.").is_empty());
        assert!(c.push("30").is_empty());
        assert_eq!(c.push(": fine"), vec!["It is 7.30:"]);
        assert_eq!(c.finish().as_deref(), Some("fine"));
        assert_eq!(c.finish(), None);
    }

    #[test]
    fn japanese_cuts_at_its_full_stops_and_long_commas() {
        assert_eq!(
            all(&["はい、わかりました。金曜の朝に", "移しました！ほかに何かありますか？"]),
            vec!["はい、わかりました。", "金曜の朝に移しました！", "ほかに何かありますか？"]
        );
        assert_eq!(all(&["「了解です。」と言いました。"]), vec!["「了解です。」", "と言いました。"]);
        assert_eq!(
            all(&["明日の会議の資料をまとめておいたので、あとで確認してください"]),
            vec!["明日の会議の資料をまとめておいたので、", "あとで確認してください"]
        );
    }

    #[test]
    fn clauses_join_with_spaces_only_between_latin_text() {
        let s = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(join(&s(&["Done.", "Anything else?"])), "Done. Anything else?");
        assert_eq!(join(&s(&["はい。", "移しました。"])), "はい。移しました。");
        assert_eq!(join(&s(&["OK.", "移しました。"])), "OK.移しました。");
    }

    #[test]
    fn markdown_is_not_spoken() {
        assert_eq!(speakable("**Done** — `task 4`"), "Done — task 4");
    }

    #[test]
    fn snake_case_reads_as_words_and_emphasis_underscores_go() {
        assert_eq!(
            speakable("ran web_search, _really_ __done__"),
            "ran web search, really done"
        );
    }

    #[test]
    fn a_comma_rejected_for_length_stays_rejected_across_pushes() {
        let mut c = Clauses::default();
        assert!(c.push("Sure, ").is_empty());
        assert!(c.push("ok").is_empty());
        assert_eq!(c.push(". Next"), vec!["Sure, ok."]);
        assert_eq!(c.finish().as_deref(), Some("Next"));
    }

    #[test]
    fn list_dashes_and_headings_are_not_spoken() {
        assert_eq!(speakable("## Plan\n- one\n-  two"), "Plan one two");
    }
}
