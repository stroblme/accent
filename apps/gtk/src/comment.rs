//! Language-aware `Ctrl+/` comment toggling over plain strings.
//!
//! Markers come from GtkSourceView language metadata, keeping this logic independent of
//! languages and text buffers.

/// Whether all nonblank lines already carry `marker`, deciding one toggle direction for all carets.
/// Partially commented selections are commented in full.
pub fn commented(text: &str, marker: &str) -> bool {
    text.split('\n')
        .filter(|l| !l.trim().is_empty())
        .all(|l| l.trim_start().starts_with(marker))
}

/// Add (`on`) or remove line comments in selected `text`, preserving blank lines and trailing newlines.
/// Insert at the shallowest indentation to align the block; removal also takes one following space.
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

/// Indentation length in bytes, counting spaces and tabs only.
///
/// ASCII whitespace guarantees a safe slicing boundary; other whitespace is treated as content.
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
