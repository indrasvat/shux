//! Kitty graphics command parsing, and the transports shux refuses.
//!
//! An APC body located by [`super::apc`] is `G<control>;<payload>`. This module
//! reads the control half. It does not decode, store or draw a payload -- the
//! pixel path lands with the image store.
//!
//! Key/value shape adapted from Zellij's `kitty_graphics/parser.rs` (MIT), which
//! the design plan names as the precedent. Every field, default and response
//! rule below is taken from kitty's `docs/graphics-protocol.rst` and checked
//! against `kitty/graphics.c`, not from memory.
//!
//! ## Why the file transports are refused
//!
//! `t=f`, `t=t` and `t=s` put a FILENAME in the payload and ask the terminal to
//! read it as image data. kitty gates that behind a permission callback
//! (`graphics.c:628`). Unimplemented, it is an arbitrary-file-read primitive: a
//! process in any pane sends `a=T,f=32,t=f;` + base64(`~/.ssh/id_rsa`), and the
//! bytes become an image that is re-emitted to every attaching client and
//! composited into `pane.snapshot` -- a file on disk. Refusing the medium
//! removes the class outright, and costs nothing: terminal-browser probes
//! `t=f`/`t=s` and falls back to inline transmit anyway.
//!
//! ## Why nothing is answered yet
//!
//! The design plan's D11 says to refuse the file transports *in a reply*. That
//! is right only once shux can draw. The protocol makes any reply an
//! advertisement of support:
//!
//! > If you get back a response to the graphics query, the terminal emulator
//! > supports the protocol, if you get back a response to the device attributes
//! > query without a response to the graphics query, it does not.
//! > -- `graphics-protocol.rst`, "Querying support and available transmission
//! > mediums"
//!
//! An **error** response is still a response. A client that receives one
//! concludes shux does graphics, stops using its text fallback, and transmits
//! into a terminal with no renderer -- a blank pane where there used to be
//! readable output. So [`error_reply`] is built and tested here, and deliberately
//! not emitted: the replies switch on with the renderer, in one change, so
//! support is advertised exactly when it is real.

/// What the application asked the terminal to do.
///
/// The protocol defines eight (`a, c, d, f, p, q, t, T`); shux implements the
/// image ones and treats animation as a named refusal rather than a parse
/// error, because telling a client "malformed" about a well-formed command it
/// is entitled to send sends it looking for a bug it does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// `a=t` -- transmit image data, do not place it yet. The protocol default.
    Transmit,
    /// `a=T` -- transmit and place at the cursor.
    TransmitAndPlace,
    /// `a=p` -- place an already-transmitted image.
    Put,
    /// `a=d` -- delete images or placements.
    Delete,
    /// `a=q` -- ask whether a transmission would have succeeded.
    Query,
    /// `a=f` transmit animation frame data, `a=a` control animation, `a=c`
    /// compose animation frames. Understood, refused: the design plan ships no
    /// animation.
    Animation,
}

impl Action {
    fn parse(value: &[u8]) -> Option<Self> {
        match value {
            b"t" => Some(Action::Transmit),
            b"T" => Some(Action::TransmitAndPlace),
            b"p" => Some(Action::Put),
            b"d" => Some(Action::Delete),
            b"q" => Some(Action::Query),
            b"f" | b"a" | b"c" => Some(Action::Animation),
            _ => None,
        }
    }
}

/// Where the image bytes are supposed to come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transport {
    /// `t=d` -- inline in the payload. The protocol default, and the only one
    /// shux implements.
    Direct,
    /// `t=f` -- a path to a regular file.
    File,
    /// `t=t` -- a path to a file the terminal should delete afterwards.
    TempFile,
    /// `t=s` -- the name of a POSIX shared-memory object.
    SharedMemory,
}

impl Transport {
    fn parse(value: &[u8]) -> Option<Self> {
        match value {
            b"d" => Some(Transport::Direct),
            b"f" => Some(Transport::File),
            b"t" => Some(Transport::TempFile),
            b"s" => Some(Transport::SharedMemory),
            _ => None,
        }
    }

    /// The medium named in the refusal, so an operator reading a pane's traffic
    /// can tell which probe was answered.
    fn describe(self) -> &'static str {
        match self {
            Transport::Direct => "direct",
            Transport::File => "file",
            Transport::TempFile => "temporary file",
            Transport::SharedMemory => "shared memory",
        }
    }
}

/// How much the application wants to hear back (`q=`).
///
/// Honouring this is not politeness. A reply the application did not ask for
/// arrives on its stdin mid-parse; kitty's own relay clients set `q=2` so they
/// can stop reading. Thresholds are `graphics.c:927` --
/// `if (g->quiet) { if (is_ok_response || g->quiet > 1) return NULL; }` -- so
/// anything at or above 2 is silent, not just the literal `2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Verbosity {
    /// `q=0`, the default -- report successes and errors.
    #[default]
    All,
    /// `q=1` -- suppress success, still report errors.
    ErrorsOnly,
    /// `q>=2` -- say nothing at all.
    Silent,
}

impl Verbosity {
    fn parse(value: &[u8]) -> Option<Self> {
        match number(value)? {
            0 => Some(Verbosity::All),
            1 => Some(Verbosity::ErrorsOnly),
            _ => Some(Verbosity::Silent),
        }
    }

    fn allows_error(self) -> bool {
        self != Verbosity::Silent
    }
}

/// A parsed control block. Payload bytes are deliberately not held here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    pub(crate) action: Action,
    pub(crate) transport: Transport,
    /// `i=` -- chosen by the APPLICATION, so it is unique only within one pane.
    pub(crate) image_id: u32,
    /// `I=` -- image NUMBER, an alternative addressing scheme. Distinct from
    /// `i=`, echoed separately in a reply, and zero when unused.
    pub(crate) image_number: u32,
    pub(crate) verbosity: Verbosity,
}

/// Why a command was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rejection {
    pub(crate) image_id: u32,
    pub(crate) image_number: u32,
    pub(crate) reason: Reason,
    /// Carried from the request: a refusal the application asked not to hear
    /// about is still not sent.
    pub(crate) verbosity: Verbosity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reason {
    /// A transport shux will not implement.
    UnsupportedTransport(Transport),
    /// Animation, in any of its three actions.
    UnsupportedAnimation,
    /// The control block named an action that is not in the protocol.
    Malformed,
}

impl Rejection {
    /// The kitty-protocol message half: a code, a colon, and prose.
    fn message(&self) -> String {
        match &self.reason {
            Reason::UnsupportedTransport(transport) => format!(
                "ENOTSUPPORTED:{} transport is refused; retransmit inline (t=d)",
                transport.describe()
            ),
            Reason::UnsupportedAnimation => {
                "ENOTSUPPORTED:animation is not implemented".to_string()
            }
            Reason::Malformed => "EINVAL:could not parse the graphics control block".to_string(),
        }
    }
}

/// Split `key=value` pairs out of a control block.
///
/// Unknown keys are skipped rather than rejected: the protocol grows, and a
/// terminal that fails a whole command over one unrecognised key is worse for
/// the application than one that ignores it. Keys are single characters in this
/// protocol, so anything longer is skipped too.
fn pairs(control: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    control.split(|b| *b == b',').filter_map(|part| {
        let mut it = part.splitn(2, |b| *b == b'=');
        match (it.next(), it.next()) {
            (Some([key]), Some(value)) => Some((*key, value)),
            _ => None,
        }
    })
}

/// Read one unsigned protocol integer, matching kitty's generated parser
/// (`parse-graphics-command.h`, `READ_UINT`) rather than Rust's defaults.
///
/// kitty scans **at most ten digits** and then requires a `,` or `;`, so an
/// eleventh digit is a malformed control block even when the value it spells is
/// small: `i=00000000001` is an error there, not `1`. It then rejects anything
/// above `u32::MAX` outright instead of wrapping.
///
/// `None` therefore means THE COMMAND IS MALFORMED, not "the key was absent".
/// kitty `return`s from its parser on a bad integer and discards the whole
/// command; defaulting the key instead would act on a command kitty would have
/// thrown away, which is how two terminals end up disagreeing about what a pane
/// asked for.
fn number(value: &[u8]) -> Option<u32> {
    if value.is_empty() || value.len() > 10 || !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(value).ok()?.parse().ok()
}

/// Read one APC body's command half (everything after the leading `G`).
pub(crate) fn parse(command: &[u8]) -> Result<Command, Rejection> {
    // The payload half is deliberately dropped: nothing in this change reads
    // image bytes, and for a refused transport those bytes are a filesystem
    // path. The store that consumes payloads brings its own bounds with it.
    let control = match command.iter().position(|b| *b == b';') {
        Some(i) => &command[..i],
        None => command,
    };

    let mut action = None;
    let mut transport = Transport::Direct;
    let mut image_id = 0;
    let mut image_number = 0;
    let mut verbosity = Verbosity::default();
    let mut malformed = false;

    for (key, value) in pairs(control) {
        match key {
            b'a' => action = Some(Action::parse(value)),
            b't' => match Transport::parse(value) {
                Some(t) => transport = t,
                None => malformed = true,
            },
            // A bad integer discards the command rather than defaulting the
            // key -- see `number`.
            b'i' => match number(value) {
                Some(n) => image_id = n,
                None => malformed = true,
            },
            b'I' => match number(value) {
                Some(n) => image_number = n,
                None => malformed = true,
            },
            b'q' => match Verbosity::parse(value) {
                Some(v) => verbosity = v,
                None => malformed = true,
            },
            _ => {}
        }
    }

    let reject = |reason| {
        Err(Rejection {
            image_id,
            image_number,
            reason,
            verbosity,
        })
    };

    if malformed {
        return reject(Reason::Malformed);
    }

    // Refuse the file-backed media before anything looks at the payload, which
    // for those transports is a PATH. Ordering is load-bearing: behind any check
    // that could return a different error, a future caller could read the path
    // believing it had merely failed to parse.
    if transport != Transport::Direct {
        return reject(Reason::UnsupportedTransport(transport));
    }

    // `a` defaults to `t` (control data reference). A continuation chunk carries
    // only `m=` and payload, and lands on that same default.
    match action {
        Some(Some(Action::Animation)) => return reject(Reason::UnsupportedAnimation),
        Some(None) => return reject(Reason::Malformed),
        _ => {}
    }
    let action = action.flatten().unwrap_or(Action::Transmit);

    Ok(Command {
        action,
        transport,
        image_id,
        image_number,
        verbosity,
    })
}

/// Build the `APC G ... ST` reply for a rejection.
///
/// `None` means "send nothing", which the protocol requires in two distinct
/// cases, both from `graphics.c:924-948`:
///
/// * the application asked for silence (`q>=2`); and
/// * **the command carried neither `i=` nor `I=`** -- kitty's
///   `if (g->id || g->image_number) { ...respond... } return NULL;`. A reply to
///   an unaddressed command is unsolicited data on the application's stdin,
///   which it has no reason to be reading.
///
/// Not wired to the wire yet -- see the module docs on why replying at all has
/// to wait for the renderer.
pub(crate) fn error_reply(rejection: &Rejection) -> Option<Vec<u8>> {
    if !rejection.verbosity.allows_error() {
        return None;
    }
    if rejection.image_id == 0 && rejection.image_number == 0 {
        return None;
    }
    let mut keys = String::new();
    if rejection.image_id != 0 {
        keys.push_str(&format!("i={}", rejection.image_id));
    }
    if rejection.image_number != 0 {
        if !keys.is_empty() {
            keys.push(',');
        }
        keys.push_str(&format!("I={}", rejection.image_number));
    }
    Some(format!("\x1b_G{keys};{}\x1b\\", rejection.message()).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(command: &[u8]) -> Command {
        parse(command).expect("expected the command to parse")
    }

    fn refused(command: &[u8]) -> Rejection {
        parse(command).expect_err("expected the command to be refused")
    }

    #[test]
    fn reads_a_direct_transmit_and_place() {
        let command = ok(b"a=T,f=32,s=10,v=20,i=7,t=d;PAYLOAD");
        assert_eq!(command.action, Action::TransmitAndPlace);
        assert_eq!(command.transport, Transport::Direct);
        assert_eq!(command.image_id, 7);
    }

    /// Both defaults come from the protocol's control data reference: `a`
    /// defaults to `t`, `t` defaults to `d`.
    #[test]
    fn the_protocol_defaults_are_transmit_and_direct() {
        let command = ok(b"f=32,s=1,v=1;PAY");
        assert_eq!(command.action, Action::Transmit);
        assert_eq!(command.transport, Transport::Direct);
    }

    /// The security case. All three file-backed media are refused, and the
    /// payload -- a PATH for these -- is never handed back to a caller.
    #[test]
    fn every_file_backed_transport_is_refused() {
        for (control, transport) in [
            (
                b"a=T,t=f,i=1;L2hvbWUvdS8uc3NoL2lkX3JzYQ==".as_slice(),
                Transport::File,
            ),
            (b"a=T,t=t,i=2;L3RtcC94", Transport::TempFile),
            (b"a=T,t=s,i=3;L3NobS94", Transport::SharedMemory),
        ] {
            assert_eq!(
                refused(control).reason,
                Reason::UnsupportedTransport(transport)
            );
        }
    }

    /// Animation is well-formed protocol shux does not implement, so it must be
    /// refused as unsupported rather than reported as a parse failure.
    #[test]
    fn all_three_animation_actions_are_unsupported_not_malformed() {
        for control in [b"a=f,i=1;D".as_slice(), b"a=a,i=1;D", b"a=c,i=1;D"] {
            assert_eq!(refused(control).reason, Reason::UnsupportedAnimation);
        }
    }

    #[test]
    fn an_action_outside_the_protocol_is_malformed() {
        assert_eq!(refused(b"a=Z,i=8;PAY").reason, Reason::Malformed);
        assert_eq!(refused(b"a=,i=8;PAY").reason, Reason::Malformed);
    }

    #[test]
    fn an_unknown_transport_is_malformed() {
        assert_eq!(refused(b"a=T,t=zz,i=8;PAY").reason, Reason::Malformed);
    }

    /// A refusal must be attributed, or an application juggling several
    /// transmissions cannot tell which one failed.
    #[test]
    fn a_refusal_names_the_medium_and_the_request() {
        let reply = error_reply(&refused(b"a=T,t=f,i=4242;cGF0aA==")).expect("must answer");
        let reply = String::from_utf8(reply).unwrap();
        assert!(reply.starts_with("\x1b_Gi=4242;"), "{reply:?}");
        assert!(reply.ends_with("\x1b\\"), "unterminated: {reply:?}");
        assert!(reply.contains("ENOTSUPPORTED"), "{reply:?}");
        assert!(reply.contains("file transport"), "{reply:?}");
    }

    /// `graphics.c:940-941` prints `i=` then `,I=` -- a client addressing by
    /// image number needs its number back or it cannot match the reply.
    #[test]
    fn a_reply_echoes_whichever_addressing_the_request_used() {
        let by_number = error_reply(&refused(b"a=T,t=f,I=13;cA==")).unwrap();
        assert!(
            String::from_utf8(by_number)
                .unwrap()
                .starts_with("\x1b_GI=13;")
        );

        let by_both = error_reply(&refused(b"a=T,t=f,i=99,I=13;cA==")).unwrap();
        assert!(
            String::from_utf8(by_both)
                .unwrap()
                .starts_with("\x1b_Gi=99,I=13;")
        );
    }

    /// `graphics.c:931` -- `if (g->id || g->image_number)`, else no response.
    /// An unaddressed command gets silence, or the terminal is writing
    /// unsolicited bytes onto an application's stdin.
    #[test]
    fn an_unaddressed_command_is_never_answered() {
        assert_eq!(error_reply(&refused(b"a=T,t=f;cGF0aA==")), None);
        assert_eq!(error_reply(&refused(b"a=T,t=f,i=0,I=0;cGF0aA==")), None);
    }

    /// `graphics.c:927` -- `if (is_ok_response || g->quiet > 1) return NULL`.
    /// At or above 2, not merely equal to it.
    #[test]
    fn quiet_at_or_above_two_suppresses_even_a_refusal() {
        assert_eq!(error_reply(&refused(b"a=T,t=f,i=1,q=2;cA==")), None);
        assert_eq!(error_reply(&refused(b"a=T,t=f,i=1,q=7;cA==")), None);
        assert!(error_reply(&refused(b"a=T,t=f,i=1,q=1;cA==")).is_some());
        assert!(error_reply(&refused(b"a=T,t=f,i=1,q=0;cA==")).is_some());
    }

    #[test]
    fn unknown_keys_are_skipped_not_rejected() {
        assert_eq!(ok(b"a=T,zz=9,X=1,i=3;PAY").image_id, 3);
    }

    /// Only the first chunk of a transmission carries `a=`; continuations
    /// carry `m=` and payload. Those must land on the protocol default rather
    /// than being refused as actionless.
    #[test]
    fn a_continuation_chunk_needs_no_action() {
        assert_eq!(ok(b"m=1;MOREPAYLOAD").action, Action::Transmit);
    }

    /// A pane is free to send absurd numbers; none of them may become an
    /// allocation, a panic, or a wrapped value.
    #[test]
    fn hostile_numbers_are_rejected_rather_than_parsed() {
        assert_eq!(number(b""), None);
        assert_eq!(number(b"99999999999999999999"), None, "over ten digits");
        assert_eq!(number(b"-1"), None);
        assert_eq!(number(b"1e9"), None);
        assert_eq!(number(b" 1"), None);
        assert_eq!(number(b"4294967295"), Some(u32::MAX));
        assert_eq!(number(b"4294967296"), None, "u32 overflow must not wrap");

        // kitty reads ten digits and then demands a separator, so an eleventh
        // makes the block malformed even when the value it spells is small.
        // This is what makes the ten-digit cap load-bearing rather than a
        // micro-optimisation: without it `00000000001` would parse as 1 and
        // shux would act on a command kitty discards.
        assert_eq!(number(b"00000000001"), None, "eleven digits");
        assert_eq!(number(b"0000000001"), Some(1), "ten digits, zero-padded");
        assert_eq!(refused(b"a=T,i=00000000001;PAY").reason, Reason::Malformed);

        // A bad integer discards the whole command; it does not default the key.
        assert_eq!(refused(b"a=T,i=99999999999;PAY").reason, Reason::Malformed);
        assert_eq!(refused(b"a=T,q=nope;PAY").reason, Reason::Malformed);
    }

    #[test]
    fn degenerate_control_blocks_do_not_panic() {
        assert_eq!(parse(b"").map(|c| c.action), Ok(Action::Transmit));
        assert_eq!(parse(b";").map(|c| c.action), Ok(Action::Transmit));
        assert_eq!(parse(b",,,").map(|c| c.action), Ok(Action::Transmit));
        assert_eq!(parse(b"=").map(|c| c.action), Ok(Action::Transmit));
        assert_eq!(parse(b"a").map(|c| c.action), Ok(Action::Transmit));
    }
}
