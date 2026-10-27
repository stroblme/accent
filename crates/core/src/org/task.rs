//! Toggle Task in an org file: a checkbox one step along `[ ]` → `[-]` → `[X]` → `[ ]`, a
//! heading's TODO keyword along the file's sequence, and any other line made a `- [ ]` item, as
//! a markdown note's Toggle Task does ([`crate::markdown::toggle_task`]). A statistics cookie,
//! `[1/3]` or `[33%]`, is left as written, and `[/]`, an empty one to org, is never written.

use super::{closed, heading, keywords};
use crate::markdown::toggle_box;
use std::ops::Range;

/// Each of the lines `lines` of `text`, numbered from 0, one step on, or `None` for a line passed
/// over as a markdown note's front matter and code blocks are: one in a block or a drawer, a
/// `#+KEY:` line and a fixed-width `: ` line. A line past the text's last is an empty one.
pub fn toggle_task(text: &str, lines: Range<usize>) -> Vec<Option<String>> {
    let all = super::lines(text);
    let keywords = keywords(text, &all);
    let mut verbatim = vec![false; all.len()];
    let mut i = 0;
    while i < all.len() {
        if let Some((last, _)) = closed(text, &all, i) {
            verbatim[i..=last].fill(true);
            i = last + 1;
            continue;
        }
        let line = text[all[i].clone()].trim_start();
        let fixed = line
            .strip_prefix(':')
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']));
        verbatim[i] = fixed || line.starts_with("#+");
        i += 1;
    }
    lines
        .map(|n| match all.get(n) {
            Some(_) if verbatim[n] => None,
            Some(line) => Some(toggle(&text[line.clone()], &keywords)),
            None => Some(toggle("", &keywords)),
        })
        .collect()
}

/// One line one step on: a heading's keyword to the next of its sequence, none after the last,
/// the first of the first sequence where it has none; any other line's box as markdown's goes.
fn toggle(line: &str, keywords: &[Vec<(&str, bool)>]) -> String {
    let Some(head) = heading(line, 0..line.len(), keywords) else {
        return toggle_box(line, &[' ', '\t'], b'-', b'X');
    };
    let Some((at, _)) = head.keyword else {
        let (stars, rest) = line.split_at(head.stars.end);
        let first = keywords[0][0].0;
        return match rest.trim().is_empty() {
            true => format!("{} {first}", stars.trim_end()),
            false => format!("{stars}{first} {rest}"),
        };
    };
    let next = keywords
        .iter()
        .find_map(|sequence| {
            let i = sequence.iter().position(|(k, _)| *k == &line[at.clone()])?;
            Some(sequence.get(i + 1).map(|(k, _)| *k))
        })
        .flatten();
    match next {
        Some(next) => format!("{}{next}{}", &line[..at.start], &line[at.end..]),
        None => format!("{}{}", &line[..at.start], line[at.end..].trim_start()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str, line: usize) -> String {
        toggle_task(text, line..line + 1).remove(0).unwrap()
    }

    #[test]
    fn a_checkbox_cycles_and_anything_else_becomes_one() {
        for (line, toggled) in [
            ("- [ ] open", "- [-] open"),
            ("- [-] doing", "- [X] doing"),
            ("- [X] done", "- [ ] done"),
            ("- [x] done", "- [ ] done"),
            ("  + item", "  + [ ] item"),
            ("1) [ ] item", "1) [-] item"),
            ("- [1/3] parent", "- [ ] [1/3] parent"),
            ("> plain", "- [ ] > plain"),
            ("", "- [ ] "),
        ] {
            assert_eq!(one(line, 0), toggled, "{line:?}");
        }
    }

    /// Priority, `COMMENT`, a statistics cookie and the tags stay where they are written.
    #[test]
    fn a_heading_steps_through_the_files_keywords() {
        let todo = "#+TODO: NEXT WAIT | DONE CANCELLED\n#+SEQ_TODO: IDEA\n";
        let mut heading = "** [#A] COMMENT Plan [1/3] :work:".to_string();
        let mut seen = Vec::new();
        for _ in 0..5 {
            heading = one(&format!("{todo}{heading}\n"), 2);
            seen.push(heading.clone());
        }
        assert_eq!(
            seen,
            [
                "** NEXT [#A] COMMENT Plan [1/3] :work:",
                "** WAIT [#A] COMMENT Plan [1/3] :work:",
                "** DONE [#A] COMMENT Plan [1/3] :work:",
                "** CANCELLED [#A] COMMENT Plan [1/3] :work:",
                "** [#A] COMMENT Plan [1/3] :work:",
            ]
        );
        // The last keyword of its own sequence goes, rather than on to another's.
        assert_eq!(one(&format!("{todo}* IDEA Maybe\n"), 2), "* Maybe");
        for (line, toggled) in [("* Plan", "* TODO Plan"), ("* TODO Plan", "* DONE Plan")] {
            assert_eq!(one(line, 0), toggled);
        }
        assert_eq!(one("* DONE", 0), "* ");
        assert_eq!(one("* ", 0), "* TODO");
    }

    #[test]
    fn blocks_drawers_keywords_and_fixed_width_lines_are_passed_over() {
        let text =
            "#+TITLE: x\n:PROPERTIES:\n:ID: 1\n:END:\n#+begin_src sh\n- a\n#+end_src\n: fixed\nb\n";
        let toggled = toggle_task(text, 0..10);
        assert_eq!(toggled[..8], [const { None }; 8]);
        assert_eq!(
            toggled[8..],
            [Some("- [ ] b".into()), Some("- [ ] ".into())]
        );
    }
}
