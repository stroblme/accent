//! The `Ctrl+/` comment toggle, as plain string work.
//!
//! The markers come from the GtkSourceView language's metadata, so this module never needs to
//! know which language it is looking at — and never needs to touch a buffer.

/// Toggle a line comment over `text`, whose lines are the ones the user selected.
///
/// Removes the marker when every non-blank line already carries one, otherwise inserts it at the
/// shallowest indentation in the selection so the block stays aligned. Blank lines are left
/// alone, and the trailing newline structure of the input is preserved.
pub fn toggle_lines(text: &str, marker: &str) -> String {
    // `split('\n')` and `join("\n")` round-trip exactly, trailing newline included.
    let lines: Vec<&str> = text.split('\n').collect();
    let content: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();
    if content.is_empty() {
        return text.to_string();
    }
    let out: Vec<String> = if content.iter().all(|l| l.trim_start().starts_with(marker)) {
        lines
            .iter()
            .map(|line| {
                let (lead, rest) = line.split_at(indent(line));
                match rest.strip_prefix(marker) {
                    Some(rest) => format!("{lead}{}", rest.strip_prefix(' ').unwrap_or(rest)),
                    None => line.to_string(),
                }
            })
            .collect()
    } else {
        let common = content.iter().copied().map(indent).min().unwrap_or(0);
        lines
            .iter()
            .map(|line| {
                if line.trim().is_empty() {
                    line.to_string()
                } else {
                    format!("{}{marker} {}", &line[..common], &line[common..])
                }
            })
            .collect()
    };
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
    fn toggle_adds_at_the_common_indent() {
        let src = "    fn a() {\n\n        b();\n    }\n";
        assert_eq!(
            toggle_lines(src, "//"),
            "    // fn a() {\n\n    //     b();\n    // }\n"
        );
    }

    #[test]
    fn toggle_removes_when_every_line_is_commented() {
        // The second line has no space after its marker; only one space is ever removed.
        let src = "    // fn a() {\n\n    //b();\n";
        assert_eq!(toggle_lines(src, "//"), "    fn a() {\n\n    b();\n");
        // A half-commented selection comments the rest instead of uncommenting.
        assert_eq!(toggle_lines("# a\nb\n", "#"), "# # a\n# b\n");
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
