//! Finds sensitive information in recognized text, for automatic redaction.
//!
//! Detectors favor precision: a missed match can still be blurred by hand,
//! but false positives make the feature tiresome.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Email,
    IpAddress,
    CreditCard,
    ApiKey,
    Jwt,
    PrivateKey,
    SocialSecurityNumber,
    /// A long random-looking string (password, token, hash).
    Secret,
}

struct Detector {
    kind: Kind,
    pattern: Regex,
    validate: fn(&str) -> bool,
}

fn always(_: &str) -> bool {
    true
}

static DETECTORS: LazyLock<Vec<Detector>> = LazyLock::new(|| {
    let d = |kind, pattern: &str, validate: fn(&str) -> bool| Detector {
        kind,
        pattern: Regex::new(pattern).expect("detector patterns are valid"),
        validate,
    };
    vec![
        d(
            Kind::PrivateKey,
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
            always,
        ),
        d(
            Kind::Jwt,
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
            always,
        ),
        d(
            Kind::ApiKey,
            concat!(
                r"\b(?:",
                r"sk-ant-[A-Za-z0-9_-]{16,}",        // Anthropic
                r"|sk-(?:proj-)?[A-Za-z0-9_-]{20,}", // OpenAI and similar
                r"|(?:sk|pk|rk)_(?:live|test)_[A-Za-z0-9]{16,}", // Stripe
                r"|gh[pousr]_[A-Za-z0-9]{20,}",      // GitHub tokens
                r"|github_pat_[A-Za-z0-9_]{20,}",    // GitHub fine-grained tokens
                r"|xox[abprs]-[A-Za-z0-9-]{10,}",    // Slack
                r"|AKIA[0-9A-Z]{16}",                // AWS access key id
                r"|AIza[0-9A-Za-z_-]{35}",           // Google API key
                r"|glpat-[A-Za-z0-9_-]{20,}",        // GitLab
                r"|npm_[A-Za-z0-9]{36}",             // npm
                r")",
            ),
            always,
        ),
        d(
            Kind::Email,
            r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b",
            always,
        ),
        d(Kind::IpAddress, r"\b(?:\d{1,3}\.){3}\d{1,3}\b", valid_ipv4),
        d(Kind::CreditCard, r"\b\d(?:[ -]?\d){12,18}\b", valid_card),
        d(Kind::SocialSecurityNumber, r"\b\d{3}-\d{2}-\d{4}\b", always),
        d(Kind::Secret, r"[A-Za-z0-9+/=_-]{24,}", looks_random),
    ]
});

/// Sensitive spans in `text` as byte ranges, sorted and non-overlapping.
/// Earlier detectors win where matches overlap.
pub fn find(text: &str) -> Vec<(Kind, Range<usize>)> {
    let mut found: Vec<(Kind, Range<usize>)> = Vec::new();
    for detector in DETECTORS.iter() {
        for m in detector.pattern.find_iter(text) {
            let overlaps = found
                .iter()
                .any(|(_, r)| m.start() < r.end && r.start < m.end());
            if !overlaps && (detector.validate)(m.as_str()) {
                found.push((detector.kind, m.range()));
            }
        }
    }
    found.sort_by_key(|(_, r)| r.start);
    found
}

fn valid_ipv4(s: &str) -> bool {
    s.split('.')
        .all(|octet| octet.parse::<u16>().is_ok_and(|n| n <= 255))
}

/// Card numbers: 13 to 19 digits passing the Luhn checksum.
fn valid_card(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            if i % 2 == 1 {
                let doubled = d * 2;
                if doubled > 9 { doubled - 9 } else { doubled }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// Mixed letters and digits with high character entropy: likely a token,
/// not a word or a path.
fn looks_random(s: &str) -> bool {
    let letters = s.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    let upper = s.chars().filter(|c| c.is_ascii_uppercase()).count();
    if letters == 0 || digits == 0 || upper == 0 || upper == letters {
        return false;
    }
    shannon_entropy(s) >= 3.8
}

/// Bits per character.
fn shannon_entropy(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let len = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(Kind, &str)> {
        find(text).into_iter().map(|(k, r)| (k, &text[r])).collect()
    }

    #[test]
    fn finds_emails_and_ips() {
        assert_eq!(
            kinds("mail jane.doe+x@example.co.uk from 192.168.1.20"),
            vec![
                (Kind::Email, "jane.doe+x@example.co.uk"),
                (Kind::IpAddress, "192.168.1.20")
            ]
        );
        assert!(kinds("version 1.2.3.400").is_empty());
    }

    #[test]
    fn cards_need_a_valid_checksum() {
        assert_eq!(
            kinds("card 4111 1111 1111 1111 ok"),
            vec![(Kind::CreditCard, "4111 1111 1111 1111")]
        );
        assert!(kinds("order 4111 1111 1111 1112").is_empty());
    }

    #[test]
    fn finds_known_tokens() {
        let aws = "AKIAIOSFODNN7EXAMPLE";
        assert_eq!(kinds(&format!("key={aws}")), vec![(Kind::ApiKey, aws)]);
        let github = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        assert_eq!(kinds(github), vec![(Kind::ApiKey, github)]);
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(kinds(jwt), vec![(Kind::Jwt, jwt)]);
        assert_eq!(
            kinds("-----BEGIN RSA PRIVATE KEY-----"),
            vec![(Kind::PrivateKey, "-----BEGIN RSA PRIVATE KEY-----")]
        );
    }

    #[test]
    fn random_strings_but_not_words_or_paths() {
        let secret = "Zq8vR2xLp9TfW4mKs7NdY3bHc";
        assert_eq!(kinds(secret), vec![(Kind::Secret, secret)]);
        assert!(kinds("internationalization_configuration").is_empty());
        assert!(kinds("/usr/local/share/applications").is_empty());
        assert!(kinds("2026-10-10T13:15:16.223").is_empty());
    }

    #[test]
    fn known_patterns_take_precedence_over_generic_ones() {
        let key = "sk-ant-api03-Zq8vR2xLp9TfW4mKs7NdY3bHc";
        assert_eq!(kinds(key), vec![(Kind::ApiKey, key)]);
    }
}
