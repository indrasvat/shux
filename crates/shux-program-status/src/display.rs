//! Disarming program-authored text before shux shows it outside the grid.

/// Removes control characters, line separators, direction overrides, fillers,
/// tag characters and other invisible formatting characters, so a `title` or
/// `msg` cannot reorder, split or hide the text shux puts around it.
pub fn sanitize_for_display(text: &str) -> String {
    text.chars().filter(|&c| !is_disarmed(c)).collect()
}

fn is_disarmed(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E007F}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_fillers_and_tag_characters_are_removed() {
        // Line/paragraph separators break a line; tag characters spell text
        // no human sees but a model reading the output does.
        assert_eq!(sanitize_for_display("top\u{2028}FAKE\u{2029}"), "topFAKE");
        assert_eq!(sanitize_for_display("ok\u{E0041}\u{E0042}\u{E007F}"), "ok");
        assert_eq!(
            sanitize_for_display(
                "a\u{00AD}b\u{034F}c\u{115F}\u{1160}d\u{3164}e\u{FFA0}f\u{180E}g\u{206A}h\u{206F}i"
            ),
            "abcdefghi"
        );
    }

    #[test]
    fn direction_overrides_and_invisibles_are_removed() {
        assert_eq!(sanitize_for_display("ok\u{202E}gpj.exe"), "okgpj.exe");
        assert_eq!(sanitize_for_display("a\u{200B}b\u{2066}c\u{FEFF}"), "abc");
        assert_eq!(sanitize_for_display("tab\tnl\n"), "tabnl");
        assert_eq!(
            sanitize_for_display("Allow Bash(ls)? ✓ 日本"),
            "Allow Bash(ls)? ✓ 日本"
        );
    }
}
