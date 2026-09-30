use anyhow::{Context, Result};
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

pub const STEP_SECS: i64 = 30;
pub const DIGITS: usize = 6;
const SEED_BYTES: usize = 20;
const SKEW_STEPS: i64 = 1;

/// Accepts RFC 4648 base32 with or without padding, any case, whitespace ignored.
pub fn parse_seed(text: &str) -> Result<Vec<u8>> {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '=')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    anyhow::ensure!(!cleaned.is_empty(), "empty TOTP seed");
    data_encoding::BASE32_NOPAD
        .decode(cleaned.as_bytes())
        .context("TOTP seed is not valid base32")
}

pub fn generate_seed() -> String {
    let mut bytes = [0u8; SEED_BYTES];
    getrandom::fill(&mut bytes).expect("os rng");
    data_encoding::BASE32_NOPAD.encode(&bytes)
}

pub fn step_at(now: jiff::Timestamp) -> i64 {
    now.as_second().div_euclid(STEP_SECS)
}

pub fn code(seed: &[u8], step: i64) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(seed).expect("hmac accepts any key length");
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let bin = u32::from_be_bytes([digest[offset], digest[offset + 1], digest[offset + 2], digest[offset + 3]])
        & 0x7fff_ffff;
    format!("{:0width$}", bin % 10u32.pow(u32::try_from(DIGITS).unwrap_or(6)), width = DIGITS)
}

fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Returns the step the code matched, within one step either side of `now`.
/// Every candidate is checked so a mismatch costs the same as a match.
pub fn verify(seed: &[u8], submitted: &str, now: jiff::Timestamp) -> Option<i64> {
    let submitted = submitted.trim();
    let center = step_at(now);
    let mut matched = None;
    for step in (center - SKEW_STEPS)..=(center + SKEW_STEPS) {
        if ct_eq(&code(seed, step), submitted) && matched.is_none() {
            matched = Some(step);
        }
    }
    matched
}

pub fn otpauth_uri(seed: &[u8], issuer: &str, account: &str) -> String {
    format!(
        "otpauth://totp/{issuer}:{account}?secret={}&issuer={issuer}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECS}",
        data_encoding::BASE32_NOPAD.encode(seed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC_SEED: &[u8] = b"12345678901234567890";

    #[test]
    fn rfc6238_sha1_vectors() {
        for (secs, expected) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            let now = jiff::Timestamp::from_second(secs).unwrap();
            assert_eq!(code(RFC_SEED, step_at(now)), expected, "t={secs}");
        }
    }

    #[test]
    fn verify_accepts_one_step_of_skew_and_nothing_further() {
        let now = jiff::Timestamp::from_second(1_111_111_111).unwrap();
        let center = step_at(now);
        assert_eq!(verify(RFC_SEED, &code(RFC_SEED, center), now), Some(center));
        assert_eq!(verify(RFC_SEED, &code(RFC_SEED, center - 1), now), Some(center - 1));
        assert_eq!(verify(RFC_SEED, &code(RFC_SEED, center + 1), now), Some(center + 1));
        assert_eq!(verify(RFC_SEED, &code(RFC_SEED, center + 2), now), None);
        assert_eq!(verify(RFC_SEED, "000000", now).is_some(), code(RFC_SEED, center) == "000000");
        assert_eq!(verify(RFC_SEED, "", now), None);
        assert_eq!(verify(RFC_SEED, " 050471 ", now), Some(center));
    }

    #[test]
    fn seed_parsing_is_lenient_about_case_padding_and_whitespace() {
        let encoded = data_encoding::BASE32.encode(RFC_SEED);
        assert_eq!(parse_seed(&encoded).unwrap(), RFC_SEED);
        assert_eq!(parse_seed(&encoded.to_lowercase()).unwrap(), RFC_SEED);
        let spaced: String = encoded.chars().enumerate().map(|(i, c)| if i % 4 == 0 { format!(" {c}") } else { c.to_string() }).collect();
        assert_eq!(parse_seed(&format!("{spaced}\n")).unwrap(), RFC_SEED);
        assert!(parse_seed("").is_err());
        assert!(parse_seed("not base32!").is_err());
    }

    #[test]
    fn generated_seeds_round_trip_and_differ() {
        let a = generate_seed();
        let b = generate_seed();
        assert_ne!(a, b);
        assert_eq!(parse_seed(&a).unwrap().len(), SEED_BYTES);
        assert!(otpauth_uri(&parse_seed(&a).unwrap(), "Note", "admin").contains(&a));
    }
}
