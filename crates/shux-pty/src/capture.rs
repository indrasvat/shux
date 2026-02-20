//! ANSI escape sequence stripping for clean text capture.
//!
//! Handles CSI, OSC, DCS, character set designation, and 8-bit CSI sequences.

/// Strip ANSI escape sequences from a string, returning clean text.
///
/// Handles:
/// - CSI sequences: `ESC [ ... final_byte`
/// - OSC sequences: `ESC ] ... BEL` or `ESC ] ... ST`
/// - DCS sequences: `ESC P ... ST`
/// - Character set designation: `ESC ( C`, `ESC ) C`, etc.
/// - Single-character escapes: `ESC M`, `ESC 7`, `ESC 8`, etc.
/// - 8-bit CSI (0x9B): `\x9B ... final_byte`
pub fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // ESC sequence
            match chars.peek() {
                Some('[') => {
                    // CSI sequence: ESC [ ... final_byte (0x40-0x7E)
                    chars.next(); // consume '['
                    while let Some(&c) = chars.peek() {
                        if c.is_ascii_alphabetic()
                            || c == '@'
                            || c == '`'
                            || c == '{'
                            || c == '|'
                            || c == '}'
                            || c == '~'
                        {
                            chars.next(); // consume final byte
                            break;
                        }
                        chars.next(); // consume parameter/intermediate byte
                    }
                }
                Some(']') => {
                    // OSC sequence: ESC ] ... ST (BEL or ESC \)
                    chars.next(); // consume ']'
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next(); // consume '\'
                            }
                            break;
                        }
                    }
                }
                Some('P') => {
                    // DCS sequence: ESC P ... ST (ESC \)
                    chars.next(); // consume 'P'
                    while let Some(c) = chars.next() {
                        if c == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some('(') | Some(')') | Some('*') | Some('+') => {
                    // Designate character set: ESC ( C, ESC ) C, etc.
                    chars.next(); // consume designator
                    chars.next(); // consume charset selector
                }
                Some(_) => {
                    // Single-character escape (e.g., ESC M, ESC 7, ESC 8)
                    chars.next();
                }
                None => {}
            }
        } else if ch == '\u{9b}' {
            // 8-bit CSI: skip like CSI above
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphabetic()
                    || c == '@'
                    || c == '`'
                    || c == '{'
                    || c == '|'
                    || c == '}'
                    || c == '~'
                {
                    chars.next();
                    break;
                }
                chars.next();
            }
        } else {
            output.push(ch);
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_removes_csi() {
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
    }

    #[test]
    fn test_strip_ansi_removes_sgr_256() {
        assert_eq!(strip_ansi("\x1b[38;5;196mcolored\x1b[0m"), "colored");
    }

    #[test]
    fn test_strip_ansi_removes_osc() {
        assert_eq!(strip_ansi("\x1b]0;title\x07text"), "text");
    }

    #[test]
    fn test_strip_ansi_removes_osc_st() {
        assert_eq!(strip_ansi("\x1b]2;title\x1b\\text"), "text");
    }

    #[test]
    fn test_strip_ansi_removes_dcs() {
        assert_eq!(strip_ansi("\x1bPdata\x1b\\text"), "text");
    }

    #[test]
    fn test_strip_ansi_preserves_plain_text() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn test_strip_ansi_preserves_newlines() {
        assert_eq!(strip_ansi("line1\nline2\n"), "line1\nline2\n");
    }

    #[test]
    fn test_strip_ansi_handles_multiline() {
        let input = "\x1b[32mline1\x1b[0m\nline2\n\x1b[1mline3\x1b[0m";
        assert_eq!(strip_ansi(input), "line1\nline2\nline3");
    }

    #[test]
    fn test_strip_ansi_charset_designation() {
        assert_eq!(strip_ansi("\x1b(Bhello"), "hello");
    }

    #[test]
    fn test_strip_ansi_cursor_movement() {
        assert_eq!(strip_ansi("\x1b[5;10Htext"), "text");
    }

    #[test]
    fn test_strip_ansi_8bit_csi() {
        assert_eq!(strip_ansi("\u{9b}31mhello\u{9b}0m"), "hello");
    }

    #[test]
    fn test_strip_ansi_empty_string() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn test_strip_ansi_only_escapes() {
        assert_eq!(strip_ansi("\x1b[31m\x1b[0m"), "");
    }

    #[test]
    fn test_strip_ansi_single_char_escape() {
        // ESC M = Reverse Index
        assert_eq!(strip_ansi("\x1bMtext"), "text");
    }
}
