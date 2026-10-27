//! The OSC 7501 payload codec: everything after `ESC ] 7501 ;` and before
//! the terminator.
//!
//! Every pair is validated before anything is returned, and every occurrence
//! of a repeated key is checked against the limits even though only the last
//! one is kept — a report that breaks a limit anywhere is discarded whole.

use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use crate::limits::{
    MAX_APP_BYTES, MAX_ID_BYTES, MAX_KEY_BYTES, MAX_MSG_DECODED, MAX_MSG_ENCODED,
    MAX_PAYLOAD_BYTES, MAX_TITLE_DECODED, MAX_TITLE_ENCODED,
};
use crate::model::{Kind, Record, RecordId, Report, State, is_id_byte};

/// The OSC number, as it appears in the first parameter.
pub const OSC_NUMBER: &[u8] = b"7501";

/// The feature-detection reply body. The lists are what shux accepts, which
/// the spec fixes for every revision.
const QUERY_REPLY_BODY: &[u8] =
    b"?:states=idle,working,done,blocked,error:kinds=permission,question,auth";

/// Standard alphabet; padding optional; non-zero trailing bits rejected.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(false),
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// `OSC 7501 ; ? ST` — answer with [`query_reply`].
    Query,
    Report(Report),
}

/// Why a payload was discarded. Nothing from a discarded report is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    PayloadTooLong,
    KeyTooLong,
    ValueTooLong,
    BadId,
    BadText,
    MissingState,
    UnknownState,
}

/// `ESC ] 7501 ; ? : states=… : kinds=…` followed by `terminator`, which the
/// caller mirrors from the query.
pub fn query_reply(terminator: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + QUERY_REPLY_BODY.len() + terminator.len());
    out.extend_from_slice(b"\x1b]7501;");
    out.extend_from_slice(QUERY_REPLY_BODY);
    out.extend_from_slice(terminator);
    out
}

/// The payload (after `7501;`) that [`decode`] turns back into `report`.
/// Text is emitted padded; callers keep within the limits.
pub fn encode_payload(report: &Report) -> Vec<u8> {
    let mut out = String::new();
    let mut push = |key: &str, value: &str| {
        if !out.is_empty() {
            out.push(':');
        }
        out.push_str(key);
        out.push('=');
        out.push_str(value);
    };
    match report {
        Report::Clear { id } => {
            push("state", "clear");
            if let Some(id) = id {
                push("id", id.as_str());
            }
        }
        Report::Set { id, record } => {
            push("state", record.state.as_str());
            if let Some(id) = id {
                push("id", id.as_str());
            }
            if let Some(kind) = record.kind {
                push("kind", kind.as_str());
            }
            if let Some(progress) = record.progress {
                push("progress", &progress.to_string());
            }
            if let Some(app) = &record.app {
                push("app", app);
            }
            if let Some(title) = &record.title {
                push("title", &BASE64.encode(title));
            }
            if let Some(msg) = &record.msg {
                push("msg", &BASE64.encode(msg));
            }
        }
    }
    out.into_bytes()
}

pub fn decode(payload: &[u8]) -> Result<Message, Reject> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(Reject::PayloadTooLong);
    }
    if trim(payload) == b"?" {
        return Ok(Message::Query);
    }

    let mut fields = Fields::default();
    for pair in payload.split(|&b| b == b':') {
        fields.take(pair)?;
    }
    fields.into_report().map(Message::Report)
}

/// Last value of each known key; bad optional values are already `None`.
#[derive(Default)]
struct Fields<'a> {
    state: Option<&'a [u8]>,
    id: Option<RecordId>,
    kind: Option<Kind>,
    progress: Option<u8>,
    app: Option<String>,
    title: Option<String>,
    msg: Option<String>,
}

impl<'a> Fields<'a> {
    fn take(&mut self, pair: &'a [u8]) -> Result<(), Reject> {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            return Ok(());
        };
        let key = trim(&pair[..eq]);
        let value = trim(&pair[eq + 1..]);
        if key.is_empty() {
            return Ok(());
        }
        if key.len() > MAX_KEY_BYTES {
            return Err(Reject::KeyTooLong);
        }
        if !key.iter().all(u8::is_ascii_lowercase) {
            return Ok(());
        }

        let limit = match key {
            b"msg" => MAX_MSG_ENCODED,
            b"title" => MAX_TITLE_ENCODED,
            b"app" => MAX_APP_BYTES,
            b"id" => MAX_ID_BYTES,
            _ => usize::MAX,
        };
        if value.len() > limit {
            return Err(if key == b"id" {
                Reject::BadId
            } else {
                Reject::ValueTooLong
            });
        }

        // An id is never skipped: a skipped id would silently retarget the
        // report at the root record.
        if key == b"id" {
            let id = std::str::from_utf8(value)
                .ok()
                .and_then(RecordId::parse)
                .ok_or(Reject::BadId)?;
            self.id = Some(id);
            return Ok(());
        }
        if !value.iter().copied().all(is_value_byte) {
            return Ok(());
        }
        // Infallible: every value byte is ASCII.
        let text = std::str::from_utf8(value).unwrap_or_default();
        match key {
            b"state" => self.state = Some(value),
            b"kind" => self.kind = Kind::parse(text),
            b"progress" => self.progress = parse_progress(text),
            b"app" => self.app = parse_app(text),
            b"title" => self.title = decode_text(value, MAX_TITLE_DECODED)?,
            b"msg" => self.msg = decode_text(value, MAX_MSG_DECODED)?,
            _ => {}
        }
        Ok(())
    }

    fn into_report(self) -> Result<Report, Reject> {
        let state = self.state.ok_or(Reject::MissingState)?;
        if state == b"clear" {
            return Ok(Report::Clear { id: self.id });
        }
        let state = std::str::from_utf8(state)
            .ok()
            .and_then(State::parse)
            .ok_or(Reject::UnknownState)?;
        let record = Record {
            state,
            kind: self.kind.filter(|_| state == State::Blocked),
            progress: self
                .progress
                .filter(|_| matches!(state, State::Working | State::Blocked)),
            app: self.app,
            title: self.title,
            msg: self.msg,
        };
        Ok(Report::Set {
            id: self.id,
            record,
        })
    }
}

/// `value := [A-Za-z0-9_.,+/=-]*`
fn is_value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
}

fn trim(bytes: &[u8]) -> &[u8] {
    let is_ws = |b: &u8| matches!(b, b' ' | b'\t');
    let start = bytes.iter().position(|b| !is_ws(b)).unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !is_ws(b))
        .map_or(start, |i| i + 1);
    &bytes[start..end]
}

/// `0` to `100` in plain decimal; anything else is absent.
fn parse_progress(s: &str) -> Option<u8> {
    if s.is_empty() || s.len() > 3 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u8>().ok().filter(|&p| p <= 100)
}

/// `[A-Za-z0-9_.+-]{1,32}`; outside the set the app is absent.
fn parse_app(s: &str) -> Option<String> {
    (!s.is_empty() && s.bytes().all(is_id_byte)).then(|| s.to_owned())
}

/// Base64 UTF-8 text with no control characters. Empty means absent.
fn decode_text(value: &[u8], max_decoded: usize) -> Result<Option<String>, Reject> {
    if value.is_empty() {
        return Ok(None);
    }
    // `Indifferent` also accepts partial padding such as `MA=`; padding is
    // optional, but when present it must complete the final quantum.
    if let Some(pad) = value.iter().position(|&b| b == b'=')
        && (!value.len().is_multiple_of(4) || value.len() - pad > 2)
    {
        return Err(Reject::BadText);
    }
    let bytes = BASE64.decode(value).map_err(|_| Reject::BadText)?;
    if bytes.len() > max_decoded {
        return Err(Reject::ValueTooLong);
    }
    let text = String::from_utf8(bytes).map_err(|_| Reject::BadText)?;
    if text.chars().any(char::is_control) {
        return Err(Reject::BadText);
    }
    Ok((!text.is_empty()).then_some(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    fn report(payload: &str) -> Result<Report, Reject> {
        match decode(payload.as_bytes())? {
            Message::Report(r) => Ok(r),
            Message::Query => panic!("{payload:?} decoded as a query"),
        }
    }

    fn set(payload: &str) -> (Option<RecordId>, Record) {
        match report(payload) {
            Ok(Report::Set { id, record }) => (id, record),
            other => panic!("{payload:?}: {other:?}"),
        }
    }

    fn b64(s: &str) -> String {
        STANDARD.encode(s)
    }

    #[test]
    fn query_is_a_bare_question_mark_with_optional_spaces() {
        assert_eq!(decode(b"?"), Ok(Message::Query));
        assert_eq!(decode(b" ? "), Ok(Message::Query));
        assert_eq!(decode(b"?:x=1"), Err(Reject::MissingState));
    }

    #[test]
    fn query_reply_lists_states_and_kinds_and_mirrors_the_terminator() {
        assert_eq!(
            query_reply(b"\x1b\\"),
            b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\"
        );
        assert!(query_reply(b"\x07").ends_with(b"auth\x07"));
    }

    #[test]
    fn full_report_decodes_every_key() {
        let (id, r) = set(&format!(
            "state=blocked:kind=permission:progress=40:app=claude-code:id=a/b:title={}:msg={}",
            b64("Edit"),
            b64("Allow Bash?")
        ));
        assert_eq!(id, RecordId::parse("a/b"));
        assert_eq!(r.state, State::Blocked);
        assert_eq!(r.kind, Some(Kind::Permission));
        assert_eq!(r.progress, Some(40));
        assert_eq!(r.app.as_deref(), Some("claude-code"));
        assert_eq!(r.title.as_deref(), Some("Edit"));
        assert_eq!(r.msg.as_deref(), Some("Allow Bash?"));
    }

    #[test]
    fn whitespace_around_keys_and_values_is_removed() {
        let (_, r) = set(" state = working : app = cargo ");
        assert_eq!(r.state, State::Working);
        assert_eq!(r.app.as_deref(), Some("cargo"));
    }

    #[test]
    fn malformed_pairs_are_skipped_and_the_rest_applies() {
        let (_, r) = set("noequals:=x:Key=v:app=a!b:state=done:kind=permission");
        assert_eq!(r.state, State::Done);
        assert_eq!(r.app, None, "byte outside the value set skips the pair");
        assert_eq!(r.kind, None, "kind only applies to blocked");
    }

    #[test]
    fn unknown_keys_are_ignored_and_the_last_value_wins() {
        let (_, r) = set("future=1:state=idle:state=working:app=a:app=b");
        assert_eq!(r.state, State::Working);
        assert_eq!(r.app.as_deref(), Some("b"));
    }

    #[test]
    fn missing_or_unknown_state_ignores_the_report() {
        assert_eq!(report("app=x"), Err(Reject::MissingState));
        assert_eq!(report("state=paused"), Err(Reject::UnknownState));
        assert_eq!(report("state=IDLE"), Err(Reject::UnknownState));
        assert_eq!(report("state="), Err(Reject::UnknownState));
    }

    #[test]
    fn clear_takes_an_optional_id_and_ignores_everything_else() {
        assert_eq!(report("state=clear"), Ok(Report::Clear { id: None }));
        assert_eq!(
            report("state=clear:id=a:app=x"),
            Ok(Report::Clear {
                id: RecordId::parse("a")
            })
        );
    }

    #[test]
    fn a_malformed_id_discards_the_report_instead_of_hitting_the_root() {
        assert_eq!(report("state=clear:id=a@b"), Err(Reject::BadId));
        assert_eq!(report("state=done:id="), Err(Reject::BadId));
        assert_eq!(report("state=done:id=a//b"), Err(Reject::BadId));
        assert_eq!(
            report("state=done:id=a@b:id=ok"),
            Err(Reject::BadId),
            "a later valid id must not rescue an earlier bad one"
        );
    }

    #[test]
    fn kind_is_kept_only_on_blocked_and_unknown_kinds_are_absent() {
        assert_eq!(set("state=blocked:kind=auth").1.kind, Some(Kind::Auth));
        assert_eq!(set("state=blocked:kind=coffee").1.kind, None);
        assert_eq!(set("state=working:kind=auth").1.kind, None);
    }

    #[test]
    fn progress_is_0_to_100_on_working_or_blocked_only() {
        assert_eq!(set("state=working:progress=0").1.progress, Some(0));
        assert_eq!(set("state=working:progress=100").1.progress, Some(100));
        assert_eq!(set("state=blocked:progress=007").1.progress, Some(7));
        for bad in ["101", "-1", "+5", "1_0", "1.5", "", "1000"] {
            assert_eq!(
                set(&format!("state=working:progress={bad}")).1.progress,
                None,
                "{bad:?}"
            );
        }
        assert_eq!(set("state=done:progress=50").1.progress, None);
    }

    #[test]
    fn app_outside_its_set_is_absent_but_over_length_discards() {
        assert_eq!(set("state=idle:app=a,b").1.app, None);
        assert_eq!(
            set(&format!("state=idle:app={}", "a".repeat(32)))
                .1
                .app
                .unwrap()
                .len(),
            32
        );
        assert_eq!(
            report(&format!("state=idle:app={}", "a".repeat(33))),
            Err(Reject::ValueTooLong)
        );
    }

    #[test]
    fn text_is_strict_base64_utf8_without_controls() {
        assert_eq!(
            set(&format!("state=done:msg={}", b64("ok")))
                .1
                .msg
                .as_deref(),
            Some("ok")
        );
        assert_eq!(
            set("state=done:msg=b2s").1.msg.as_deref(),
            Some("ok"),
            "padding optional"
        );
        assert_eq!(set("state=done:msg=").1.msg, None, "empty is absent");
        assert_eq!(
            report("state=done:msg=b2t"),
            Err(Reject::BadText),
            "non-zero trailing bits"
        );
        assert_eq!(
            set("state=done:msg=@@@@").1.msg,
            None,
            "outside the value set: pair skipped"
        );
        assert_eq!(report("state=done:msg=a"), Err(Reject::BadText));
        assert_eq!(
            report(&format!("state=done:msg={}", STANDARD.encode([0xff, 0xfe]))),
            Err(Reject::BadText)
        );
        for ctl in ["a\nb", "tab\there", "\u{7f}", "\u{85}", "\u{0}"] {
            assert_eq!(
                report(&format!("state=done:title={}", b64(ctl))),
                Err(Reject::BadText),
                "{ctl:?}"
            );
        }
    }

    #[test]
    fn padding_is_optional_but_must_be_complete_when_present() {
        assert_eq!(set("state=done:title=MA").1.title.as_deref(), Some("0"));
        assert_eq!(set("state=done:title=MA==").1.title.as_deref(), Some("0"));
        for bad in ["MA=", "MA===", "M=A=", "=MA", "b2s=="] {
            assert_eq!(
                report(&format!("state=done:title={bad}")),
                Err(Reject::BadText),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_bad_earlier_duplicate_still_discards() {
        assert_eq!(
            report(&format!("state=done:msg=b2t:msg={}", b64("fine"))),
            Err(Reject::BadText)
        );
        assert_eq!(
            report(&format!("state=done:app={}:app=ok", "a".repeat(40))),
            Err(Reject::ValueTooLong)
        );
    }

    #[test]
    fn limits_are_exact() {
        let at = |key: &str, n: usize| format!("state=done:{key}={}", "A".repeat(n));
        // Encoded caps: msg 2732 (= 4 * 683, decodes to 2049 > 2048 → too long).
        assert_eq!(
            report(&at("msg", MAX_MSG_ENCODED + 4)),
            Err(Reject::ValueTooLong)
        );
        assert_eq!(
            report(&at("title", MAX_TITLE_ENCODED + 4)),
            Err(Reject::ValueTooLong)
        );
        let ok_msg = b64(&"m".repeat(MAX_MSG_DECODED));
        assert!(ok_msg.len() <= MAX_MSG_ENCODED);
        assert_eq!(
            set(&format!("state=done:msg={ok_msg}"))
                .1
                .msg
                .unwrap()
                .len(),
            2048
        );
        let long_msg = b64(&"m".repeat(MAX_MSG_DECODED + 1));
        assert_eq!(
            report(&format!("state=done:msg={long_msg}")),
            Err(Reject::ValueTooLong)
        );
        let ok_title = b64(&"t".repeat(MAX_TITLE_DECODED));
        assert_eq!(
            set(&format!("state=done:title={ok_title}"))
                .1
                .title
                .unwrap()
                .len(),
            192
        );
        let long_title = b64(&"t".repeat(MAX_TITLE_DECODED + 1));
        assert_eq!(
            report(&format!("state=done:title={long_title}")),
            Err(Reject::ValueTooLong)
        );

        assert!(report(&format!("state=done:{}=1", "k".repeat(16))).is_ok());
        assert_eq!(
            report(&format!("state=done:{}=1", "k".repeat(17))),
            Err(Reject::KeyTooLong)
        );

        let big = format!("state=done:x={}", "a".repeat(MAX_PAYLOAD_BYTES));
        assert_eq!(decode(big.as_bytes()), Err(Reject::PayloadTooLong));
        let fits = format!("state=done:x={}", "a".repeat(MAX_PAYLOAD_BYTES - 13));
        assert_eq!(fits.len(), MAX_PAYLOAD_BYTES);
        assert!(decode(fits.as_bytes()).is_ok());
    }

    #[test]
    fn non_utf8_payload_bytes_only_skip_their_pair() {
        let mut p = b"state=working:app=".to_vec();
        p.extend_from_slice(&[0xc3, 0x28]);
        let Ok(Message::Report(Report::Set { record, .. })) = decode(&p) else {
            panic!("report expected");
        };
        assert_eq!(record.app, None);
    }

    mod prop {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
                let _ = decode(&bytes);
            }

            #[test]
            fn accepted_text_is_bounded_and_control_free(
                pairs in proptest::collection::vec(
                    ("(state|msg|title|app|id|kind|progress)", "[ -~]{0,40}"), 0..8)
            ) {
                let payload = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(":");
                if let Ok(Message::Report(Report::Set { record, .. })) = decode(payload.as_bytes()) {
                    for t in [&record.msg, &record.title].into_iter().flatten() {
                        prop_assert!(!t.chars().any(char::is_control));
                        prop_assert!(t.len() <= MAX_MSG_DECODED);
                    }
                    if let Some(app) = &record.app {
                        prop_assert!(app.len() <= MAX_APP_BYTES);
                    }
                }
            }
        }
    }
}
