//! The `Ctrl+/` comment toggle, as plain string work.
//!
//! The markers come from the GtkSourceView language's metadata, so this module never needs to
//! know which language it is looking at — and never needs to touch a buffer.

/// Whether every non-blank line of `text` carries `marker` already, which is when a toggle takes
/// it off: a selection only half commented is commented whole. Asked once over all the lines
/// before any is changed, so a column of carets goes one way, as VS Code's does.
pub fn commented(text: &str, marker: &str) -> bool {
    text.split('\n')
        .filter(|l| !l.trim().is_empty())
        .all(|l| l.trim_start().starts_with(marker))
}

/// `text`, whose lines are the ones the user selected, with a line comment put on (`on`) or taken
/// off. It goes on at the shallowest indentation in the selection so the block stays aligned, and
/// comes off with one space after it. Blank lines are left alone, and the trailing newline
/// structure of the input is preserved.
pub fn comment_lines(text: &str, marker: &str, on: bool) -> String {
    // `split('\n')` and `join("\n")` round-trip exactly, trailing newline included.
    let lines: Vec<&str> = text.split('\n').collect();
    let common = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| indent(l))
        .min();
    let Some(common) = common else {
        return text.to_string();
    };
    let out: Vec<String> = lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                return line.to_string();
            }
            if on {
                return format!("{}{marker} {}", &line[..common], &line[common..]);
            }
            let (lead, rest) = line.split_at(indent(line));
            match rest.strip_prefix(marker) {
                Some(rest) => format!("{lead}{}", rest.strip_prefix(' ').unwrap_or(rest)),
                None => line.to_string(),
            }
        })
        .collect();
    out.join("\n")
}

/// Toggle a block comment around `text`.
///
/// Unwraps when the text already sits inside the markers, otherwise wraps it in them. Whitespace
/// around the block survives either way, so a selection keeps its own newlines.
pub fn toggle_block(text: &str, start: &str, end: &str) -> String {
    let body = text.trim();
    if body.len() >= start.len() + end.len() && body.starts_with(start) && body.ends_with(end) {
        let inner = &body[start.len()..body.len() - end.len()];
        let inner = inner.strip_prefix(' ').unwrap_or(inner);
        let inner = inner.strip_suffix(' ').unwrap_or(inner);
        let lead = &text[..text.len() - text.trim_start().len()];
        let trail = &text[text.trim_end().len()..];
        format!("{lead}{inner}{trail}")
    } else {
        format!("{start} {text} {end}")
    }
}

/// Byte length of a line's indentation.
///
/// ponytail: spaces and tabs only, which is what code indents with. That keeps the offset an
/// ASCII boundary, so slicing a line at it can never split a character; an exotic indent (a
/// no-break space, say) is simply treated as content.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comment_goes_on_at_the_common_indent() {
        let src = "    fn a() {\n\n        b();\n    }\n";
        assert!(!commented(src, "//"));
        assert_eq!(
            comment_lines(src, "//", true),
            "    // fn a() {\n\n    //     b();\n    // }\n"
        );
    }

    #[test]
    fn a_comment_comes_off_when_every_line_is_commented() {
        // The second line has no space after its marker; only one space is ever removed.
        let src = "    // fn a() {\n\n    //b();\n";
        assert!(commented(src, "//"));
        assert_eq!(
            comment_lines(src, "//", false),
            "    fn a() {\n\n    b();\n"
        );
        // A half-commented selection is not commented, so a toggle comments the rest too.
        assert!(!commented("# a\nb\n", "#"));
        assert_eq!(comment_lines("# a\nb\n", "#", true), "# # a\n# b\n");
    }

    /// Told the direction, as a column is once it has decided over all its lines: off leaves a
    /// line without a marker as it is.
    #[test]
    fn a_direction_given_is_kept() {
        assert_eq!(comment_lines("# a\nb", "#", false), "a\nb");
        assert_eq!(comment_lines("# a", "#", true), "# # a");
    }

    #[test]
    fn toggle_block_wraps_and_unwraps() {
        let wrapped = toggle_block("body", "/*", "*/");
        assert_eq!(wrapped, "/* body */");
        assert_eq!(toggle_block(&wrapped, "/*", "*/"), "body");
        assert_eq!(toggle_block("<!--x-->", "<!--", "-->"), "x");
        // Surrounding whitespace is kept, so a line selection keeps its newline.
        assert_eq!(toggle_block("/* x */\n", "/*", "*/"), "x\n");
    }
}
