//! How long shux waits on a peer that is alive but may be busy.
//!
//! A deadline on another process's answer bounds failure, never success: a
//! healthy daemon or client always answers, and only a stopped or wedged one
//! does not. No finite wait can tell a stuck peer from a slow one on a
//! saturated machine, so every such wait shares one generous bound and reports
//! the peer as not answering, never as dead.

use std::sync::OnceLock;
use std::time::Duration;

/// Overrides [`peer_patience`], in milliseconds, for this process only: a
/// daemon it starts does not inherit it. A test that builds a peer which never
/// answers sets it, so the test does not wait out the default.
pub const PEER_PATIENCE_ENV: &str = "SHUX_PEER_PATIENCE_MS";

const DEFAULT: Duration = Duration::from_secs(30);
const MAX_MS: u64 = 3_600_000;

static PATIENCE: OnceLock<Result<Duration, String>> = OnceLock::new();

/// Reads and validates [`PEER_PATIENCE_ENV`]. `main` calls it before anything
/// else runs, so a malformed value fails the command up front.
pub fn init() -> anyhow::Result<()> {
    PATIENCE
        .get_or_init(load)
        .as_ref()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// How long to wait on a peer before reporting that it did not answer.
pub fn peer_patience() -> Duration {
    // `main` has already refused to start on an invalid value; an embedder
    // that skipped `init` gets the default rather than a panic.
    *PATIENCE.get_or_init(load).as_ref().unwrap_or(&DEFAULT)
}

fn load() -> Result<Duration, String> {
    match std::env::var(PEER_PATIENCE_ENV) {
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(format!("{PEER_PATIENCE_ENV} is not valid UTF-8"))
        }
        Ok(raw) => parse(&raw),
    }
}

fn parse(raw: &str) -> Result<Duration, String> {
    // `u64` parsing also takes a leading `+`.
    match raw.parse::<u64>() {
        Ok(ms @ 1..=MAX_MS) if !raw.starts_with('+') => Ok(Duration::from_millis(ms)),
        _ => Err(format!(
            "{PEER_PATIENCE_ENV} must be a whole number of milliseconds from 1 to {MAX_MS}, \
             got {raw:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_override_is_whole_positive_milliseconds() {
        assert_eq!(parse("2000"), Ok(Duration::from_secs(2)));
        assert_eq!(parse("3600000"), Ok(Duration::from_secs(3600)));
        for bad in ["0", "", "2s", "-5", "1.5", " 100", "+100", "3600001"] {
            let err = parse(bad).expect_err(bad);
            assert!(
                err.contains(PEER_PATIENCE_ENV),
                "{bad:?}: the error names the variable"
            );
        }
    }
}
