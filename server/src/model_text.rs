//! Every line the server writes for the model to read, in each language it
//! speaks. Tool names, JSON keys and stored enum values stay as they are.

use crate::text::Lang;

impl Lang {
    const fn of(self, en: &'static str, ja: &'static str) -> &'static str {
        match self {
            Lang::En => en,
            Lang::Ja => ja,
        }
    }
}

fn plural(n: impl Into<i64>) -> &'static str {
    if n.into() == 1 { "" } else { "s" }
}

pub const REPLY_IN_JAPANESE: &str =
    "ユーザーは日本語で読みます。返信、メモ、タイトル、要約はすべて自然な日本語で書いてください。";

pub const SPEAK_JAPANESE: &str = "ユーザーとは日本語で話します。上の指示が何語で書かれていても、自然な話し言葉の日本語で答えてください。\
    文は短く。マークダウン、絵文字、箇条書きは使いません。数字や時刻、日付は口に出すときの言い方で（七時半、二十分くらい、来週の金曜）。\
    外来語やラテン文字の名前はカタカナで書きます（ギットハブ、ズーム）。数字は算用数字のままでかまいません。";

pub const VISITOR_JAPANESE: &str = "相手は日本語で読みます。返信はすべて自然な日本語で書いてください。";

/// Leads every call's prompt, ahead of everything else.
pub fn call_briefly(l: Lang) -> &'static str {
    l.of(
        "On this call, keep every spoken reply to one to three short sentences. \
         Answer first. No lists, no markdown, no recaps of what was said. Ask at most one question at a time. \
         Offer more detail only when asked.",
        "この通話では、話す返事はどれも短い文で一〜三文にとどめてください。まず答えから。\
         箇条書き、マークダウン、話したことのまとめは不要です。質問は一度にひとつまで。\
         詳しい説明は聞かれたときだけにします。",
    )
}

/// Closes every call's prompt: the language the caller was heard speaking, over their setting.
pub fn call_speaks_only(l: Lang) -> &'static str {
    l.of(
        "The caller is speaking English on this call. Reply only in English, whatever language the rest of \
         these instructions, the notes or the thread are in.",
        "この通話で相手は日本語を話しています。ほかの指示やメモ、スレッドが何語で書かれていても、返事は日本語だけで話してください。",
    )
}

pub fn share_from(l: Lang, owner: &str) -> String {
    match l {
        Lang::En => format!("# From {owner}"),
        Lang::Ja => format!("# {owner}さんから"),
    }
}

pub fn share_note_line(l: Lang, owner: &str) -> String {
    match l {
        Lang::En => format!(
            "A message the visitor wants passed on to {owner} is filed with share_note, which ends the turn; the visitor is told it was passed on."
        ),
        Lang::Ja => format!(
            "相手が{owner}さんに伝えてほしいメッセージは share_note で残します。これでターンが終わり、相手には伝えたことが知らされます。"
        ),
    }
}

// --- the context block ---

pub fn timezone_invalid(l: Lang) -> &'static str {
    l.of("UTC (configured timezone invalid)", "UTC（設定のタイムゾーンが無効）")
}

pub fn no_standing(l: Lang) -> &'static str {
    l.of("(no standing context yet)", "（常設コンテキストはまだありません）")
}

pub fn standing_heading(l: Lang) -> &'static str {
    l.of("# Standing context", "# 常設コンテキスト")
}

pub fn part_of_day(l: Lang, hour: i8) -> &'static str {
    match hour {
        5..=7 => l.of("early morning", "早朝"),
        8..=10 => l.of("morning", "朝"),
        11..=13 => l.of("midday", "昼"),
        14..=17 => l.of("afternoon", "午後"),
        18..=21 => l.of("evening", "夕方"),
        _ => l.of("night", "夜"),
    }
}

pub fn in_words(l: Lang, mins: i64) -> String {
    match (l, mins < 60) {
        (Lang::En, true) => format!("{mins} min"),
        (Lang::En, false) => format!("{}h{:02}m", mins / 60, mins % 60),
        (Lang::Ja, true) => format!("{mins}分"),
        (Lang::Ja, false) => format!("{}時間{:02}分", mins / 60, mins % 60),
    }
}

/// The clock line's local date and time.
pub fn now_stamp(l: Lang, local: &jiff::Zoned) -> String {
    match l {
        Lang::En => local.strftime("%A %Y-%m-%d %H:%M").to_string(),
        Lang::Ja => format!(
            "{}（{}） {}",
            local.strftime("%Y-%m-%d"),
            weekday(l, local.weekday()),
            local.strftime("%H:%M")
        ),
    }
}

/// A weekday's short name, as the week's digest dates its rows.
pub fn weekday(l: Lang, day: jiff::civil::Weekday) -> &'static str {
    use jiff::civil::Weekday as W;
    match day {
        W::Monday => l.of("Mon", "月"),
        W::Tuesday => l.of("Tue", "火"),
        W::Wednesday => l.of("Wed", "水"),
        W::Thursday => l.of("Thu", "木"),
        W::Friday => l.of("Fri", "金"),
        W::Saturday => l.of("Sat", "土"),
        W::Sunday => l.of("Sun", "日"),
    }
}

pub fn now_heading(l: Lang) -> &'static str {
    l.of("# Now\n\n", "# 現在\n\n")
}

pub fn no_day_plan(l: Lang) -> &'static str {
    l.of("Day's plan: none generated for today\n", "今日の予定: まだ作られていません\n")
}

pub fn day_plan(l: Lang, first: &str, last: &str, now: &str, left: usize) -> String {
    match l {
        Lang::En => format!(
            "Day's plan: {first}-{last}; now {now}, {left} event{} left",
            plural(left as i64)
        ),
        Lang::Ja => format!("今日の予定: {first}-{last}、現在 {now}、残り{left}件"),
    }
}

pub fn nightly_run_in(l: Lang, at: &str, until: &str) -> String {
    match l {
        Lang::En => format!("Nightly run {at}, in {until}"),
        Lang::Ja => format!("夜の処理 {at}（あと{until}）"),
    }
}

pub fn quiet_until(l: Lang, end: &str, title: &str) -> String {
    match l {
        Lang::En => format!("Quiet until {end} ({title})"),
        Lang::Ja => format!("{end}まで静かに（{title}）"),
    }
}

pub fn calendar_label(l: Lang) -> &'static str {
    l.of("Calendar:\n", "カレンダー:\n")
}

pub fn quiet_mark(l: Lang) -> &'static str {
    l.of("quiet", "静か")
}

pub fn more_in_parens(l: Lang, rest: usize) -> String {
    match l {
        Lang::En => format!("(+{rest} more)"),
        Lang::Ja => format!("（ほか{rest}件）"),
    }
}

pub fn plan_heading(l: Lang) -> &'static str {
    l.of("# Today's plan\n\n", "# 今日の予定\n\n")
}

pub fn no_plan_today(l: Lang) -> &'static str {
    l.of("(no plan generated for today)\n", "（今日の予定はまだ作られていません）\n")
}

pub fn mark_now(l: Lang) -> &'static str {
    l.of(" <- now", " <- 今")
}

pub fn mark_next(l: Lang, after: &str) -> String {
    match l {
        Lang::En => format!(" <- next, in {after}"),
        Lang::Ja => format!(" <- 次、あと{after}"),
    }
}

pub fn block_word(l: Lang) -> &'static str {
    l.of("block", "ブロック")
}

pub fn routine_via(l: Lang, channel: &str) -> String {
    match l {
        Lang::En => format!("routine via {channel}"),
        Lang::Ja => format!("ルーティン（{channel}）"),
    }
}

pub fn silent(l: Lang) -> &'static str {
    l.of(" (silent)", "（無音）")
}

pub fn status_counts(l: Lang, n: [usize; 5]) -> String {
    let [pending, fired, done, dropped, snoozed] = n;
    match l {
        Lang::En => format!(
            "{pending} pending, {fired} fired, {done} done, {dropped} dropped, {snoozed} snoozed"
        ),
        Lang::Ja => format!(
            "pending {pending}件、fired {fired}件、done {done}件、dropped {dropped}件、snoozed {snoozed}件"
        ),
    }
}

pub fn tomorrow_plan(l: Lang, tomorrow: jiff::civil::Date, generated: bool) -> String {
    match l {
        Lang::En => format!(
            "Tomorrow's plan ({tomorrow}): {}\n\n",
            if generated { "generated" } else { "not generated yet" }
        ),
        Lang::Ja => format!(
            "明日の予定（{tomorrow}）: {}\n\n",
            if generated { "作成済み" } else { "未作成" }
        ),
    }
}

pub fn minutes_short(l: Lang, min: u32) -> String {
    match l {
        Lang::En => format!(" {min}m"),
        Lang::Ja => format!(" {min}分"),
    }
}

pub fn progress(l: Lang, percent: u32, left: Option<i64>) -> String {
    match (l, left) {
        (Lang::En, Some(left)) => format!(" {percent}% ~{left}m left"),
        (Lang::Ja, Some(left)) => format!(" {percent}% 残り約{left}分"),
        (_, None) => format!(" {percent}%"),
    }
}

pub fn overdue(l: Lang) -> &'static str {
    l.of(" overdue", " 期限切れ")
}

pub fn due_today(l: Lang) -> &'static str {
    l.of(" due today", " 今日まで")
}

pub fn due_tomorrow(l: Lang) -> &'static str {
    l.of(" due tomorrow", " 明日まで")
}

pub fn due_on(l: Lang, day: jiff::civil::Date) -> String {
    match l {
        Lang::En => format!(" due {day}"),
        Lang::Ja => format!(" {day}まで"),
    }
}

pub fn tasks_heading(l: Lang) -> &'static str {
    l.of("# Tasks\n\n", "# タスク\n\n")
}

pub fn order_heading(l: Lang) -> &'static str {
    l.of("# Today's order\n\n", "# 今日の順番\n\n")
}

pub fn order_empty(l: Lang) -> &'static str {
    l.of("(empty — Now falls back to the queue)\n\n", "（空 — 「今すぐ」はキューから選ぶ）\n\n")
}

pub fn no_tasks(l: Lang) -> &'static str {
    l.of("(no tasks)\n\n", "（タスクなし）\n\n")
}

pub fn now_none(l: Lang) -> &'static str {
    l.of("Now: (none)\n", "今すぐ: （なし）\n")
}

pub fn now_label(l: Lang) -> &'static str {
    l.of("Now:\n", "今すぐ:\n")
}

pub fn later_none(l: Lang) -> &'static str {
    l.of("Later: (none)\n", "あとで: （なし）\n")
}

pub fn later_trimmed(l: Lang, open: usize) -> String {
    match l {
        Lang::En => format!("Later: {open} open (titles trimmed for size)"),
        Lang::Ja => format!("あとで: 未完了{open}件（長さのため件名は省略）"),
    }
}

pub fn later_label(l: Lang, open: usize) -> String {
    match l {
        Lang::En => format!("Later ({open} open):"),
        Lang::Ja => format!("あとで（未完了{open}件）:"),
    }
}

pub fn and_more(l: Lang, rest: usize) -> String {
    match l {
        Lang::En => format!("- ... and {rest} more"),
        Lang::Ja => format!("- ……ほか{rest}件"),
    }
}

pub fn due_soon(l: Lang, soon: usize, days: i64, overdue: usize) -> String {
    match l {
        Lang::En => format!("Due soon: {soon} in the next {days} days, {overdue} overdue"),
        Lang::Ja => format!("期限間近: {days}日以内に{soon}件、期限切れ{overdue}件"),
    }
}

pub fn done_today(l: Lang, n: i64) -> String {
    match l {
        Lang::En => format!("Done today: {n}\n\n"),
        Lang::Ja => format!("今日の完了: {n}件\n\n"),
    }
}

pub fn days_ago(l: Lang, days: Option<i32>) -> String {
    match days {
        None => l.of("date unreadable", "日付不明").into(),
        Some(0) => l.of("today", "今日").into(),
        Some(1) => l.of("yesterday", "昨日").into(),
        Some(n) => match l {
            Lang::En => format!("{n} days ago"),
            Lang::Ja => format!("{n}日前"),
        },
    }
}

pub fn notes_heading(l: Lang, stale: bool, date: &str, ago: &str) -> String {
    match l {
        Lang::En => format!(
            "# {}Notes from last night (written {date}, {ago})",
            if stale { "(stale) " } else { "" }
        ),
        Lang::Ja => format!("# {}昨夜のメモ（{date}、{ago}に作成）", if stale { "（古い）" } else { "" }),
    }
}

pub fn working_memory_heading(l: Lang) -> &'static str {
    l.of(
        "# Working memory (note_write; the user does not see it)\n\n",
        "# 作業メモ（note_write。ユーザーには見えません）\n\n",
    )
}

pub fn coming_up(l: Lang) -> &'static str {
    l.of("Coming up:\n", "これから:\n")
}

pub fn debrief_heading(l: Lang) -> &'static str {
    l.of("# Latest debrief\n\n", "# 最新の朝の手紙\n\n")
}

pub fn no_debrief(l: Lang) -> &'static str {
    l.of("(no debrief yet)\n\n", "（朝の手紙はまだありません）\n\n")
}

pub fn trimmed(l: Lang) -> &'static str {
    l.of("(trimmed for size)", "（長さのため省略）")
}

pub fn on_off(l: Lang, on: bool) -> &'static str {
    if on { l.of("on", "オン") } else { l.of("off", "オフ") }
}

pub fn no_plan_factor(l: Lang) -> &'static str {
    l.of("no plan factor yet", "計画係数はまだありません")
}

pub fn plan_factor(l: Lang, value: f64, sample: i64) -> String {
    match l {
        Lang::En => format!("Plan factor: {value:.1}× from {sample} session{}", plural(sample)),
        Lang::Ja => format!("計画係数: {value:.1}×（{sample}セッションから）"),
    }
}

pub struct SettingsLine<'a> {
    pub name: &'a str,
    pub tz: &'a str,
    pub nightly_time: &'a str,
    pub template: &'a str,
    pub counter: &'a str,
    pub nightly: bool,
    pub checkins: bool,
    pub facts: i64,
    pub split: &'a str,
    pub factor: &'a str,
    pub spent: u32,
    pub allowance: u32,
}

pub fn settings(l: Lang, s: &SettingsLine) -> String {
    let (nightly, checkins) = (on_off(l, s.nightly), on_off(l, s.checkins));
    let SettingsLine { name, tz, nightly_time, template, counter, facts, split, factor, spent, allowance, .. } = s;
    match l {
        Lang::En => format!(
            "# Settings\n\n{name} | {tz} | nightly_time {nightly_time} | template {template} | counter {counter} | nightly {nightly} | checkins {checkins}\n\
             Memory: {facts} fact{} ({split})\n\
             {factor}\n\
             Trigger points you may lay today: {spent} of {allowance} used\n\n",
            plural(*facts),
        ),
        Lang::Ja => format!(
            "# 設定\n\n{name} | {tz} | nightly_time {nightly_time} | template {template} | counter {counter} | nightly {nightly} | checkins {checkins}\n\
             記憶: {facts}件（{split}）\n\
             {factor}\n\
             今日置けるトリガーポイント: {allowance}件中{spent}件使用済み\n\n",
        ),
    }
}

pub fn activity_heading(l: Lang) -> &'static str {
    l.of("# Recent activity\n\n", "# 最近の動き\n\n")
}

pub fn none_line(l: Lang) -> &'static str {
    l.of("(none)\n", "（なし）\n")
}

// --- a thread's own context ---

/// The marker a reply in a check-in thread carries into its session, so the
/// model reads the thread's opening assistant turns as its own scheduled
/// check-ins rather than as answers it once gave.
pub fn checkin_thread_note(l: Lang, date: &str) -> String {
    match l {
        Lang::En => format!(
            "# This conversation\n\nOpened by your scheduled check-in on {date}: every assistant \
             message the user has not answered yet is a check-in question you sent, and the user \
             is replying to it now."
        ),
        Lang::Ja => format!(
            "# この会話\n\n{date}の定時チェックインで始まった会話です。ユーザーがまだ答えていない\
             アシスタントのメッセージはどれもあなたが送ったチェックインの質問で、ユーザーは今それに返信しています。"
        ),
    }
}

/// What the history window no longer reaches: a thread longer than the
/// window loses its oldest turns, and only the summary still carries them.
pub fn summary_thread_note(l: Lang, summary: &str) -> String {
    match l {
        Lang::En => format!("# This conversation\n\nEarlier in this conversation: {summary}"),
        Lang::Ja => format!("# この会話\n\nこの会話のこれまで: {summary}"),
    }
}

pub fn title_user(l: Lang) -> &'static str {
    l.of("User", "ユーザー")
}

pub fn title_assistant(l: Lang) -> &'static str {
    l.of("Assistant", "アシスタント")
}

// --- background sessions' openings ---

pub fn notes_to_settle(l: Lang) -> &'static str {
    l.of(
        "\n\nWorking notes due to settle tonight, each with note_settle (id: title):\n",
        "\n\n今夜 note_settle で一つずつ片付ける作業メモ（id: 内容）:\n",
    )
}

pub fn nightly_opening(l: Lang, date: jiff::civil::Date) -> String {
    match l {
        Lang::En => format!("Nightly run for {date}."),
        Lang::Ja => format!("{date}の夜の処理です。"),
    }
}

pub fn summarize_opening(l: Lang, so_far: Option<&str>) -> String {
    let ask = l.of("Summarise this conversation.", "この会話を要約してください。");
    match (l, so_far) {
        (_, None) => ask.to_string(),
        (Lang::En, Some(s)) => format!("Summary so far: {s}\n\n{ask}"),
        (Lang::Ja, Some(s)) => format!("これまでの要約: {s}\n\n{ask}"),
    }
}

pub fn review_opening(l: Lang, week: &str, sunday: jiff::civil::Date, digest: &str) -> String {
    match l {
        Lang::En => format!("The week of {week}, up to and including {sunday}.\n\n{digest}"),
        Lang::Ja => format!("{week}から{sunday}までの一週間です。\n\n{digest}"),
    }
}

pub fn search_query(l: Lang) -> &'static str {
    l.of("Query", "検索語")
}

pub fn search_question(l: Lang) -> &'static str {
    l.of("Question", "質問")
}

pub fn search_results(l: Lang) -> &'static str {
    l.of("Results", "結果")
}

pub fn no_results(l: Lang) -> &'static str {
    l.of("no results", "結果なし")
}

// --- the harvest digest ---

pub fn harvest_kind(l: Lang, checkin: Option<&str>) -> String {
    match (l, checkin) {
        (Lang::En, Some(d)) => format!("checkin {d}"),
        (Lang::Ja, Some(d)) => format!("チェックイン {d}"),
        (_, None) => l.of("talk", "会話").into(),
    }
}

pub fn harvest_thread(l: Lang, title: &str, kind: &str, when: &str, body: &str) -> String {
    match l {
        Lang::En => format!("## {title} ({kind}, last active {when})\n{body}\n\n"),
        Lang::Ja => format!("## {title}（{kind}、最終 {when}）\n{body}\n\n"),
    }
}

pub fn speaker(l: Lang, user: bool) -> &'static str {
    if user { l.of("user", "ユーザー") } else { l.of("note", "Note") }
}

pub fn tonights_episodic(l: Lang) -> &'static str {
    l.of("## Tonight's episodic entries", "## 今夜のエピソード記録")
}

// --- the week's digest ---

pub fn review_section(l: Lang, section: ReviewSection) -> &'static str {
    match section {
        ReviewSection::Tasks => l.of("Tasks", "タスク"),
        ReviewSection::Sessions => l.of("Sessions", "作業セッション"),
        ReviewSection::Triggers => l.of("Trigger points", "トリガーポイント"),
        ReviewSection::Nights => l.of("Nights", "夜ごとの記録"),
        ReviewSection::Conversations => l.of("Conversations", "会話"),
        ReviewSection::Memory => l.of("The week as memory holds it", "記憶に残る一週間"),
    }
}

#[derive(Clone, Copy)]
pub enum ReviewSection {
    Tasks,
    Sessions,
    Triggers,
    Nights,
    Conversations,
    Memory,
}

pub fn planned_min(l: Lang, min: i64) -> String {
    match l {
        Lang::En => format!(", planned {min} min"),
        Lang::Ja => format!("、予定{min}分"),
    }
}

pub fn ran_min(l: Lang, min: i64) -> String {
    match l {
        Lang::En => format!(", ran {min} min"),
        Lang::Ja => format!("、実際{min}分"),
    }
}

pub fn never_ended(l: Lang) -> &'static str {
    l.of(", never ended", "、終了せず")
}

pub fn asked_about_overrun(l: Lang) -> &'static str {
    l.of(", asked about overrun", "、超過について確認済み")
}

pub fn triggers_line(l: Lang, by_status: &str, said: i64, quiet: i64) -> String {
    match l {
        Lang::En => format!("{by_status} — said {said}, stayed quiet {quiet}"),
        Lang::Ja => format!("{by_status} — 発言{said}、見送り{quiet}"),
    }
}

pub fn facts_kept(l: Lang, date: &str, n: i64) -> String {
    match l {
        Lang::En => format!("- {date}: {n} fact(s) kept"),
        Lang::Ja => format!("- {date}: {n}件を記憶"),
    }
}

// --- trigger sessions ---

pub fn close_day_prompt(l: Lang) -> &'static str {
    l.of(
        "It is the close of the day. In one or two lines say what is \
         still pending and what got done, then ask whether to carry the rest to tomorrow. If they say \
         yes, call plan_carry.",
        "一日の締めくくりです。まだ残っていることと終わったことを一、二行で伝え、\
         残りを明日に回すかどうか尋ねてください。回すと言われたら plan_carry を呼びます。",
    )
}

pub fn lay_day_prompt(l: Lang) -> &'static str {
    l.of(
        "The user's day has begun. Lay the day: check today's order and set it if the night left \
         it empty or wrong, then lay 4 to 8 wake-ups with trigger_set, each tied to a moment — \
         just after a work session's planned end, before and after calendar events, midday, late \
         afternoon. Then stay quiet unless something needs the user now.",
        "ユーザーの一日が始まりました。一日を組んでください: 今日の順番を確かめ、夜のうちに空か\
         ずれていれば決め、trigger_set で目覚めを4〜8個、それぞれある瞬間に結びつけて置く — \
         作業セッションの予定の終わりの直後、カレンダーの予定の前後、昼、夕方。そのあと、今\
         ユーザーに必要なことがなければ黙る。",
    )
}

pub fn idle_prompt(l: Lang) -> &'static str {
    l.of(
        "The user has gone quiet while your working memory holds notes. \
         Decide whether one of them is worth raising now.",
        "作業メモが残ったまま、ユーザーからの反応が途絶えています。\
         どれかを今持ち出す価値があるか判断してください。",
    )
}

pub fn laid_at(l: Lang, laid: &str, meant: &str) -> String {
    match l {
        Lang::En => format!("Laid at {laid}, meant for {meant}."),
        Lang::Ja => format!("{laid}に設定、{meant}の予定。"),
    }
}

pub fn meant_for(l: Lang, meant: &str) -> String {
    match l {
        Lang::En => format!("Meant for {meant}."),
        Lang::Ja => format!("{meant}の予定。"),
    }
}

pub fn work_session(l: Lang, title: &str, started: &str) -> String {
    match l {
        Lang::En => format!("Work session: {title:?}, started {started}"),
        Lang::Ja => format!("作業セッション: {title:?}、{started}開始"),
    }
}

pub fn pomodoro_round(l: Lang, phase: &str, round: i64) -> String {
    match l {
        Lang::En => format!(", in the {phase} of round {round}"),
        Lang::Ja => format!("、{round}ラウンド目の{phase}"),
    }
}

pub fn steps_done_since(l: Lang, done: i64) -> String {
    match l {
        Lang::En => format!(", {done} step{} done since the last check.", plural(done)),
        Lang::Ja => format!("、前回の確認から{done}ステップ完了。"),
    }
}

pub fn last_user_message(l: Lang, at: &str) -> String {
    match l {
        Lang::En => format!("Last message from the user: {at}."),
        Lang::Ja => format!("ユーザーの最後のメッセージ: {at}。"),
    }
}

pub fn user_silent(l: Lang) -> &'static str {
    l.of("The user has not written anything yet.\n", "ユーザーはまだ何も書いていません。\n")
}

pub fn quiet_for(l: Lang, min: i64) -> String {
    match l {
        Lang::En => format!("Nothing from the user for {min} min."),
        Lang::Ja => format!("ユーザーから{min}分間反応がありません。"),
    }
}

pub fn open_working_lines(l: Lang) -> &'static str {
    l.of(
        "Working notes (id: title, age, last raised):\n",
        "作業メモ（id: 内容、経過、最後に触れた時）:\n",
    )
}

pub fn ago_short(l: Lang, min: i64) -> String {
    match (l, min) {
        (Lang::En, 0..=59) => format!("{min} min"),
        (Lang::En, 60..=2879) => format!("{} h", min / 60),
        (Lang::En, _) => format!("{} d", min / 1440),
        (Lang::Ja, 0..=59) => format!("{min}分"),
        (Lang::Ja, 60..=2879) => format!("{}時間", min / 60),
        (Lang::Ja, _) => format!("{}日", min / 1440),
    }
}

pub fn nudged_ago(l: Lang, ago: &str) -> String {
    match l {
        Lang::En => format!("nudged {ago} ago"),
        Lang::Ja => format!("{ago}前に声かけ"),
    }
}

pub fn never_nudged(l: Lang) -> &'static str {
    l.of("never nudged", "声かけなし")
}

pub fn working_line(l: Lang, id: &str, title: &str, added: &str, nudge: &str) -> String {
    match l {
        Lang::En => format!("- {id}: {title:?}, added {added} ago, {nudge}"),
        Lang::Ja => format!("- {id}: {title:?}、{added}前に追加、{nudge}"),
    }
}

// --- calls ---

pub fn why_this_call(l: Lang) -> &'static str {
    l.of("\n\n# Why this call\n\n", "\n\n# この通話の理由\n\n")
}

pub fn called_about(l: Lang, title: &str, body: &str) -> String {
    match l {
        Lang::En => format!("You called about: {title}. You opened with: {body}"),
        Lang::Ja => format!("用件: {title}。最初にこう話しました: {body}"),
    }
}

pub fn user_called(l: Lang) -> &'static str {
    l.of("The user called you.", "ユーザーから電話がありました。")
}

pub fn earlier_in_thread(l: Lang) -> &'static str {
    l.of("\n\n# Earlier in this thread\n", "\n\n# このスレッドのこれまで\n")
}

pub fn call_speaker(l: Lang, user: bool) -> &'static str {
    if user { l.of("you", "ユーザー") } else { "Note" }
}

pub fn phone_ringing(l: Lang) -> &'static str {
    l.of("[note] The phone is ringing.", "[note] 電話を鳴らしています。")
}

// --- what a share link shows its session ---

pub fn share_shared(l: Lang) -> &'static str {
    l.of("# What is shared\n\n", "# 共有されている内容\n\n")
}

pub fn share_today_ahead(l: Lang) -> &'static str {
    l.of("# Today and ahead\n\n", "# 今日とこの先\n\n")
}

pub fn share_busy(l: Lang) -> &'static str {
    l.of("Busy", "予定あり")
}

pub fn share_today(l: Lang) -> &'static str {
    l.of(" (today)", "（今日）")
}

pub fn share_nothing_planned(l: Lang) -> &'static str {
    l.of("- nothing planned\n", "- 予定なし\n")
}

pub fn share_goals(l: Lang) -> &'static str {
    l.of("# Goals\n\n", "# 目標\n\n")
}

pub fn share_goal_due(l: Lang, day: &str) -> String {
    match l {
        Lang::En => format!(", due {day}"),
        Lang::Ja => format!("、期限 {day}"),
    }
}

pub fn share_goal(l: Lang, title: &str, done: i64, total: i64, due: &str) -> String {
    match l {
        Lang::En => format!("- {title} ({done} of {total} tasks done{due})"),
        Lang::Ja => format!("- {title}（{total}件中{done}件完了{due}）"),
    }
}

pub fn share_none(l: Lang) -> &'static str {
    l.of("- none\n", "- なし\n")
}

pub fn share_and_more(l: Lang, n: usize) -> String {
    match l {
        Lang::En => format!("- and {n} more"),
        Lang::Ja => format!("- ほか{n}件"),
    }
}

pub fn share_open_tasks(l: Lang) -> &'static str {
    l.of("# Open tasks\n\n", "# 未完了のタスク\n\n")
}

pub fn share_urgent(l: Lang) -> &'static str {
    l.of("Urgent:\n", "急ぎ:\n")
}

pub fn share_others(l: Lang) -> &'static str {
    l.of("Others:\n", "その他:\n")
}

pub fn share_more_ask(l: Lang, n: usize) -> String {
    match l {
        Lang::En => format!("- and {n} more; ask"),
        Lang::Ja => format!("- ほか{n}件。ツールで確かめられます"),
    }
}

pub fn share_none_open(l: Lang) -> &'static str {
    l.of("- none open\n", "- 未完了なし\n")
}

pub fn share_done_recent(l: Lang, days: i64) -> String {
    match l {
        Lang::En => format!("# Done in the last {days} days\n\n"),
        Lang::Ja => format!("# 直近{days}日間に完了\n\n"),
    }
}

pub fn share_nothing_yet(l: Lang) -> &'static str {
    l.of("- nothing yet\n", "- まだなし\n")
}

pub fn share_mark_urgent(l: Lang) -> &'static str {
    l.of(" [urgent]", " [急ぎ]")
}

pub fn share_mark_overdue(l: Lang) -> &'static str {
    l.of(" [overdue]", " [期限切れ]")
}

pub fn share_mark_due_soon(l: Lang) -> &'static str {
    l.of(" [due soon]", " [期限間近]")
}

pub fn share_steps(l: Lang, done: i64, total: i64) -> String {
    match l {
        Lang::En => format!(", {done} of {total} steps done"),
        Lang::Ja => format!("、{total}ステップ中{done}完了"),
    }
}

pub fn share_goal_of(l: Lang, goal: &str) -> String {
    match l {
        Lang::En => format!(", goal: {goal}"),
        Lang::Ja => format!("、目標: {goal}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Japanese line that slips back into English reads as a missed
    /// translation; every pair here must differ.
    #[test]
    fn every_fixed_line_has_its_own_japanese() {
        let pairs: [fn(Lang) -> &'static str; 14] = [
            call_briefly, no_standing, standing_heading, now_heading, plan_heading, tasks_heading,
            working_memory_heading, coming_up, debrief_heading, activity_heading, close_day_prompt,
            idle_prompt, share_shared, notes_to_settle,
        ];
        for f in pairs {
            assert_ne!(f(Lang::En), f(Lang::Ja));
            assert!(!f(Lang::Ja).is_ascii(), "{}", f(Lang::Ja));
        }
    }
}
