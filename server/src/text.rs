//! Every user-visible line the server writes itself, in each language it speaks.

use std::path::Path;

pub const LANGUAGES: [&str; 3] = ["", "en", "ja"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    En,
    Ja,
}

impl Lang {
    /// `""` (auto) is `None`: the caller decides from what it knows of the reader.
    pub fn from_setting(setting: &str) -> Option<Self> {
        match setting {
            "en" => Some(Lang::En),
            "ja" => Some(Lang::Ja),
            _ => None,
        }
    }

    /// The language of the highest-weighted tag in an `Accept-Language` header
    /// that the server speaks.
    pub fn from_accept_language(header: &str) -> Option<Self> {
        let mut best: Option<(f32, Lang)> = None;
        for part in header.split(',') {
            let mut fields = part.split(';');
            let tag = fields.next().unwrap_or_default().trim().to_ascii_lowercase();
            let q = fields
                .find_map(|f| f.trim().strip_prefix("q=").and_then(|q| q.parse::<f32>().ok()))
                .unwrap_or(1.0);
            let primary = tag.split('-').next().unwrap_or_default();
            let Some(lang) = Lang::from_setting(primary) else { continue };
            if q > 0.0 && best.is_none_or(|(b, _)| q > b) {
                best = Some((q, lang));
            }
        }
        best.map(|(_, lang)| lang)
    }

    /// The user's chosen language; auto follows `accept_language` where a
    /// request carries one, and is English where nothing says otherwise.
    pub fn resolve(setting: &str, accept_language: Option<&str>) -> Self {
        Lang::from_setting(setting)
            .or_else(|| accept_language.and_then(Lang::from_accept_language))
            .unwrap_or_default()
    }

    /// For text sent with no request in hand: auto follows the language of the
    /// user's browser when it last called.
    pub fn for_user(config_dir: &Path, username: &str) -> Self {
        crate::config::UserConfig::load(config_dir, username)
            .ok()
            .and_then(|c| Lang::from_setting(c.language()))
            .or_else(|| seen(config_dir, username))
            .unwrap_or_default()
    }

    /// For a request: the setting, then the request's `Accept-Language`, then
    /// the browser last seen.
    pub fn for_request(config_dir: &Path, username: &str, accept_language: Option<&str>) -> Self {
        let setting = crate::config::UserConfig::load(config_dir, username)
            .ok()
            .and_then(|c| Lang::from_setting(c.language()));
        setting
            .or_else(|| accept_language.and_then(Lang::from_accept_language))
            .or_else(|| seen(config_dir, username))
            .unwrap_or_default()
    }

    /// The tag the voice side and the web know the language by.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ja => "ja",
        }
    }

    fn pick(self, en: &str, ja: &str) -> String {
        match self {
            Lang::En => en.to_string(),
            Lang::Ja => ja.to_string(),
        }
    }
}

fn seen_path(config_dir: &Path, username: &str) -> std::path::PathBuf {
    config_dir.join("users").join(username).join("seen_language")
}

/// The language of the user's browser when it last called, if one was seen.
pub fn seen(config_dir: &Path, username: &str) -> Option<Lang> {
    let raw = std::fs::read_to_string(seen_path(config_dir, username)).ok()?;
    Lang::from_setting(raw.trim())
}

pub fn remember_seen(config_dir: &Path, username: &str, lang: Lang) -> std::io::Result<()> {
    let path = seen_path(config_dir, username);
    std::fs::create_dir_all(path.parent().expect("a user's file always has a parent"))?;
    crate::context::write_atomic(&path, lang.code())
}

/// The browser language each user was last seen with, so a request writes
/// the file only when it changes.
#[derive(Default)]
pub struct SeenLangs(std::sync::Mutex<std::collections::HashMap<i64, Lang>>);

impl SeenLangs {
    /// Records the language of a signed-in request's `Accept-Language`; an
    /// unspoken language counts as English.
    pub fn note(&self, config_dir: &Path, user_id: i64, username: &str, accept_language: &str) {
        if accept_language.trim().is_empty() {
            return;
        }
        let lang = Lang::from_accept_language(accept_language).unwrap_or_default();
        let mut seen_by = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if seen_by.get(&user_id) == Some(&lang) {
            return;
        }
        if seen(config_dir, username) == Some(lang) || remember_seen(config_dir, username, lang).is_ok() {
            seen_by.insert(user_id, lang);
        }
    }
}

pub fn action_done(l: Lang) -> String {
    l.pick("Done", "完了")
}

pub fn action_snooze_15(l: Lang) -> String {
    l.pick("Snooze 15", "15分後に")
}

pub fn action_drop(l: Lang) -> String {
    l.pick("Drop", "やめる")
}

pub fn action_start_session(l: Lang) -> String {
    l.pick("Start session", "セッション開始")
}

pub fn action_carry_to_tomorrow(l: Lang) -> String {
    l.pick("Carry to tomorrow", "明日に回す")
}

pub fn checkin_title(l: Lang) -> String {
    l.pick("Check-in", "チェックイン")
}

pub fn checkin_body(l: Lang, at: &str) -> String {
    match l {
        Lang::En => format!("Time for your {at} check-in — how is the day going?"),
        Lang::Ja => format!("{at}のチェックインです。今日はどうですか？"),
    }
}

/// A check-in thread's name before its first reply; `day` is `None` when the
/// plan date does not parse.
pub fn checkin_thread_title(l: Lang, day: Option<jiff::civil::Date>, at: &str) -> String {
    match (l, day) {
        (Lang::En, Some(day)) => format!("{}'s {at} check-in", day.strftime("%A")),
        (Lang::En, None) => format!("The {at} check-in"),
        (Lang::Ja, Some(day)) => format!("{}の{at}のチェックイン", weekday_ja(day.weekday())),
        (Lang::Ja, None) => format!("{at}のチェックイン"),
    }
}

fn weekday_ja(day: jiff::civil::Weekday) -> &'static str {
    use jiff::civil::Weekday as W;
    match day {
        W::Monday => "月曜",
        W::Tuesday => "火曜",
        W::Wednesday => "水曜",
        W::Thursday => "木曜",
        W::Friday => "金曜",
        W::Saturday => "土曜",
        W::Sunday => "日曜",
    }
}

pub fn good_morning(l: Lang) -> String {
    l.pick("Good morning", "おはようございます")
}

pub fn no_debrief(l: Lang) -> String {
    l.pick("(no debrief yet)", "（まだ朝の手紙はありません）")
}

pub fn your_week(l: Lang) -> String {
    l.pick("Your week", "この一週間")
}

pub fn no_week(l: Lang) -> String {
    l.pick("(no week yet)", "（まだ週のふりかえりはありません）")
}

/// The body of a fired event of a kind with nothing more specific to say.
pub fn scheduled_for(l: Lang, at: &str) -> String {
    match l {
        Lang::En => format!("scheduled for {at}"),
        Lang::Ja => format!("{at}の予定"),
    }
}

pub fn starting_now(l: Lang) -> String {
    l.pick("Starting now", "開始の時間")
}

pub fn block_start_body(l: Lang, task: &str, until: &str) -> String {
    match l {
        Lang::En => format!("{task} · until {until}"),
        Lang::Ja => format!("{task} · {until}まで"),
    }
}

/// The same start as one line in the day's thread.
pub fn block_start_line(l: Lang, task: &str, until: &str) -> String {
    match l {
        Lang::En => format!("Starting now: {task}, until {until}."),
        Lang::Ja => format!("開始の時間です：{task}、{until}まで。"),
    }
}

pub fn break_title(l: Lang) -> String {
    l.pick("Break", "休憩")
}

pub fn break_body(l: Lang, min: i64, round: i64, task: &str) -> String {
    match l {
        Lang::En => format!("{min} min. Round {round} of {task} done."),
        Lang::Ja => format!("{min}分。{task}の{round}ラウンド目が終わりました。"),
    }
}

pub fn round_title(l: Lang, round: i64) -> String {
    match l {
        Lang::En => format!("Round {round}"),
        Lang::Ja => format!("{round}ラウンド目"),
    }
}

pub fn round_body(l: Lang, task: &str) -> String {
    match l {
        Lang::En => format!("Back to {task}."),
        Lang::Ja => format!("{task}に戻りましょう。"),
    }
}

pub fn times_up(l: Lang) -> String {
    l.pick("Time's up.", "時間です。")
}

pub fn overrun_ended_title(l: Lang) -> String {
    l.pick("Force-terminating", "強制終了")
}

pub fn overrun_ended_body(l: Lang, task: &str, elapsed: i64, planned: i64) -> String {
    match l {
        Lang::En => format!(
            "{task} ran {elapsed} min against {planned} planned, so it ends here. \
             Start it again when you are ready."
        ),
        Lang::Ja => format!(
            "{task}は予定{planned}分のところ{elapsed}分続いたので、ここで終えます。\
             準備ができたらまた始めてください。"
        ),
    }
}

pub fn overrun_ask_title(l: Lang) -> String {
    l.pick("Are you OK?", "大丈夫ですか？")
}

pub fn overrun_ask_body(l: Lang, task: &str, elapsed: i64, planned: i64, extra: i64) -> String {
    match l {
        Lang::En => format!(
            "{task} has run {elapsed} min against {planned} planned. How is it going? \
             Take a break, or give it {extra} more minutes."
        ),
        Lang::Ja => format!(
            "{task}は予定{planned}分のところ{elapsed}分経ちました。調子はどうですか？\
             休憩するか、あと{extra}分続けましょう。"
        ),
    }
}

/// A notification's title and body as one line in a thread.
pub fn titled(l: Lang, title: &str, body: &str) -> String {
    match l {
        Lang::En => format!("{title}: {body}"),
        Lang::Ja => format!("{title}：{body}"),
    }
}

pub fn matrix_fresh(l: Lang) -> String {
    l.pick("Fresh start.", "新しく始めます。")
}

pub fn matrix_capped(l: Lang) -> String {
    l.pick("You've used today's sessions.", "今日のセッションは使い切りました。")
}

pub fn matrix_busy(l: Lang) -> String {
    l.pick("Still on your last message.", "前のメッセージに返信中です。")
}

pub fn matrix_unreachable(l: Lang) -> String {
    l.pick("Couldn't reach Note right now.", "今はNoteにつながりません。")
}

/// What a visitor on a share link is told once their message is filed.
pub fn share_passed_on(l: Lang, owner: &str) -> String {
    match l {
        Lang::En => format!("Passed on to {owner}."),
        Lang::Ja => format!("{owner}さんに伝えました。"),
    }
}

pub fn share_note_from(l: Lang, share: &str) -> String {
    match l {
        Lang::En => format!("Note from {share}"),
        Lang::Ja => format!("{share}からのメモ"),
    }
}

pub fn empty_reply(l: Lang) -> String {
    l.pick(
        "(the assistant is not configured on this server)",
        "（このサーバーではアシスタントが設定されていません）",
    )
}

pub fn test_call_title(l: Lang) -> String {
    l.pick("Test call", "テスト通話")
}

pub fn test_call_body(l: Lang) -> String {
    l.pick("This was a test call from Note.", "Noteからのテスト通話でした。")
}

pub fn test_notification(l: Lang) -> String {
    l.pick("Test notification", "テスト通知")
}

pub fn err_no_channel(l: Lang) -> String {
    l.pick("no channel could reach you", "どの方法でも届きませんでした")
}

pub fn err_calls_not_set_up(l: Lang) -> String {
    l.pick("calls are not set up on this server", "このサーバーでは通話が使えません")
}

pub fn err_link_matrix_first(l: Lang) -> String {
    l.pick("link a Matrix account first", "先にMatrixアカウントをリンクしてください")
}

pub fn err_call_service_disconnected(l: Lang) -> String {
    l.pick("the call service isn't connected", "通話サービスにつながっていません")
}

pub fn err_call_service_unreachable(l: Lang) -> String {
    l.pick(
        "The call service isn't reachable right now. Try again in a minute.",
        "今は通話サービスにつながりません。少ししてからもう一度どうぞ。",
    )
}

pub fn err_already_ringing(l: Lang) -> String {
    l.pick("Already ringing", "呼び出し中です")
}

pub fn err_daily_cap(l: Lang) -> String {
    l.pick("daily session limit reached", "今日のセッション上限に達しました")
}

pub fn err_session_in_progress(l: Lang) -> String {
    l.pick("a session is already in progress", "別のセッションが進行中です")
}

pub fn err_at_capacity(l: Lang) -> String {
    l.pick("the server is at capacity", "サーバーが混み合っています")
}

pub fn err_reply_in_progress(l: Lang) -> String {
    l.pick("a reply is already in progress", "返信を作成中です")
}

pub fn err_assistant_unavailable(l: Lang) -> String {
    l.pick("the assistant is unavailable; try again", "アシスタントが応答できません。もう一度どうぞ")
}

pub fn err_share_rate(l: Lang) -> String {
    l.pick(
        "too many messages from this address; try again later",
        "このアドレスからのメッセージが多すぎます。しばらくしてからどうぞ",
    )
}

pub fn err_share_no_thread(l: Lang) -> String {
    l.pick("no such conversation", "その会話は見つかりません")
}

pub fn err_share_cap(l: Lang) -> String {
    l.pick("this link has reached today's message limit", "このリンクは今日のメッセージ上限に達しました")
}

pub fn err_share_unavailable(l: Lang) -> String {
    l.pick("Note could not answer", "Noteが応答できませんでした")
}

pub fn call_title(l: Lang) -> String {
    l.pick("Call", "通話")
}

/// Spoken when a reply failed; the caller is asked to repeat.
pub fn call_apology(l: Lang) -> String {
    l.pick(
        "Sorry, I lost my train of thought. Could you say that again?",
        "すみません、考えがまとまりませんでした。もう一度言ってもらえますか？",
    )
}

/// Spoken before a call that keeps failing hangs up.
pub fn call_bow_out(l: Lang) -> String {
    l.pick(
        "I'm having trouble thinking right now. I'll message you instead.",
        "今うまく考えられないので、メッセージで連絡しますね。",
    )
}

/// The model's cue when the user rang in with nothing queued.
pub fn call_greet(l: Lang) -> String {
    l.pick("the user called you; greet them briefly", "the user called you; greet them briefly in Japanese")
}

pub fn session_thread_title(l: Lang, task: &str) -> String {
    match l {
        Lang::En => format!("Session: {task}"),
        Lang::Ja => format!("セッション：{task}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_wins_and_auto_follows_the_browser() {
        assert_eq!(Lang::resolve("ja", Some("en-US")), Lang::Ja);
        assert_eq!(Lang::resolve("en", Some("ja")), Lang::En);
        assert_eq!(Lang::resolve("", Some("ja-JP,ja;q=0.9,en;q=0.8")), Lang::Ja);
        assert_eq!(Lang::resolve("", Some("en-GB,ja;q=0.5")), Lang::En);
        assert_eq!(Lang::resolve("", Some("fr, ja;q=0.3")), Lang::Ja);
        assert_eq!(Lang::resolve("", Some("ja;q=0")), Lang::En);
        assert_eq!(Lang::resolve("", None), Lang::En);
    }

    fn user_dir(tmp: &tempfile::TempDir, language: &str) {
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            "display_name = \"Aki\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("users/aki")).unwrap();
        std::fs::write(tmp.path().join("users/aki/user.toml"), format!("language = \"{language}\"\n")).unwrap();
    }

    #[test]
    fn auto_without_a_request_follows_the_browser_last_seen() {
        let tmp = tempfile::tempdir().unwrap();
        user_dir(&tmp, "");
        assert_eq!(Lang::for_user(tmp.path(), "aki"), Lang::En);

        let seen_by = SeenLangs::default();
        seen_by.note(tmp.path(), 1, "aki", "ja-JP,ja;q=0.9,en;q=0.8");
        assert_eq!(Lang::for_user(tmp.path(), "aki"), Lang::Ja);
        assert_eq!(Lang::for_request(tmp.path(), "aki", None), Lang::Ja);
        assert_eq!(Lang::for_request(tmp.path(), "aki", Some("en-US")), Lang::En);

        seen_by.note(tmp.path(), 1, "aki", "fr-FR");
        assert_eq!(Lang::for_user(tmp.path(), "aki"), Lang::En, "an unspoken language reads as English");
        seen_by.note(tmp.path(), 1, "aki", "  ");
        assert_eq!(seen(tmp.path(), "aki"), Some(Lang::En), "a blank header changes nothing");
    }

    #[test]
    fn a_setting_outranks_the_browser_last_seen() {
        let tmp = tempfile::tempdir().unwrap();
        user_dir(&tmp, "en");
        remember_seen(tmp.path(), "aki", Lang::Ja).unwrap();
        assert_eq!(Lang::for_user(tmp.path(), "aki"), Lang::En);
        assert_eq!(Lang::for_request(tmp.path(), "aki", Some("ja")), Lang::En);
    }

    #[test]
    fn the_file_is_written_only_when_the_language_changes() {
        let tmp = tempfile::tempdir().unwrap();
        user_dir(&tmp, "");
        let seen_by = SeenLangs::default();
        seen_by.note(tmp.path(), 1, "aki", "ja");
        std::fs::remove_file(tmp.path().join("users/aki/seen_language")).unwrap();
        seen_by.note(tmp.path(), 1, "aki", "ja");
        assert_eq!(seen(tmp.path(), "aki"), None, "a repeat of the cached language is not rewritten");
        seen_by.note(tmp.path(), 1, "aki", "en");
        assert_eq!(seen(tmp.path(), "aki"), Some(Lang::En));
    }

    #[test]
    fn a_check_in_thread_is_named_by_its_weekday() {
        let day: jiff::civil::Date = "2026-10-05".parse().unwrap();
        assert_eq!(checkin_thread_title(Lang::En, Some(day), "09:00"), "Monday's 09:00 check-in");
        assert_eq!(checkin_thread_title(Lang::Ja, Some(day), "09:00"), "月曜の09:00のチェックイン");
        assert_eq!(checkin_thread_title(Lang::Ja, None, "09:00"), "09:00のチェックイン");
    }
}
