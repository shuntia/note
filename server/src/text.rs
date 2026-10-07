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

pub fn fallback_debrief(l: Lang) -> String {
    l.pick(
        "(Plan generated from your template. The assistant was unavailable overnight.)",
        "（テンプレートから作った計画です。夜のあいだアシスタントが使えませんでした。）",
    )
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

/// Why a request failed, written: one short line with the fix where there is one.
pub fn failure_reason(l: Lang, reason: crate::failure::Reason) -> String {
    use crate::failure::Reason as R;
    match reason {
        R::ModelUnavailable => l.pick(
            "The AI model isn't available. Check the model setting.",
            "AIモデルが見つかりません。モデルの設定を確認してください。",
        ),
        R::AuthInvalid => l.pick(
            "The AI service rejected the API key. Check the key.",
            "AIサービスがAPIキーを受け付けません。キーを確認してください。",
        ),
        R::OutOfCredits => l.pick(
            "The AI service is out of credits or over its spend limit.",
            "AIサービスのクレジットが足りないか、利用上限に達しています。",
        ),
        R::RateLimited => l.pick(
            "The AI service is getting too many requests. Try again in a minute.",
            "AIサービスへのリクエストが多すぎます。少し待ってからもう一度どうぞ。",
        ),
        R::ProviderDown => l.pick(
            "The AI service isn't responding. Try again shortly.",
            "AIサービスが応答していません。しばらくしてからもう一度どうぞ。",
        ),
        R::ContextTooLong => l.pick(
            "This conversation is too long for the model. Start a new one.",
            "会話が長すぎてモデルが読み切れません。新しい会話を始めてください。",
        ),
        R::Refused => l.pick("The AI service declined this request.", "AIサービスがこのリクエストを断りました。"),
        R::Internal => l.pick("Something went wrong inside Note.", "Noteの内部でエラーが起きました。"),
    }
}

/// Why a reply failed, spoken in a call: one sentence.
pub fn failure_spoken(l: Lang, reason: crate::failure::Reason) -> String {
    use crate::failure::Reason as R;
    match reason {
        R::ModelUnavailable => l.pick(
            "Sorry, the AI model isn't available right now; check the model setting.",
            "すみません、AIモデルが見つかりません。設定を確認してください。",
        ),
        R::AuthInvalid => l.pick(
            "Sorry, the AI service won't accept my key; check the API key.",
            "すみません、AIサービスにキーが通りません。APIキーを確認してください。",
        ),
        R::OutOfCredits => l.pick(
            "Sorry, the AI service has run out of credits.",
            "すみません、AIサービスのクレジットが切れているみたいです。",
        ),
        R::RateLimited => l.pick(
            "Sorry, the AI service is swamped; could you say that again in a moment?",
            "すみません、AIサービスが混み合っています。少ししてからもう一度言ってもらえますか？",
        ),
        R::ProviderDown => l.pick(
            "Sorry, the AI service isn't answering; could you say that again in a moment?",
            "すみません、AIサービスから返事がありません。少ししてからもう一度言ってもらえますか？",
        ),
        R::ContextTooLong => l.pick(
            "Sorry, this call has grown too long for me to follow; please call again.",
            "すみません、話が長くなって追いきれなくなりました。かけ直してもらえますか？",
        ),
        R::Refused => l.pick(
            "Sorry, the AI service won't answer that one.",
            "すみません、その話にはAIサービスが答えてくれませんでした。",
        ),
        R::Internal => call_apology(l),
    }
}

/// The title of a message telling the user that work in the background failed.
pub fn failure_title(l: Lang) -> String {
    l.pick("Note ran into a problem", "Noteで問題が起きました")
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

pub fn err_share_rate(l: Lang) -> String {
    l.pick(
        "too many messages from this address; try again later",
        "このアドレスからのメッセージが多すぎます。しばらくしてからどうぞ",
    )
}

pub fn err_invite_gone(l: Lang) -> String {
    l.pick("this invite is no longer open", "この招待はもう使えません")
}

pub fn err_invite_rate(l: Lang) -> String {
    l.pick(
        "too many tries from this address; try again later",
        "このアドレスからの試行が多すぎます。しばらくしてからどうぞ",
    )
}

pub fn err_join_taken(l: Lang) -> String {
    l.pick("that username is taken", "そのユーザー名は使われています")
}

pub fn err_join_username(l: Lang) -> String {
    l.pick(
        "a username is up to 64 letters, digits, - or _",
        "ユーザー名は英数字と - _ で64文字まで",
    )
}

pub fn err_join_password(l: Lang, min: usize) -> String {
    match l {
        Lang::En => format!("the password must be at least {min} characters"),
        Lang::Ja => format!("パスワードは{min}文字以上にしてください"),
    }
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

/// A reply cut off by the session's step limit.
pub fn max_turns_reply(l: Lang) -> String {
    l.pick(
        "(I ran out of steps before finishing — ask again and I'll pick it up from here)",
        "（終わる前に手順が尽きました。もう一度聞いてもらえれば、ここから続けます）",
    )
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

pub fn session_thread_title(l: Lang, task: &str) -> String {
    match l {
        Lang::En => format!("Session: {task}"),
        Lang::Ja => format!("セッション：{task}"),
    }
}

/// An error line some module wrote in English, in `l`. Lines with no
/// Japanese here pass through as written.
pub fn error_line(l: Lang, english: &str) -> String {
    if l == Lang::En {
        return english.to_string();
    }
    ERRORS_JA
        .iter()
        .find_map(|(pattern, ja)| {
            let caught = captures(pattern, english)?;
            let mut out = (*ja).to_string();
            for (i, c) in caught.iter().enumerate() {
                out = out.replace(&format!("{{{i}}}"), field_ja(c));
            }
            Some(out)
        })
        .unwrap_or_else(|| english.to_string())
}

/// The runs `{}` stands for when `line` fits `pattern`, matched leftmost.
fn captures<'a>(pattern: &str, line: &'a str) -> Option<Vec<&'a str>> {
    let literals: Vec<&str> = pattern.split("{}").collect();
    let (first, rest) = literals.split_first()?;
    let mut at = line.strip_prefix(first).map(|_| first.len())?;
    let mut caught = Vec::new();
    for (i, lit) in rest.iter().enumerate() {
        let start = if i + 1 == rest.len() {
            let end = line.len().checked_sub(lit.len())?;
            (end >= at && line.get(end..) == Some(*lit)).then_some(end)?
        } else if lit.is_empty() {
            return None;
        } else {
            at + line[at..].find(lit)?
        };
        caught.push(&line[at..start]);
        at = start + lit.len();
    }
    Some(caught)
}

fn field_ja(field: &str) -> &str {
    FIELDS_JA.iter().find(|(en, _)| *en == field).map_or(field, |(_, ja)| ja)
}

const FIELDS_JA: &[(&str, &str)] = &[
    ("alerts", "通知"),
    ("before", "日時"),
    ("brief", "指示"),
    ("category", "カテゴリ"),
    ("close_day_time", "一日の締めの時刻"),
    ("content", "内容"),
    ("context", "コンテキスト"),
    ("counter", "カウンター"),
    ("date", "日付"),
    ("days", "曜日"),
    ("description", "説明"),
    ("display_name", "表示名"),
    ("due_at", "期限"),
    ("duration_min", "所要時間"),
    ("end", "終了時刻"),
    ("expires_at", "期限"),
    ("external_id", "外部ID"),
    ("from_date", "開始日"),
    ("horizon_days", "公開する日数"),
    ("idle_nudge_min", "放置時のお知らせ"),
    ("kind", "種類"),
    ("language", "言語"),
    ("limit", "件数"),
    ("message", "メッセージ"),
    ("messages_per_day", "1日のメッセージ数"),
    ("model", "モデル"),
    ("morning_until", "朝の終わり"),
    ("mxid", "Matrix ID"),
    ("name", "名前"),
    ("nightly_time", "夜の処理の時刻"),
    ("notes", "メモ"),
    ("notify", "通知"),
    ("on_date", "日付"),
    ("outcome", "結果"),
    ("password", "パスワード"),
    ("planned_min", "予定時間"),
    ("pomodoro_break_min", "ポモドーロの休憩時間"),
    ("pomodoro_work_min", "ポモドーロの作業時間"),
    ("progress", "進捗"),
    ("ring_for", "着信"),
    ("role", "役割"),
    ("source", "ソース"),
    ("start", "開始時刻"),
    ("state", "状態"),
    ("step_count", "ステップ数"),
    ("step_index", "ステップ番号"),
    ("step_name", "ステップ名"),
    ("template", "テンプレート"),
    ("text", "テキスト"),
    ("timezone", "タイムゾーン"),
    ("title", "タイトル"),
    ("triggers_per_day", "1日のトリガー数"),
    ("until_date", "終了日"),
    ("urgency", "緊急度"),
    ("url", "URL"),
    ("username", "ユーザー名"),
    ("voice", "声"),
    ("voice_voice", "声"),
];

/// English error lines and their Japanese, most specific first. `{}` in the
/// English matches any run of text, put back as `{0}`, `{1}`… in order; a
/// field name caught is given its Japanese label.
const ERRORS_JA: &[(&str, &str)] = &[
    ("the new password must be at least {} characters", "新しいパスワードは{0}文字以上にしてください"),
    ("source_id must be 1 to {} characters of A-Za-z0-9:._-", "ソースIDは英数字と :._- で1〜{0}文字にしてください"),
    ("text must be one line of 1..={} characters", "テキストは1行・{0}文字以内にしてください"),
    ("duration_min must be a multiple of {}, from {} to {}", "所要時間は{0}分刻みで{1}〜{2}分にしてください"),
    ("snooze minutes must be in 1..=1440, got {}", "スヌーズは1〜1440分にしてください（入力：{0}）"),
    ("days must be a bitmask in 0..=127, Mon = 1 … Sun = 64", "曜日の指定が正しくありません"),
    ("kind must be \"announcement\" or \"material\"", "種類は announcement か material にしてください"),
    ("{} must be non-blank and at most {} characters", "{0}は空にせず{1}文字以内にしてください"),
    ("{} must be non-blank and at most {} bytes", "{0}は空にせず{1}バイト以内にしてください"),
    ("{} must be 1 to {} characters", "{0}は1〜{1}文字にしてください"),
    ("{} must be 1..={} characters", "{0}は1〜{1}文字にしてください"),
    ("{} must be 1 to {} bytes", "{0}は1〜{1}バイトにしてください"),
    ("{} must be 1..={} bytes", "{0}は1〜{1}バイトにしてください"),
    ("{} must be at most {} characters", "{0}は{1}文字以内にしてください"),
    ("{} must be at most {} bytes", "{0}は{1}バイト以内にしてください"),
    ("{} must be a zero-padded 24-hour HH:MM, or blank for no close of day", "{0}は24時間表記のHH:MM（例 21:30）か、締めなしなら空欄にしてください"),
    ("{} must be a zero-padded 24-hour HH:MM", "{0}は24時間表記のHH:MM（例 07:30）にしてください"),
    ("{} must be zero-padded HH:MM, got {}", "{0}はHH:MM（例 07:30）にしてください（入力：{1}）"),
    ("{} must be YYYY-MM-DD, got {}", "{0}はYYYY-MM-DD形式にしてください（入力：{1}）"),
    ("{} must be YYYY-MM-DD", "{0}はYYYY-MM-DD形式にしてください"),
    ("{} must be an RFC 3339 instant, got {}", "{0}は日時（RFC 3339）にしてください（入力：{1}）"),
    ("{} must be one of {}, got {}", "{0}は次のいずれかにしてください：{1}（入力：{2}）"),
    ("{} must be one of {}", "{0}は次のいずれかにしてください：{1}"),
    ("{} must be remaining or elapsed", "{0}は remaining か elapsed にしてください"),
    ("{} must be urgent, checkins or never", "{0}は urgent、checkins、never のいずれかにしてください"),
    ("{} must be blank, en or ja", "{0}は空欄、en、ja のいずれかにしてください"),
    ("{} must be admin or member", "{0}は admin か member にしてください"),
    ("{} must be done or stopped", "{0}は done か stopped にしてください"),
    ("{} must look like @name:server", "{0}は @name:server の形にしてください"),
    ("{} must not be negative", "{0}は0以上にしてください"),
    ("{} must not be empty", "{0}を入力してください"),
    ("{} must be a number", "{0}は数値にしてください"),
    ("{} must be a timestamp", "{0}は日時にしてください"),
    ("{} must be in the future", "{0}は未来の日時にしてください"),
    ("{} must be a non-empty name without spaces", "{0}は空白を含まない名前にしてください"),
    ("{} must be 0 to {}", "{0}は0〜{1}にしてください"),
    ("{} must be 1 to {}", "{0}は1〜{1}にしてください"),
    ("{} must be {} to {}", "{0}は{1}〜{2}にしてください"),
    ("{} is not a known IANA timezone", "{0}が不明なタイムゾーンです"),
    ("{} is not one of the available templates", "そのテンプレートはありません"),
    ("{} is not one of the available voices", "その声は選べません"),
    ("{} has no voices", "この言語で使える声がありません"),
    ("Now already holds {} tasks", "「今」にはすでに{0}件のタスクがあります"),
    ("external_id {} already belongs to task {}", "外部ID {0} はすでにタスク {1} に使われています"),
    ("external_id {} already belongs to calendar entry {}", "外部ID {0} はすでに予定 {1} に使われています"),
    ("external_id belongs to the path, not the body", "外部IDは本文ではなくパスに含めてください"),
    ("a calendar holds at most {} entries", "カレンダーに入れられる予定は{0}件までです"),
    ("no calendar entry {}", "予定 {0} はありません"),
    ("no such calendar entry", "その予定はありません"),
    ("an entry must end after it starts, got {}", "終わりは始まりより後にしてください（入力：{0}）"),
    ("an entry with no days needs on_date, the one day it happens", "曜日を決めない予定には日付が必要です"),
    ("a recurring entry has days or on_date, not both", "曜日か日付のどちらか一方を指定してください"),
    ("an entry recurs on days or happens on one date, not both", "曜日か日付のどちらか一方を指定してください"),
    ("from_date and until_date bound a recurring entry, not a one-off", "開始日と終了日は繰り返しの予定にだけ指定できます"),
    ("from_date {} is after until_date {}", "開始日 {0} が終了日 {1} より後になっています"),
    ("a step carries no due date of its own; the task it belongs to holds it", "ステップには期限を付けられません。親のタスクに付けてください"),
    ("a step carries no category of its own; it reads the one on the task it belongs to", "ステップにはカテゴリを付けられません。親のタスクのものが使われます"),
    ("a step belongs to no goal of its own; the task it belongs to holds one", "ステップは目標につなげられません。親のタスクにつなげてください"),
    ("a task cannot be its own step", "タスクを自分自身のステップにはできません"),
    ("steps are one level deep: a step cannot have steps of its own", "ステップは一段だけです。ステップの下にステップは作れません"),
    ("steps are one level deep: a task with steps cannot become a step", "ステップは一段だけです。ステップのあるタスクはステップにできません"),
    ("a step reads its parent's urgency", "ステップの緊急度は親のタスクに従います"),
    ("a split needs {} to {} steps", "分けるには{0}〜{1}個のステップが必要です"),
    ("this task already has steps", "このタスクにはすでにステップがあります"),
    ("invalid state: {}", "状態が正しくありません：{0}"),
    ("event already {}", "この予定はもう済んでいます"),
    ("no goal {}", "目標 {0} はありません"),
    ("no task {}", "タスク {0} はありません"),
    ("no event {}", "予定 {0} はありません"),
    ("at most {} categories", "カテゴリは{0}個までです"),
    ("at most {} tokens per user", "トークンは1人{0}個までです"),
    ("at most {} passkeys per user", "パスキーは1人{0}個までです"),
    ("at most {} devices per account", "端末は1アカウント{0}台までです"),
    ("memory {} is archived and immutable", "記憶 {0} はアーカイブ済みで変更できません"),
    ("template {} has no events", "テンプレート {0} には予定がありません"),
    ("template entry {} is not a table", "テンプレートの項目 {0} の形式が正しくありません"),
    ("template {}: invalid {} {}", "テンプレート {0}：{1} の値 {2} が正しくありません"),
    ("entry {} is a block, and blocks never ping","項目 {0} はブロックなので通知しません"),
    ("task not found", "タスクが見つかりません"),
    ("conversation not found", "会話が見つかりません"),
    ("user not found", "ユーザーが見つかりません"),
    ("memory not found", "記憶が見つかりません"),
    ("trace not found", "トレースが見つかりません"),
    ("no such passkey", "そのパスキーはありません"),
    ("unknown category", "不明なカテゴリです"),
    ("database error", "データベースのエラーです"),
    ("unreadable user config", "ユーザー設定を読み込めません"),
    ("malformed JSON body", "JSONの形式が正しくありません"),
    ("a token cannot write source manual", "トークンからは手動のタスクを書き込めません"),
    ("the user deleted this task; it is not recreated", "このタスクは削除されたため、作り直しません"),
    ("the task could not be briefed", "タスクの説明を作れませんでした"),
    ("only a top-level task can be briefed, not one of its steps", "説明を作れるのは親のタスクだけです"),
    ("the item could not be read", "項目を読み取れませんでした"),
    ("the assistant is unavailable; try again", "アシスタントが応答できません。もう一度どうぞ"),
    ("the assistant reached no decision; try again", "アシスタントが判断できませんでした。もう一度どうぞ"),
    ("a day that is over cannot be filled", "終わった日は埋められません"),
    ("a day that is over cannot be carried", "終わった日は持ち越せません"),
    ("that endpoint belongs to another account", "その通知先は別のアカウントのものです"),
    ("endpoint must be an https:// URL with a plain host", "通知先は https:// のURLにしてください"),
    ("endpoint must not point at this server", "通知先にこのサーバーは使えません"),
    ("endpoint must not point into a private network", "通知先にプライベートネットワークは使えません"),
    ("endpoint host does not resolve", "通知先のホストが見つかりません"),
    ("too many open connections", "接続が多すぎます"),
    ("the link was removed", "リンクは削除されました"),
    ("share links are off, or the cap is reached", "共有リンクが無効か、上限に達しています"),
    ("refresh is not configured", "更新は設定されていません"),
    ("refresh is unavailable", "今は更新できません"),
    ("that challenge has expired", "時間切れです。もう一度お試しください"),
    ("that passkey could not be verified", "パスキーを確認できませんでした"),
    ("that passkey is already registered", "そのパスキーはすでに登録されています"),
    ("that code doesn't match", "コードが一致しません"),
    ("wrong password or code", "パスワードかコードが違います"),
    ("wrong password", "パスワードが違います"),
    ("too many attempts", "試行回数が多すぎます。しばらくしてからどうぞ"),
    ("too many sign-ins in flight; try again", "サインインが混み合っています。もう一度どうぞ"),
    ("elevation required", "管理者の再認証が必要です"),
    ("admin only", "管理者専用です"),
    ("cross-site request refused", "別のサイトからのリクエストは受け付けません"),
    ("no passkeys on this account", "このアカウントにはパスキーがありません"),
    ("no second factor on this account and no admin secret on this server", "このアカウントには二段階認証がなく、サーバーにも管理者用の秘密がありません"),
    ("username is taken", "そのユーザー名は使われています"),
    ("you can't change your own role or disable yourself", "自分の役割の変更や無効化はできません"),
    ("that would leave no enabled admin", "有効な管理者がいなくなってしまいます"),
    ("you can't reset another admin's password", "ほかの管理者のパスワードはリセットできません"),
    ("the configured chat provider has no model to change", "このチャットプロバイダーには変更できるモデルがありません"),
];

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
    fn error_lines_read_in_japanese_with_their_fields_named() {
        let ja = |line: &str| error_line(Lang::Ja, line);
        assert_eq!(ja("Now already holds 3 tasks"), "「今」にはすでに3件のタスクがあります");
        assert_eq!(ja("message must be non-blank and at most 16384 bytes"), "メッセージは空にせず16384バイト以内にしてください");
        assert_eq!(ja("nightly_time must be a zero-padded 24-hour HH:MM"), "夜の処理の時刻は24時間表記のHH:MM（例 07:30）にしてください");
        assert_eq!(ja("the new password must be at least 8 characters"), "新しいパスワードは8文字以上にしてください");
        assert_eq!(ja("name must be 1 to 64 characters"), "名前は1〜64文字にしてください");
        assert_eq!(ja("pomodoro_work_min must be 5 to 90"), "ポモドーロの作業時間は5〜90にしてください");
        assert_eq!(ja("external_id x already belongs to task 4"), "外部ID x はすでにタスク 4 に使われています");
        assert_eq!(ja("wrong password"), "パスワードが違います");
        assert_eq!(ja("something new and unknown"), "something new and unknown");
        assert_eq!(ja("AIモデルが見つかりません。"), "AIモデルが見つかりません。", "a line already in Japanese passes through");
        assert_eq!(error_line(Lang::En, "wrong password"), "wrong password");
    }

    #[test]
    fn every_error_pattern_is_reachable_and_fills_every_slot() {
        for (i, (en, ja)) in ERRORS_JA.iter().enumerate() {
            let slots = en.matches("{}").count();
            let sample = en.replace("{}", "Z");
            let line = error_line(Lang::Ja, &sample);
            let first = ERRORS_JA.iter().position(|(p, _)| captures(p, &sample).is_some());
            assert_eq!(first, Some(i), "{en:?} is shadowed by an earlier pattern");
            assert!(!line.contains('{'), "{en:?} -> {line:?}");
            assert!(ja.matches('{').count() <= slots, "{ja:?} names a slot {en:?} lacks");
        }
    }

    #[test]
    fn call_lines_are_spoken_in_the_call_language() {
        assert_eq!(call_title(Lang::Ja), "通話");
        assert!(call_apology(Lang::Ja).ends_with('？'));
        assert!(call_bow_out(Lang::En).starts_with("I'm having trouble"));
    }

    #[test]
    fn a_check_in_thread_is_named_by_its_weekday() {
        let day: jiff::civil::Date = "2026-10-05".parse().unwrap();
        assert_eq!(checkin_thread_title(Lang::En, Some(day), "09:00"), "Monday's 09:00 check-in");
        assert_eq!(checkin_thread_title(Lang::Ja, Some(day), "09:00"), "月曜の09:00のチェックイン");
        assert_eq!(checkin_thread_title(Lang::Ja, None, "09:00"), "09:00のチェックイン");
    }
}
