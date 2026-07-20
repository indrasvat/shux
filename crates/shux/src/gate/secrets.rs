//! Pre-bless secret scanner (task §5/§7, council #3). A bless is a WRITE that commits
//! captured terminal text into the repo; this refuses if that material carries a
//! credential. It reports RULE IDS only and NEVER echoes the matched secret (council: do
//! not leak the value into logs/CI). Curated high-precision patterns plus a CONSERVATIVE
//! high-entropy backstop with a hash allowlist (so a git sha / content hash in normal
//! output does not block a legitimate bless).

use std::sync::OnceLock;

use regex::Regex;

/// One curated detector: a stable id + its pattern.
struct Rule {
    id: &'static str,
    re: Regex,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let r = |id: &'static str, pat: &str| Rule {
            id,
            re: Regex::new(pat).expect("secret rule compiles"),
        };
        vec![
            r("aws-access-key", r"\bAKIA[0-9A-Z]{16}\b"),
            r("github-token", r"\bgh[pousr]_[A-Za-z0-9]{36,}\b"),
            r("gitlab-token", r"\bglpat-[A-Za-z0-9_-]{20,}\b"),
            r("slack-token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b"),
            r("slack-webhook", r"https://hooks\.slack\.com/services/[A-Za-z0-9/]{20,}"),
            r("npm-token", r"\bnpm_[A-Za-z0-9]{36}\b"),
            r("stripe-key", r"\b[rs]k_(live|test)_[A-Za-z0-9]{16,}\b"),
            r("openai-key", r"\bsk-(proj-)?[A-Za-z0-9]{20,}\b"),
            r("google-api-key", r"\bAIza[0-9A-Za-z_-]{35}\b"),
            r("private-key-block", r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"),
            r("jwt", r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}"),
            r("bearer-token", r"(?i)\bbearer\s+[A-Za-z0-9._~+/-]{20,}={0,2}"),
            // user:pass@host in a URL (basic-auth / DB connection string). The user part
            // is OPTIONAL (`*`, not `+`) so the very common password-only form
            // `redis://:pass@host` / `mongodb://:pass@db` is caught (adv Agent B, MAJOR-2).
            r(
                "basic-auth-url",
                r"[a-zA-Z][a-zA-Z0-9+.-]*://[^/\s:@]*:[^/\s:@]{3,}@",
            ),
            // key = secret / password: value assignments.
            r(
                "generic-assignment",
                r#"(?i)\b(password|passwd|secret|token|api[_-]?key|access[_-]?key|auth)\b\s*[:=]\s*['"]?[A-Za-z0-9/+_.-]{8,}"#,
            ),
        ]
    })
}

/// Scan `text` and return the SORTED, de-duplicated rule IDs that matched (empty = clean).
/// The high-entropy backstop appends `high-entropy-token` when a long, mixed-class,
/// non-hash token is present.
pub fn scan(text: &str) -> Vec<String> {
    let mut hits: Vec<String> = Vec::new();
    for rule in rules() {
        if rule.re.is_match(text) {
            hits.push(rule.id.to_string());
        }
    }
    if has_high_entropy_token(text) {
        hits.push("high-entropy-token".to_string());
    }
    hits.sort();
    hits.dedup();
    hits
}

/// Replace only the high-entropy TOKENS in `text` with `[redacted]`, preserving everything
/// else (085 F11).
///
/// The entropy backstop fires on ordinary host paths — a `mkdtemp` directory component is
/// long, mixed-class and non-hex — and the caller's response was to discard the whole
/// message. That destroyed the only explanation of a SECURITY refusal: a `cwd` escaping
/// containment reported `infra_error` with nothing but "[redacted]", indistinguishable from
/// a transient failure. Redacting the token keeps the sentence, and the sentence is the
/// part the reader needs.
pub fn redact_high_entropy_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if !token.is_empty() {
            if is_high_entropy_token(token) {
                out.push_str("[redacted]");
            } else {
                out.push_str(token);
            }
            token.clear();
        }
    };
    for c in text.chars() {
        if c.is_whitespace() || matches!(c, '"' | '\'' | '=' | ',' | ';' | '<' | '>') {
            flush(&mut token, &mut out);
            out.push(c);
        } else {
            token.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

/// True if any whitespace-delimited token looks like a high-entropy secret: length ≥ 24,
/// mixed letters AND digits, Shannon entropy ≥ 4.2 bits/char, and NOT a plain hex/decimal
/// hash (git shas, content digests in normal output must not block a bless). The 24-char
/// floor (lowered from 32 — adv Agent B, MAJOR-2) catches shorter tokens; the hex allowlist
/// is a deliberate tradeoff (a bare 32-hex string is as likely a hash as a token, and
/// flagging every hash would block ordinary blesses).
fn has_high_entropy_token(text: &str) -> bool {
    text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '=' | ',' | ';' | '<' | '>'))
        .any(is_high_entropy_token)
}

fn is_high_entropy_token(tok: &str) -> bool {
    if tok.len() < 24 {
        return false;
    }
    if !tok
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-' | '.'))
    {
        return false;
    }
    let has_alpha = tok.chars().any(|c| c.is_ascii_alphabetic());
    let has_digit = tok.chars().any(|c| c.is_ascii_digit());
    if !(has_alpha && has_digit) {
        return false;
    }
    // Allowlist: a pure-hex token is a hash/id, not a credential.
    if tok.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    shannon_entropy(tok) >= 4.2
}

/// Shannon entropy (bits/char) of `s`.
fn shannon_entropy(s: &str) -> f64 {
    let mut counts = [0usize; 256];
    let mut n = 0usize;
    for b in s.bytes() {
        counts[b as usize] += 1;
        n += 1;
    }
    if n == 0 {
        return 0.0;
    }
    let n = n as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_terminal_text_is_not_flagged() {
        for ok in [
            "hello, lens gate",
            "the quick brown fox jumps over the lazy dog 12345",
            // A git short/long sha (pure hex) must not trip the entropy backstop.
            "commit 9e3f2a1b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f",
            // A shux content hash in a status line.
            "capture_sha256=aa11bb22cc33dd44ee55ff6600112233445566778899aabbccddeeff00112233",
        ] {
            assert!(
                scan(ok).is_empty(),
                "false positive on {ok:?}: {:?}",
                scan(ok)
            );
        }
    }

    #[test]
    fn common_credential_shapes_are_flagged() {
        // Fixtures are split with `concat!` so no literal provider credential appears in
        // the SOURCE (else GitHub push-protection blocks the commit); the compile-time
        // concatenation still hands the scanner the full runtime value.
        let cases = [
            ("AKIAIOSFODNN7EXAMPLE", "aws-access-key"), // AWS docs example (allowlisted)
            (
                concat!("ghp", "_1234567890abcdefghijklmnopqrstuvwxyzAB"),
                "github-token",
            ),
            (concat!("xox", "b-123456789012-abcdefghijkl"), "slack-token"),
            (
                concat!(
                    "https://hooks.",
                    "slack.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX"
                ),
                "slack-webhook",
            ),
            (
                concat!("sk_", "live_abcdefghijklmnopqrstuvwx"),
                "stripe-key",
            ),
            (
                concat!("AI", "zaSyD1234567890abcdefghijklmnopqrstuv"),
                "google-api-key",
            ),
            ("password = hunter2secret", "generic-assignment"),
            (
                "postgres://user:s3cr3tpass@db.internal:5432/app",
                "basic-auth-url",
            ),
            // adv Agent B, MAJOR-2: the very common password-only URL form.
            (
                "redis://:hunter2secret@cache.internal:6379",
                "basic-auth-url",
            ),
            ("mongodb://:s3cretpass@db:27017", "basic-auth-url"),
            // A 28-char mixed token now trips the lowered entropy floor.
            ("Ab3Xy9Zq7Kp2Lm5Nv8Rt4Ws6Yc1D", "high-entropy-token"),
        ];
        for (text, rule) in cases {
            let hits = scan(text);
            assert!(
                hits.iter().any(|h| h == rule),
                "expected rule {rule} for {text:?}, got {hits:?}"
            );
        }
    }

    #[test]
    fn private_key_block_is_flagged() {
        assert!(
            scan("-----BEGIN OPENSSH PRIVATE KEY-----").contains(&"private-key-block".to_string())
        );
        assert!(scan("-----BEGIN RSA PRIVATE KEY-----").contains(&"private-key-block".to_string()));
    }

    #[test]
    fn jwt_is_flagged() {
        // Split so the literal JWT does not appear in source (GitHub push-protection).
        let jwt = concat!(
            "eyJ",
            "hbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEFghiJKLmno"
        );
        assert!(scan(jwt).contains(&"jwt".to_string()));
    }

    #[test]
    fn scan_never_echoes_the_secret_value() {
        // The API surface is rule IDs only — a caller can log the ids without leaking.
        let secret = "AKIAIOSFODNN7EXAMPLE";
        for id in scan(secret) {
            assert!(!id.contains(secret), "rule id must not embed the secret");
        }
    }

    #[test]
    fn results_are_sorted_and_deduped() {
        let text = "AKIAIOSFODNN7EXAMPLE AKIAIOSFODNN7EXAMPL2 password=abcdefgh";
        let hits = scan(text);
        let mut sorted = hits.clone();
        sorted.sort();
        assert_eq!(hits, sorted);
        sorted.dedup();
        assert_eq!(hits, sorted, "no duplicate rule ids");
    }
}
