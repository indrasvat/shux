//! Entity id **references**: a full UUID, or an unambiguous prefix of one.
//!
//! # Why this exists (issue #120)
//!
//! Every human-readable surface shux prints — `session list`, `pane list`, and
//! the one-line success banners — shows an entity's id truncated to its first
//! 8 hex characters, the same convention git uses for commit SHAs. Nothing
//! accepted that form back: id parameters went straight to `Uuid::parse_str`,
//! which rejected 8 characters as malformed. So the id a person could read was
//! never an id they could type, and the documented lens loop (`lens run` →
//! `pane wait-settled` → `pane glance`) was unfollowable from the listing it
//! starts with.
//!
//! This module is the single place that decides what an *id reference* means.
//! Both the daemon (RPC parameters) and the CLI (`-s` / `-w` / `-p`) route
//! through it, so the two cannot disagree about which id a given string names.
//!
//! It deliberately does NOT know about names. Sessions and windows also answer
//! to a name, and that resolution lives with the caller — the CLI tries an
//! exact name first and only then asks this module, because an exact match
//! must beat a partial one. A daemon parameter literally called `id` takes an
//! id and nothing else.
//!
//! # The rules
//!
//! 1. **A complete UUID always wins, and is never checked against the graph.**
//!    Every form `Uuid::parse_str` accepts is a complete UUID: hyphenated,
//!    32-hex "simple", braced, `urn:uuid:`, upper or lower case. Resolution
//!    returns it verbatim, exactly as before this module existed, so no call
//!    that works today can change behaviour or error message. Whether the
//!    entity exists stays the handler's question to answer.
//! 2. **Anything else is a prefix**, after trimming surrounding whitespace,
//!    removing hyphens and lowercasing. It must be [`MIN_PREFIX_LEN`]..=31 hex
//!    digits. A string that normalizes to a full 32 is a UUID with its hyphens
//!    in the wrong places — rule 1 already took every well-formed spelling, so
//!    what is left is malformed, not a prefix.
//! 3. A prefix matches an entity when it is a prefix of that entity's id in
//!    **hyphen-free** form. Exactly one match resolves; none is
//!    [`RefError::NotFound`]; more than one is [`RefError::Ambiguous`], which
//!    names the candidates rather than silently picking one.
//!
//! [`MIN_PREFIX_LEN`] is 4, matching git's floor for abbreviated SHAs. Three
//! characters out of a 32-character space is 1 part in 4096 — close enough to
//! "any typo hits something" that resolving it would be a footgun, and short
//! enough that no listing ever prints it.

use std::fmt;

use uuid::Uuid;

/// Shortest id prefix that may resolve. Below this, input is rejected as
/// malformed even when it would happen to be unique — see the module docs.
pub const MIN_PREFIX_LEN: usize = 4;

/// A UUID has 32 hex digits once its hyphens are removed.
pub const UUID_HEX_LEN: usize = 32;

/// How many colliding candidates an [`RefError::Ambiguous`] lists before it
/// starts counting instead. Long enough to disambiguate by eye, short enough
/// that a pathological graph cannot turn one error into a wall of text.
pub const MAX_LISTED_CANDIDATES: usize = 8;

/// Which kind of entity a reference names. Drives the error wording and the
/// RPC `resource` field, so the caller is told *what* was not found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    Session,
    Window,
    Pane,
}

impl RefKind {
    /// Singular noun, as used in errors and as the RPC `resource` value.
    pub const fn as_str(self) -> &'static str {
        match self {
            RefKind::Session => "session",
            RefKind::Window => "window",
            RefKind::Pane => "pane",
        }
    }
}

impl fmt::Display for RefKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an id reference could not even be interpreted, let alone looked up.
///
/// Kept separate from "looked up and found nothing": a caller that typo'd the
/// syntax needs a different fix from one that named a pane which has since
/// exited, and agents branch on the distinction via the RPC error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MalformedReason {
    /// Empty or whitespace-only.
    Empty,
    /// Non-empty, but nothing survives hyphen-stripping (e.g. `"----"`).
    NoDigits,
    /// Fewer than [`MIN_PREFIX_LEN`] hex digits after normalization.
    TooShort { len: usize },
    /// Contains something that is not a hex digit (hyphens are stripped first).
    NotHex,
    /// More hex digits than a UUID has, so it cannot be a prefix of one.
    TooLong { len: usize },
}

impl fmt::Display for MalformedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MalformedReason::Empty => f.write_str("it is empty"),
            MalformedReason::NoDigits => f.write_str("it is all hyphens, with no hex digits"),
            MalformedReason::TooShort { len } => write!(
                f,
                "an id prefix needs at least {MIN_PREFIX_LEN} hex characters, got {len}"
            ),
            MalformedReason::NotHex => f.write_str("an id is hex digits and hyphens only"),
            MalformedReason::TooLong { len } => write!(
                f,
                "an id prefix is at most {} hex characters (a whole uuid is {UUID_HEX_LEN}), got {len}",
                UUID_HEX_LEN - 1
            ),
        }
    }
}

/// Everything that can go wrong turning a string into an entity id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefError {
    /// The string is not a UUID and cannot be a prefix of one.
    #[error("{kind} '{}' is not a uuid or an id prefix: {reason}", .input.escape_debug())]
    Malformed {
        kind: RefKind,
        input: String,
        reason: MalformedReason,
    },

    /// A well-formed prefix that matches no live entity.
    #[error("no {kind} has an id starting with '{}'", .input.escape_debug())]
    NotFound { kind: RefKind, input: String },

    /// A well-formed prefix that matches more than one live entity. Resolving
    /// it to any single one of them would be a coin flip on the caller's
    /// behalf, so it is refused and the candidates are named.
    #[error(
        "{kind} id '{}' is ambiguous: {total} {kind}s share that prefix ({}{}). \
         Use more characters.",
        .input.escape_debug(),
        .candidates.join(", "),
        if *.total > .candidates.len() { ", …" } else { "" },
    )]
    Ambiguous {
        kind: RefKind,
        input: String,
        /// Full hyphenated ids of the colliding entities, sorted, capped at
        /// [`MAX_LISTED_CANDIDATES`].
        candidates: Vec<String>,
        /// How many collided in total, which may exceed `candidates.len()`.
        total: usize,
    },
}

impl RefError {
    /// The entity kind this error is about.
    pub fn kind(&self) -> RefKind {
        match self {
            RefError::Malformed { kind, .. }
            | RefError::NotFound { kind, .. }
            | RefError::Ambiguous { kind, .. } => *kind,
        }
    }

    /// The reference the caller supplied, verbatim.
    pub fn input(&self) -> &str {
        match self {
            RefError::Malformed { input, .. }
            | RefError::NotFound { input, .. }
            | RefError::Ambiguous { input, .. } => input,
        }
    }
}

/// What a reference string turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedRef {
    /// A complete UUID. Returned verbatim without consulting any graph.
    Exact(Uuid),
    /// A normalized hex prefix: lowercase, no hyphens,
    /// [`MIN_PREFIX_LEN`]..=31 characters.
    Prefix(String),
}

/// Classify a reference string without looking anything up.
///
/// Split out from [`resolve_ref`] so callers that hold their candidate set
/// somewhere other than a [`crate::graph::SessionGraphSnapshot`] — the CLI
/// resolves against a `session.list` response — reuse the exact same syntax
/// rules instead of writing a second, subtly different parser.
pub fn parse_ref(kind: RefKind, input: &str) -> Result<ParsedRef, RefError> {
    let trimmed = input.trim();

    // Rule 1: a complete UUID in any accepted form short-circuits. This must
    // stay first — it is what makes the whole feature additive.
    if let Ok(uuid) = Uuid::parse_str(trimmed) {
        return Ok(ParsedRef::Exact(uuid));
    }

    let malformed = |reason| RefError::Malformed {
        kind,
        input: input.to_string(),
        reason,
    };

    if trimmed.is_empty() {
        return Err(malformed(MalformedReason::Empty));
    }

    // Hyphens are noise in a prefix: a half-pasted UUID ("b57c601b-5f61")
    // carries one, and where it falls tells us nothing the digits do not.
    let hex: String = trimmed
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();

    if hex.is_empty() {
        return Err(malformed(MalformedReason::NoDigits));
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(malformed(MalformedReason::NotHex));
    }
    if hex.len() < MIN_PREFIX_LEN {
        return Err(malformed(MalformedReason::TooShort { len: hex.len() }));
    }
    // `>=`, not `>`: 32 hex digits are a whole UUID's worth. Every spelling
    // `Uuid::parse_str` accepts was taken by rule 1, so reaching here with 32
    // means the hyphens are in non-UUID positions — malformed, not a prefix.
    if hex.len() >= UUID_HEX_LEN {
        return Err(malformed(MalformedReason::TooLong { len: hex.len() }));
    }

    Ok(ParsedRef::Prefix(hex))
}

/// Match an already-parsed prefix against a set of candidate ids.
///
/// `input` is carried through only so the error can quote what the caller
/// actually typed rather than the normalized form.
pub fn match_prefix<I>(kind: RefKind, input: &str, prefix: &str, ids: I) -> Result<Uuid, RefError>
where
    I: IntoIterator<Item = Uuid>,
{
    let mut hits: Vec<Uuid> = ids
        .into_iter()
        .filter(|id| id.simple().to_string().starts_with(prefix))
        .collect();

    match hits.len() {
        0 => Err(RefError::NotFound {
            kind,
            input: input.trim().to_string(),
        }),
        1 => Ok(hits.remove(0)),
        total => {
            // Sorted so the same collision always reports the same list — an
            // error a caller cannot reproduce is an error they cannot act on.
            hits.sort_unstable();
            let candidates = hits
                .iter()
                .take(MAX_LISTED_CANDIDATES)
                .map(|u| u.hyphenated().to_string())
                .collect();
            Err(RefError::Ambiguous {
                kind,
                input: input.trim().to_string(),
                candidates,
                total,
            })
        }
    }
}

/// Resolve a reference string against a candidate id set.
///
/// A complete UUID is returned without consulting `ids` at all — see rule 1 in
/// the module docs. Only prefixes are looked up.
pub fn resolve_ref<I>(kind: RefKind, input: &str, ids: I) -> Result<Uuid, RefError>
where
    I: IntoIterator<Item = Uuid>,
{
    match parse_ref(kind, input)? {
        ParsedRef::Exact(uuid) => Ok(uuid),
        ParsedRef::Prefix(prefix) => match_prefix(kind, input, &prefix, ids),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Uuid {
        Uuid::parse_str(s).expect("test uuid")
    }

    /// Four ids sharing progressively longer prefixes, so every boundary in
    /// the matching rule has something to bite on.
    fn fleet() -> Vec<Uuid> {
        vec![
            u("aaaa1111-0000-4000-8000-000000000001"),
            u("aaaa2222-0000-4000-8000-000000000002"),
            u("aaab3333-0000-4000-8000-000000000003"),
            u("bbbb4444-0000-4000-8000-000000000004"),
        ]
    }

    // ── Rule 1: a complete UUID is passed through untouched ──────────────

    #[test]
    fn full_uuid_resolves_without_consulting_the_graph() {
        // The empty candidate set is the point: a complete UUID must not
        // depend on graph membership, so behaviour for existing callers is
        // bit-for-bit what it was before this module.
        let id = u("aaaa1111-0000-4000-8000-000000000001");
        assert_eq!(
            resolve_ref(RefKind::Pane, &id.to_string(), std::iter::empty()),
            Ok(id)
        );
    }

    #[test]
    fn every_uuid_spelling_uuid_crate_accepts_is_still_exact() {
        let id = u("aaaa1111-0000-4000-8000-000000000001");
        for spelling in [
            "aaaa1111-0000-4000-8000-000000000001",
            "AAAA1111-0000-4000-8000-000000000001",
            "aaaa1111000040008000000000000001",
            "{aaaa1111-0000-4000-8000-000000000001}",
            "urn:uuid:aaaa1111-0000-4000-8000-000000000001",
        ] {
            assert_eq!(
                parse_ref(RefKind::Pane, spelling),
                Ok(ParsedRef::Exact(id)),
                "{spelling} should parse as a complete uuid"
            );
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        let id = u("aaaa1111-0000-4000-8000-000000000001");
        assert_eq!(
            resolve_ref(
                RefKind::Pane,
                "  aaaa1111-0000-4000-8000-000000000001\n",
                vec![id]
            ),
            Ok(id)
        );
        // …and a prefix pasted with the newline a shell pipeline leaves on it.
        assert_eq!(resolve_ref(RefKind::Pane, "\taaaa1111 \n", fleet()), Ok(id));
        // Errors quote the trimmed form, not the whitespace-padded original.
        assert_eq!(
            resolve_ref(RefKind::Pane, "  cccc \n", fleet()),
            Err(RefError::NotFound {
                kind: RefKind::Pane,
                input: "cccc".to_string(),
            })
        );
    }

    // ── Rule 2: prefix syntax ────────────────────────────────────────────

    #[test]
    fn short_id_from_a_listing_resolves() {
        assert_eq!(
            resolve_ref(RefKind::Pane, "bbbb4444", fleet()),
            Ok(u("bbbb4444-0000-4000-8000-000000000004"))
        );
    }

    #[test]
    fn minimum_length_is_four_and_three_is_refused() {
        assert_eq!(
            resolve_ref(RefKind::Pane, "bbbb", fleet()),
            Ok(u("bbbb4444-0000-4000-8000-000000000004"))
        );
        assert_eq!(
            resolve_ref(RefKind::Pane, "bbb", fleet()),
            Err(RefError::Malformed {
                kind: RefKind::Pane,
                input: "bbb".to_string(),
                reason: MalformedReason::TooShort { len: 3 },
            })
        );
    }

    #[test]
    fn a_unique_but_too_short_prefix_is_still_refused() {
        // "b" matches exactly one id here. It is *still* rejected: the floor
        // is about typo blast radius, not about whether this graph happens to
        // be small right now.
        assert!(matches!(
            resolve_ref(RefKind::Pane, "b", fleet()),
            Err(RefError::Malformed {
                reason: MalformedReason::TooShort { len: 1 },
                ..
            })
        ));
    }

    #[test]
    fn prefixes_are_case_insensitive() {
        assert_eq!(
            resolve_ref(RefKind::Pane, "BBBB4444", fleet()),
            Ok(u("bbbb4444-0000-4000-8000-000000000004"))
        );
    }

    #[test]
    fn hyphens_anywhere_are_stripped_before_matching() {
        for spelling in [
            "bbbb4444-0000",
            "bbbb-4444-0000",
            "-bbbb4444-",
            "bbbb4444-0000-4",
        ] {
            assert_eq!(
                resolve_ref(RefKind::Pane, spelling, fleet()),
                Ok(u("bbbb4444-0000-4000-8000-000000000004")),
                "{spelling} should match"
            );
        }
    }

    #[test]
    fn a_31_hex_prefix_is_a_prefix_not_a_uuid() {
        let id = u("bbbb4444-0000-4000-8000-000000000004");
        let thirty_one = &id.simple().to_string()[..31];
        assert_eq!(
            parse_ref(RefKind::Pane, thirty_one),
            Ok(ParsedRef::Prefix(thirty_one.to_string()))
        );
        assert_eq!(resolve_ref(RefKind::Pane, thirty_one, fleet()), Ok(id));
    }

    /// A whole UUID's worth of hex with the hyphens in the wrong places is a
    /// MALFORMED uuid, not a 32-character prefix. `Uuid::parse_str` already
    /// took every well-formed spelling, so anything left at 32 is a typo — and
    /// accepting it would contradict the documented 4..=31 range.
    #[test]
    fn thirty_two_hex_with_misplaced_hyphens_is_malformed_not_a_prefix() {
        for bad in [
            "bbbb4444-00-00-4000-8000-000000000004", // hyphens shifted
            "-bbbb4444-0000-4000-8000-000000000004", // leading hyphen
            "bbbb4444-0000-4000-8000-000000000004-", // trailing hyphen
            "bbbb44440000400080000000000000-04",     // one hyphen, wrong spot
        ] {
            assert!(
                matches!(
                    parse_ref(RefKind::Pane, bad),
                    Err(RefError::Malformed {
                        reason: MalformedReason::TooLong { len: 32 },
                        ..
                    })
                ),
                "{bad} normalizes to 32 hex and must be malformed, got {:?}",
                parse_ref(RefKind::Pane, bad)
            );
        }
        // …while the same id spelled correctly still resolves.
        assert_eq!(
            resolve_ref(
                RefKind::Pane,
                "bbbb4444-0000-4000-8000-000000000004",
                fleet()
            ),
            Ok(u("bbbb4444-0000-4000-8000-000000000004"))
        );
    }

    #[test]
    fn non_hex_is_malformed() {
        for bad in ["zzzzzzzz", "not-an-id", "aaaa111g", "b57c601b!"] {
            assert!(
                matches!(
                    parse_ref(RefKind::Pane, bad),
                    Err(RefError::Malformed {
                        reason: MalformedReason::NotHex,
                        ..
                    })
                ),
                "{bad} should be NotHex"
            );
        }
    }

    #[test]
    fn empty_and_whitespace_are_malformed_as_empty() {
        for bad in ["", "   ", "\t\n"] {
            assert!(
                matches!(
                    parse_ref(RefKind::Pane, bad),
                    Err(RefError::Malformed {
                        reason: MalformedReason::Empty,
                        ..
                    })
                ),
                "{bad:?} should be Empty"
            );
        }
    }

    /// A string of hyphens is not empty, and saying "it is empty" to someone
    /// looking at `----` reads as a bug in the error rather than in the input.
    #[test]
    fn a_string_of_only_hyphens_says_so_rather_than_claiming_to_be_empty() {
        for bad in ["-", "----", " --- "] {
            let err = parse_ref(RefKind::Pane, bad).unwrap_err();
            assert!(
                matches!(
                    err,
                    RefError::Malformed {
                        reason: MalformedReason::NoDigits,
                        ..
                    }
                ),
                "{bad:?} should be NoDigits, got {err:?}"
            );
            assert!(
                !err.to_string().contains("is empty"),
                "{bad:?} is not empty: {err}"
            );
        }
    }

    #[test]
    fn longer_than_a_uuid_is_malformed() {
        let too_long = "a".repeat(33);
        assert!(matches!(
            parse_ref(RefKind::Pane, &too_long),
            Err(RefError::Malformed {
                reason: MalformedReason::TooLong { len: 33 },
                ..
            })
        ));
        // The message must name the real ceiling for a prefix, not the uuid
        // length — a reader who typed 32 needs to know 31 is the limit.
        let msg = parse_ref(RefKind::Pane, &too_long).unwrap_err().to_string();
        assert!(msg.contains("31"), "{msg}");
    }

    // ── Rule 3: matching, and refusing to guess ──────────────────────────

    #[test]
    fn an_unmatched_prefix_is_not_found_not_malformed() {
        assert_eq!(
            resolve_ref(RefKind::Pane, "cccc", fleet()),
            Err(RefError::NotFound {
                kind: RefKind::Pane,
                input: "cccc".to_string(),
            })
        );
    }

    #[test]
    fn an_empty_graph_is_not_found_not_a_panic() {
        assert_eq!(
            resolve_ref(RefKind::Window, "aaaa", Vec::new()),
            Err(RefError::NotFound {
                kind: RefKind::Window,
                input: "aaaa".to_string(),
            })
        );
    }

    #[test]
    fn a_colliding_prefix_names_the_candidates_instead_of_guessing() {
        let err = resolve_ref(RefKind::Pane, "aaaa", fleet()).unwrap_err();
        match err {
            RefError::Ambiguous {
                kind,
                input,
                candidates,
                total,
            } => {
                assert_eq!(kind, RefKind::Pane);
                assert_eq!(input, "aaaa");
                assert_eq!(total, 2);
                assert_eq!(
                    candidates,
                    vec![
                        "aaaa1111-0000-4000-8000-000000000001".to_string(),
                        "aaaa2222-0000-4000-8000-000000000002".to_string(),
                    ]
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn one_more_character_disambiguates() {
        assert_eq!(
            resolve_ref(RefKind::Pane, "aaaa1", fleet()),
            Ok(u("aaaa1111-0000-4000-8000-000000000001"))
        );
        // And the shorter prefix that spans all three "aaa" ids collides on
        // all three, proving the count is real and not hardcoded.
        match resolve_ref(RefKind::Pane, "aaa", fleet()) {
            Err(RefError::Malformed { .. }) => {} // 3 chars — below the floor
            other => panic!("expected the length floor to fire first, got {other:?}"),
        }
        match resolve_ref(RefKind::Pane, "aaab", fleet()) {
            Ok(id) => assert_eq!(id, u("aaab3333-0000-4000-8000-000000000003")),
            other => panic!("expected a unique match, got {other:?}"),
        }
    }

    #[test]
    fn ambiguity_candidates_are_sorted_and_capped() {
        // 12 ids sharing "dead", more than MAX_LISTED_CANDIDATES.
        let ids: Vec<Uuid> = (0..12)
            .map(|i| u(&format!("dead{i:04x}-0000-4000-8000-00000000000{i:x}")))
            .collect();
        let err = resolve_ref(RefKind::Session, "dead", ids).unwrap_err();
        match err {
            RefError::Ambiguous {
                candidates, total, ..
            } => {
                assert_eq!(total, 12);
                assert_eq!(candidates.len(), MAX_LISTED_CANDIDATES);
                let mut sorted = candidates.clone();
                sorted.sort();
                assert_eq!(candidates, sorted, "candidates must be sorted");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn ambiguity_message_says_what_to_do_about_it() {
        let err = resolve_ref(RefKind::Pane, "aaaa", fleet()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(
            msg.contains("aaaa1111-0000-4000-8000-000000000001"),
            "{msg}"
        );
        assert!(msg.contains("Use more characters"), "{msg}");
    }

    #[test]
    fn a_capped_ambiguity_message_marks_the_truncation() {
        let ids: Vec<Uuid> = (0..12)
            .map(|i| u(&format!("dead{i:04x}-0000-4000-8000-00000000000{i:x}")))
            .collect();
        let msg = resolve_ref(RefKind::Session, "dead", ids)
            .unwrap_err()
            .to_string();
        assert!(msg.contains('…'), "truncated list must be marked: {msg}");
        assert!(msg.contains("12 sessions"), "{msg}");
    }

    // ── Kind plumbing ────────────────────────────────────────────────────

    #[test]
    fn error_wording_names_the_entity_kind() {
        for (kind, noun) in [
            (RefKind::Session, "session"),
            (RefKind::Window, "window"),
            (RefKind::Pane, "pane"),
        ] {
            assert_eq!(kind.as_str(), noun);
            let msg = resolve_ref(kind, "cccc", fleet()).unwrap_err().to_string();
            assert!(msg.contains(noun), "{msg} should name a {noun}");
        }
    }

    #[test]
    fn accessors_expose_kind_and_input_for_every_variant() {
        let malformed = parse_ref(RefKind::Window, "zz").unwrap_err();
        assert_eq!(malformed.kind(), RefKind::Window);
        assert_eq!(malformed.input(), "zz");

        let not_found = resolve_ref(RefKind::Pane, "cccc", fleet()).unwrap_err();
        assert_eq!(not_found.kind(), RefKind::Pane);
        assert_eq!(not_found.input(), "cccc");

        let ambiguous = resolve_ref(RefKind::Session, "aaaa", fleet()).unwrap_err();
        assert_eq!(ambiguous.kind(), RefKind::Session);
        assert_eq!(ambiguous.input(), "aaaa");
    }

    #[test]
    fn malformed_error_quotes_control_characters_rather_than_emitting_them() {
        // Ingress guard (issue #104): a reference is echoed back in an error
        // that may land on a terminal, so it must not carry raw escapes.
        let err = parse_ref(RefKind::Pane, "\u{1b}]0;x\u{7}").unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains('\u{1b}'), "raw ESC leaked into the message");
        assert!(!msg.contains('\u{7}'), "raw BEL leaked into the message");
    }
}
