//! Disarming program-authored text before shux shows it outside the grid.

/// Removes control characters, direction overrides and other invisible
/// formatting characters, so a `title` or `msg` cannot reorder or hide the
/// text shux puts around it.
pub fn sanitize_for_display(text: &str) -> String {
    text.chars().filter(|&c| !is_disarmed(c)).collect()
}

fn is_disarmed(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}')
}

#[cfg(test)]
mod tests {
    use super::*;

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
