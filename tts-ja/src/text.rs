use std::sync::LazyLock;

use regex::{Captures, Regex};
use unicode_normalization::UnicodeNormalization;

/// Readings kanalizer gets wrong or that are spelled as letters; keys are lowercase.
const READINGS: &[(&str, &str)] = &[
    ("ai", "エーアイ"),
    ("android", "アンドロイド"),
    ("api", "エーピーアイ"),
    ("app", "アプリ"),
    ("chatgpt", "チャットジーピーティー"),
    ("claude", "クロード"),
    ("discord", "ディスコード"),
    ("email", "イーメール"),
    ("etc", "エトセトラ"),
    ("excel", "エクセル"),
    ("github", "ギットハブ"),
    ("gmail", "ジーメール"),
    ("google", "グーグル"),
    ("ios", "アイオーエス"),
    ("ipad", "アイパッド"),
    ("iphone", "アイフォーン"),
    ("javascript", "ジャバスクリプト"),
    ("line", "ライン"),
    ("linux", "リナックス"),
    ("mac", "マック"),
    ("matrix", "マトリックス"),
    ("nasa", "ナサ"),
    ("note", "ノート"),
    ("notion", "ノーション"),
    ("ok", "オーケー"),
    ("okay", "オーケー"),
    ("openai", "オープンエーアイ"),
    ("outlook", "アウトルック"),
    ("pdf", "ピーディーエフ"),
    ("powerpoint", "パワーポイント"),
    ("python", "パイソン"),
    ("slack", "スラック"),
    ("teams", "チームズ"),
    ("todo", "トゥードゥー"),
    ("twitter", "ツイッター"),
    ("url", "ユーアールエル"),
    ("vs", "ブイエス"),
    ("wi-fi", "ワイファイ"),
    ("wifi", "ワイファイ"),
    ("windows", "ウィンドウズ"),
    ("youtube", "ユーチューブ"),
    ("zoom", "ズーム"),
];

const LETTERS: [&str; 26] = [
    "エー",
    "ビー",
    "シー",
    "ディー",
    "イー",
    "エフ",
    "ジー",
    "エイチ",
    "アイ",
    "ジェー",
    "ケー",
    "エル",
    "エム",
    "エヌ",
    "オー",
    "ピー",
    "キュー",
    "アール",
    "エス",
    "ティー",
    "ユー",
    "ブイ",
    "ダブリュー",
    "エックス",
    "ワイ",
    "ゼット",
];

static LATIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z]+(?:-[A-Za-z]+)*").expect("the Latin pattern"));
static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)").expect("the link pattern"));
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://\S+").expect("the URL pattern"));
static MARKUP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[*_`#>|]").expect("the markup pattern"));
static SPACED_DIGITS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d)\s+(\d)").expect("the digits pattern"));
static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("the space pattern"));

/// Text both engines read well: NFKC-normalised, without markup or emoji, Latin words in katakana.
pub fn prepare(text: &str) -> String {
    let text: String = text.nfkc().filter(|c| !is_emoji(*c)).collect();
    let text = LINK.replace_all(&text, "$1");
    let text = URL.replace_all(&text, "");
    let text = MARKUP.replace_all(&text, "");
    let text = text.trim().replace(['\n', '\r'], "。");
    let text = LATIN.replace_all(&text, |c: &Captures| katakana(&c[0]));
    let text = SPACED_DIGITS.replace_all(&text, "$1、$2");
    SPACE.replace_all(&text, "").into_owned()
}

/// Whether `text` holds anything to say.
pub fn speakable(text: &str) -> bool {
    text.chars().any(|c| c.is_alphanumeric())
}

/// Listed words take their reading, acronyms are spelled out, and the rest go to kanalizer,
/// VOICEVOX's English reader.
fn katakana(word: &str) -> String {
    let key = word.to_ascii_lowercase();
    if let Some((_, kana)) = READINGS.iter().find(|(k, _)| *k == key) {
        return (*kana).to_owned();
    }
    if word.contains('-') {
        return word.split('-').map(katakana).collect();
    }
    let acronym = word.len() <= 4 && word.chars().all(|c| c.is_ascii_uppercase());
    if !acronym && word.len() > 1 {
        if let Ok(kana) = kanalizer::convert(&key)
            .with_error_on_incomplete(false)
            .perform()
        {
            return kana;
        }
    }
    key.bytes().map(|b| LETTERS[(b - b'a') as usize]).collect()
}

fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF | 0xFE00..=0xFE0F | 0x200D | 0xE0020..=0xE007F)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_words_become_katakana() {
        assert_eq!(
            prepare("GitHubのプルリクエスト"),
            "ギットハブのプルリクエスト"
        );
        assert_eq!(
            prepare("Wi-FiとPDFとAI"),
            "ワイファイとピーディーエフとエーアイ"
        );
        assert_eq!(prepare("Zoomで OK です"), "ズームでオーケーです");
        assert_eq!(prepare("NHKを見る"), "エヌエイチケーを見る");
        assert_eq!(prepare("Pull Requestを出す"), "プルリクエストを出す");
        assert!(!prepare("Slackとmeeting").contains(|c: char| c.is_ascii_alphabetic()));
    }

    #[test]
    fn markup_and_emoji_are_dropped() {
        assert_eq!(prepare("**大事** なこと🎉"), "大事なこと");
        assert_eq!(
            prepare("[リンク](https://example.com)を見て"),
            "リンクを見て"
        );
        assert_eq!(prepare("ＡＢＣ１２３"), "エービーシー123");
        assert_eq!(prepare("ｶﾀｶﾅ"), "カタカナ");
        assert_eq!(prepare("1 2"), "1、2");
    }

    #[test]
    fn nothing_to_say() {
        assert!(!speakable(&prepare("🎉✨")));
        assert!(speakable(&prepare("はい。")));
    }
}
