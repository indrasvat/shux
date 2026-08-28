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
//! ## Why nothing is answered
//!
//! shux emits no graphics reply at all, and that is a deliberate match to
//! kitty rather than a gap. kitty's `REPORT_ERROR` is log-only (`graphics.c:28`)
//! and every parse failure in `parse-graphics-command.h` follows it with a bare
//! `return`, so a malformed command produces no wire response there either.
//!
//! It is also the safe default while shux cannot draw. The protocol treats any
//! response as an advertisement of support:
//!
//! > If you get back a response to the graphics query, the terminal emulator
//! > supports the protocol, if you get back a response to the device attributes
//! > query without a response to the graphics query, it does not.
//! > -- `graphics-protocol.rst`, "Querying support and available transmission
//! > mediums"
//!
//! An **error** is still a response. A client that receives one concludes shux
//! does graphics, abandons its text fallback, and transmits into a terminal
//! with no renderer -- a blank pane where there used to be readable output.
//! The replies, and the `q=` verbosity rules that gate them, belong with the
//! renderer that makes them true.
//!
//! ## Where shux is deliberately laxer than kitty
//!
//! kitty discards a whole command when any key it knows carries a malformed
//! value -- `o=`, `d=`, `f=`, `m=`, `S=`, `z=`, `p=`. shux reads five keys and
//! acts on none of the rest, so validating them here would be knowledge with no
//! consumer. The store that reads a payload must validate `m=` in particular:
//! a garbage chunking key is a dropped command in kitty and would otherwise be
//! a live transfer here.

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
}

/// A parsed control block. Payload bytes are deliberately not held here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    pub(crate) action: Action,
}

/// Why a command was refused. No reply is built from it -- see the module docs
/// -- so it carries the reason and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// A transport shux will not implement.
    UnsupportedTransport(Transport),
    /// Animation, in any of its three actions.
    UnsupportedAnimation,
    /// `i=` and `I=` together. `graphics-protocol.rst:839` makes this an error
    /// the terminal must not act on, and kitty checks it before the action
    /// switch and before any transport work (`graphics.c:2568`).
    BothIdAndNumber,
    /// The control block is not one this protocol can express.
    Malformed,
}

/// Split the control block into `key=value` pairs.
///
/// A token that is not exactly `<single byte>=<value>` fails the whole command.
/// kitty's generated parser treats an unreadable key as fatal -- an invalid key
/// character and a key not followed by `=` both `return`
/// (`parse-graphics-command.h:91-101`) -- and skipping them instead made
/// `a=T,` + a space + `t=f` parse as an ordinary direct transmission, so a
/// single stray byte defeated the recognition the file-transport refusal is
/// keyed on.
///
/// Unknown SINGLE-CHARACTER keys are still skipped, which IS laxer than kitty.
/// That is deliberate: the protocol grows by adding keys, and failing a whole
/// command over one shux has not learned about yet is worse for the application
/// than ignoring it.
fn pairs(control: &[u8]) -> Result<Vec<(u8, &[u8])>, Rejection> {
    // An empty control block is the protocol's own default command, not a
    // malformed one: `a` defaults to `t` and `t` to `d`.
    if control.is_empty() {
        return Ok(Vec::new());
    }
    control
        .split(|b| *b == b',')
        .map(|part| {
            let mut it = part.splitn(2, |b| *b == b'=');
            match (it.next(), it.next()) {
                (Some([key]), Some(value)) => Ok((*key, value)),
                // A trailing or doubled comma yields an empty token. kitty's
                // AFTER_VALUE state rejects that too.
                _ => Err(Rejection::Malformed),
            }
        })
        .collect()
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
    // The payload half is deliberately dropped: nothing here reads image bytes,
    // and for a refused transport those bytes are a filesystem path. The store
    // that consumes payloads brings its own bounds with it.
    let control = match command.iter().position(|b| *b == b';') {
        Some(i) => &command[..i],
        None => command,
    };

    let mut action = None;
    let mut transport = Transport::Direct;
    let mut image_id = 0;
    let mut image_number = 0;
    let mut malformed = false;

    for (key, value) in pairs(control)? {
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
            _ => {}
        }
    }

    // Refuse the file-backed media FIRST, before any other verdict can claim
    // the command. A rejection that does not name the transport leaves a caller
    // unable to tell that the payload it holds is a path rather than image
    // data, so this must not sit behind any check that could return a
    // different reason.
    if transport != Transport::Direct {
        return Err(Rejection::UnsupportedTransport(transport));
    }

    if malformed {
        return Err(Rejection::Malformed);
    }

    if image_id != 0 && image_number != 0 {
        return Err(Rejection::BothIdAndNumber);
    }

    // `a` defaults to `t` (control data reference). A continuation chunk carries
    // only `m=` and payload, and lands on that same default.
    match action {
        Some(Some(Action::Animation)) => return Err(Rejection::UnsupportedAnimation),
        Some(None) => return Err(Rejection::Malformed),
        _ => {}
    }

    Ok(Command {
        action: action.flatten().unwrap_or(Action::Transmit),
    })
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
        assert_eq!(
            ok(b"a=T,f=32,s=10,v=20,i=7,t=d;PAYLOAD").action,
            Action::TransmitAndPlace
        );
    }

    /// Both defaults come from the protocol's control data reference: `a`
    /// defaults to `t`, `t` defaults to `d`.
    #[test]
    fn the_protocol_defaults_are_transmit_and_direct() {
        assert_eq!(ok(b"f=32,s=1,v=1;PAY").action, Action::Transmit);
        // `t` defaulting to direct is observable only through what is NOT
        // refused, now that `Command` no longer carries the transport.
        assert!(parse(b"f=32,s=1,v=1;PAY").is_ok());
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
            assert_eq!(refused(control), Rejection::UnsupportedTransport(transport));
        }
    }

    /// The refusal must survive a command whose OTHER keys are bad, or the
    /// rejection loses the one fact a caller needs: that the payload it holds
    /// is a filesystem path rather than image data.
    ///
    /// A block too broken to tokenise is the exception, and it is covered in
    /// `a_malformed_token_fails_the_command_rather_than_being_skipped`.
    #[test]
    fn a_file_transport_is_refused_as_such_even_when_the_block_is_also_malformed() {
        for control in [
            b"a=T,t=f,i=1,q=nope;L2V0Yy9wYXNzd2Q=".as_slice(),
            b"a=T,t=f,i=1,I=99999999999;L2V0Yy9wYXNzd2Q=",
            b"a=T,t=f,i=99999999999;L2V0Yy9wYXNzd2Q=",
        ] {
            assert_eq!(
                refused(control),
                Rejection::UnsupportedTransport(Transport::File),
                "refusal lost the transport for {control:?}"
            );
        }
    }

    /// A stray byte must not turn a named file medium into an ordinary direct
    /// transmission. kitty's parser treats the space as fatal to the command
    /// (`parse-graphics-command.h:91`); skipping the token instead let
    /// `a=T, t=f` through as `Direct`.
    #[test]
    fn a_malformed_token_fails_the_command_rather_than_being_skipped() {
        for control in [
            b"a=T, t=f,i=1;L2V0Yy9wYXNzd2Q=".as_slice(),
            b"a=T,\tt=f,i=1;L2V0Yy9wYXNzd2Q=",
            b"a=T,tt=f,i=1;L2V0Yy9wYXNzd2Q=",
            b"a=T,junk,i=3;PAY",
            b"a=T,,i=3;PAY",
            // A structurally broken token beats the transport refusal, because
            // the block cannot be read far enough to know a medium was named.
            // Safe, and it matches kitty, which discards at the bad token
            // without ever reaching `t=`. The security property is not "the
            // refusal names the medium" but "the payload is never read as a
            // path", and a malformed command is discarded whole.
            b"a=T,t=f,junk,i=1;L2V0Yy9wYXNzd2Q=",
        ] {
            assert_eq!(
                refused(control),
                Rejection::Malformed,
                "token shape accepted for {control:?}"
            );
        }
    }

    /// Animation is well-formed protocol shux does not implement, so it must be
    /// refused as unsupported rather than reported as a parse failure.
    #[test]
    fn all_three_animation_actions_are_unsupported_not_malformed() {
        for control in [b"a=f,i=1;D".as_slice(), b"a=a,i=1;D", b"a=c,i=1;D"] {
            assert_eq!(refused(control), Rejection::UnsupportedAnimation);
        }
    }

    /// `graphics-protocol.rst:839` -- "Specifying both `i` and `I` keys in any
    /// command is an error." kitty checks it at `graphics.c:2568`, before the
    /// action switch and before any transport work.
    #[test]
    fn specifying_both_an_image_id_and_an_image_number_is_an_error() {
        assert_eq!(refused(b"a=T,i=1,I=2;D"), Rejection::BothIdAndNumber);
        // Either alone is fine, and zero means "unset" for both.
        assert!(parse(b"a=T,i=1;D").is_ok());
        assert!(parse(b"a=T,I=2;D").is_ok());
        assert!(parse(b"a=T,i=0,I=2;D").is_ok());
        assert!(parse(b"a=T,i=1,I=0;D").is_ok());
    }

    #[test]
    fn an_action_outside_the_protocol_is_malformed() {
        assert_eq!(refused(b"a=Z,i=8;PAY"), Rejection::Malformed);
        assert_eq!(refused(b"a=,i=8;PAY"), Rejection::Malformed);
    }

    #[test]
    fn an_unknown_transport_is_malformed() {
        assert_eq!(refused(b"a=T,t=zz,i=8;PAY"), Rejection::Malformed);
    }

    #[test]
    fn unknown_single_character_keys_are_skipped_not_rejected() {
        assert!(parse(b"a=T,W=1,X=9,i=3;PAY").is_ok());
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
        assert_eq!(refused(b"a=T,i=00000000001;PAY"), Rejection::Malformed);

        // A bad integer discards the whole command; it does not default the key.
        assert_eq!(refused(b"a=T,i=99999999999;PAY"), Rejection::Malformed);
    }

    #[test]
    fn degenerate_control_blocks_do_not_panic() {
        assert_eq!(parse(b"").map(|c| c.action), Ok(Action::Transmit));
        assert_eq!(parse(b";").map(|c| c.action), Ok(Action::Transmit));
        assert_eq!(parse(b"a").map(|c| c.action), Err(Rejection::Malformed));
        assert_eq!(parse(b"=").map(|c| c.action), Err(Rejection::Malformed));
        assert_eq!(parse(b",,,").map(|c| c.action), Err(Rejection::Malformed));
    }
}
