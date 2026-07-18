//! Vim-notation → PTY bytes for the `keys` step (design D6). Pure, fails closed.
//!
//! A key string mixes literal text and `<...>` tokens, exactly like vim's `:normal`:
//! `":wq<CR>"` → `:wq` + `0x0d`. Unknown `<...>` tokens are an ERROR (fail closed),
//! not sent literally — a typo'd chord must not silently drive the TUI wrong. `<lt>`
//! escapes a literal `<`. Arrow/function keys use xterm "normal" CSI forms.

/// Decode one key string into the bytes to send to the PTY.
pub fn encode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let Some(close) = s[i..].find('>').map(|off| i + off) else {
                return Err(format!("unterminated `<` in key string {s:?}"));
            };
            let token = &s[i + 1..close];
            out.extend_from_slice(&decode_token(token, s)?);
            i = close + 1;
        } else {
            // Copy one UTF-8 scalar's bytes literally.
            let ch_len = utf8_len(bytes[i]);
            out.extend_from_slice(&bytes[i..(i + ch_len).min(bytes.len())]);
            i += ch_len;
        }
    }
    Ok(out)
}

/// Decode all key strings in a chord list, concatenated.
pub fn encode_all(keys: &[String]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for k in keys {
        out.extend_from_slice(&encode(k)?);
    }
    Ok(out)
}

fn utf8_len(first: u8) -> usize {
    match first {
        b if b < 0x80 => 1,
        b if b >> 5 == 0b110 => 2,
        b if b >> 4 == 0b1110 => 3,
        b if b >> 3 == 0b11110 => 4,
        _ => 1,
    }
}

fn decode_token(token: &str, ctx: &str) -> Result<Vec<u8>, String> {
    // Modifier chords: C- (ctrl), M-/A- (alt = ESC prefix), S- (shift).
    if let Some(rest) = strip_ci_prefix(token, "C-") {
        return ctrl(rest, ctx);
    }
    if let Some(rest) = strip_ci_prefix(token, "M-").or_else(|| strip_ci_prefix(token, "A-")) {
        // Alt = ESC then the target (a named key or a single literal char).
        let mut v = vec![0x1b];
        v.extend_from_slice(&decode_named_or_char(rest, ctx)?);
        return Ok(v);
    }
    if let Some(rest) = strip_ci_prefix(token, "S-") {
        // Shift on a single letter = uppercase.
        if rest.chars().count() == 1 {
            return Ok(rest.to_uppercase().into_bytes());
        }
        // Shift on a named key emits the xterm modified CSI form (adv MAJOR: silently
        // dropping the shift sent plain Tab where the scenario asked for BackTab, driving
        // the gate to the WRONG UI state). Unknown shifted named keys fail closed rather
        // than mis-decode.
        return decode_shift_named(rest, ctx);
    }
    // A bare `<...>` must be a NAMED key — an unknown token fails closed.
    decode_named(token).ok_or_else(|| format!("unknown key token <{token}> in {ctx:?}"))
}

/// A shift-modified named key → its xterm CSI form (`<S-Tab>` → BackTab `ESC[Z`;
/// arrows/Home/End → `ESC[1;2X`). Unknown → error (never a silent shift-drop).
fn decode_shift_named(name: &str, ctx: &str) -> Result<Vec<u8>, String> {
    let seq: &[u8] = match name.to_ascii_lowercase().as_str() {
        "tab" => b"\x1b[Z",
        "up" => b"\x1b[1;2A",
        "down" => b"\x1b[1;2B",
        "right" => b"\x1b[1;2C",
        "left" => b"\x1b[1;2D",
        "home" => b"\x1b[1;2H",
        "end" => b"\x1b[1;2F",
        _ => return Err(format!("unsupported shift chord <S-{name}> in {ctx:?}")),
    };
    Ok(seq.to_vec())
}

/// A modifier target: a named key, else a single literal char, else an error.
fn decode_named_or_char(name: &str, ctx: &str) -> Result<Vec<u8>, String> {
    if let Some(b) = decode_named(name) {
        return Ok(b);
    }
    if name.chars().count() == 1 {
        return Ok(name.as_bytes().to_vec());
    }
    Err(format!("unknown key token <{name}> in {ctx:?}"))
}

/// Case-insensitively strip a modifier prefix like `C-`. Byte-based (adv BLOCKER: a
/// string slice `token[..prefix.len()]` PANICS when the boundary lands inside a
/// multibyte char, e.g. `<aé>`). The prefix is ASCII, so a matching prefix length is
/// always a valid char boundary.
fn strip_ci_prefix<'a>(token: &'a str, prefix: &str) -> Option<&'a str> {
    let pn = prefix.len();
    match token.as_bytes().get(..pn) {
        Some(head) if head.eq_ignore_ascii_case(prefix.as_bytes()) => Some(&token[pn..]),
        _ => None,
    }
}

fn ctrl(rest: &str, ctx: &str) -> Result<Vec<u8>, String> {
    // Special control names first.
    let byte = match rest.to_ascii_lowercase().as_str() {
        "space" | "@" => Some(0x00),
        "[" => Some(0x1b),
        "\\" => Some(0x1c),
        "]" => Some(0x1d),
        "^" => Some(0x1e),
        "_" => Some(0x1f),
        _ if rest.len() == 1 && rest.as_bytes()[0].is_ascii_alphabetic() => {
            Some(rest.as_bytes()[0].to_ascii_uppercase() & 0x1f)
        }
        _ => None,
    };
    byte.map(|b| vec![b])
        .ok_or_else(|| format!("unsupported control chord <C-{rest}> in {ctx:?}"))
}

fn decode_named(name: &str) -> Option<Vec<u8>> {
    let seq: &[u8] = match name.to_ascii_lowercase().as_str() {
        "esc" | "escape" => b"\x1b",
        "cr" | "enter" | "return" => b"\r",
        "lf" | "nl" => b"\n",
        "tab" => b"\t",
        "space" => b" ",
        "bs" | "backspace" => b"\x7f",
        "del" | "delete" => b"\x1b[3~",
        "nul" => b"\x00",
        "lt" => b"<",
        "gt" => b">",
        "bslash" => b"\\",
        "bar" => b"|",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "pageup" | "pgup" => b"\x1b[5~",
        "pagedown" | "pgdn" => b"\x1b[6~",
        "insert" | "ins" => b"\x1b[2~",
        "f1" => b"\x1bOP",
        "f2" => b"\x1bOQ",
        "f3" => b"\x1bOR",
        "f4" => b"\x1bOS",
        "f5" => b"\x1b[15~",
        "f6" => b"\x1b[17~",
        "f7" => b"\x1b[18~",
        "f8" => b"\x1b[19~",
        "f9" => b"\x1b[20~",
        "f10" => b"\x1b[21~",
        "f11" => b"\x1b[23~",
        "f12" => b"\x1b[24~",
        _ => return None,
    };
    Some(seq.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_text_passes_through() {
        assert_eq!(encode("gg").unwrap(), b"gg");
        assert_eq!(encode(":wq").unwrap(), b":wq");
    }

    #[test]
    fn ctrl_chords() {
        assert_eq!(encode("<C-c>").unwrap(), vec![0x03]);
        assert_eq!(encode("<C-a>").unwrap(), vec![0x01]);
        assert_eq!(encode("<C-z>").unwrap(), vec![0x1a]);
        // Case-insensitive modifier + special control targets.
        assert_eq!(encode("<c-C>").unwrap(), vec![0x03]);
        assert_eq!(encode("<C-Space>").unwrap(), vec![0x00]);
        assert_eq!(encode("<C-[>").unwrap(), vec![0x1b]);
    }

    #[test]
    fn named_keys() {
        assert_eq!(encode("<Esc>").unwrap(), b"\x1b");
        assert_eq!(encode("<CR>").unwrap(), b"\r");
        assert_eq!(encode("<Tab>").unwrap(), b"\t");
        assert_eq!(encode("<BS>").unwrap(), b"\x7f");
        assert_eq!(encode("<Up>").unwrap(), b"\x1b[A");
        assert_eq!(encode("<F5>").unwrap(), b"\x1b[15~");
    }

    #[test]
    fn alt_is_esc_prefixed() {
        assert_eq!(encode("<M-a>").unwrap(), vec![0x1b, b'a']);
        assert_eq!(encode("<A-x>").unwrap(), vec![0x1b, b'x']);
    }

    #[test]
    fn mixed_literal_and_tokens() {
        assert_eq!(encode(":wq<CR>").unwrap(), b":wq\r".to_vec());
        assert_eq!(encode("i<Esc>").unwrap(), b"i\x1b".to_vec());
    }

    #[test]
    fn lt_escapes_literal_angle() {
        assert_eq!(encode("<lt>foo").unwrap(), b"<foo".to_vec());
    }

    #[test]
    fn unknown_token_fails_closed() {
        assert!(encode("<Teleport>").is_err());
        assert!(encode("<C-нет>").is_err());
        assert!(encode("abc<def").is_err(), "unterminated < is an error");
    }

    #[test]
    fn ascii_then_multibyte_token_fails_closed_not_panics() {
        // adv BLOCKER: `<aé>` used to PANIC (a string slice inside a multibyte char).
        for t in ["<aé>", "<1é>", "<x🦀>", "<z—>"] {
            assert!(encode(t).is_err(), "{t:?} must fail closed, not panic");
        }
    }

    #[test]
    fn shift_named_keys_emit_modified_csi() {
        // adv MAJOR: `<S-Tab>` must be BackTab, not a silently-dropped plain Tab.
        assert_eq!(encode("<S-Tab>").unwrap(), b"\x1b[Z".to_vec());
        assert_eq!(encode("<S-Up>").unwrap(), b"\x1b[1;2A".to_vec());
        assert_eq!(encode("<S-Left>").unwrap(), b"\x1b[1;2D".to_vec());
        assert_eq!(encode("<S-Home>").unwrap(), b"\x1b[1;2H".to_vec());
        // An unsupported shifted named key fails closed (no silent base-form).
        assert!(encode("<S-F1>").is_err());
    }

    #[test]
    fn utf8_literals_survive() {
        assert_eq!(encode("café").unwrap(), "café".as_bytes());
        assert_eq!(encode("🦀").unwrap(), "🦀".as_bytes());
    }

    #[test]
    fn encode_all_concatenates() {
        let keys = vec!["gg".to_string(), "<C-c>".to_string(), ":q<CR>".to_string()];
        assert_eq!(encode_all(&keys).unwrap(), b"gg\x03:q\r".to_vec());
    }
}
