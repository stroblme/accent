//! Template expansion for new notes.

use chrono::NaiveDateTime;
use chrono::format::StrftimeItems;

/// Expand a template. Returns the text and the byte offset of every `{{cursor}}`, in order: the
/// first is where the caret belongs, and the rest are the stops Tab walks after it.
///
/// A placeholder accent does not know, or one with a format string chrono rejects, is copied out
/// verbatim: a typo in a template should be visible in the new note, not destroy its text.
pub fn render(text: &str, title: &str, now: NaiveDateTime) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut cursors = Vec::new();
    let mut rest = text;

    while let Some(open) = rest.find("{{") {
        let Some(close) = rest[open + 2..].find("}}") else {
            break;
        };
        let name = &rest[open + 2..open + 2 + close];
        out.push_str(&rest[..open]);
        match expand(name, title, now) {
            Some(value) => out.push_str(&value),
            // The marker itself never reaches the note.
            None if name == "cursor" => cursors.push(out.len()),
            None => out.push_str(&rest[open..open + 2 + close + 2]),
        }
        rest = &rest[open + 2 + close + 2..];
    }
    out.push_str(rest);
    (out, cursors)
}

/// `title`, `time`, `date`, each with an optional `:format`; and `date` with a signed day offset
/// before the colon, so `{{date-1}}` is yesterday and `{{date+3:%A}}` the weekday in three days.
fn expand(name: &str, title: &str, now: NaiveDateTime) -> Option<String> {
    let (key, fmt) = name
        .split_once(':')
        .map_or((name, None), |(k, f)| (k, Some(f)));
    match key {
        "title" => return Some(title.to_string()),
        "time" => return strftime(fmt.unwrap_or("%H:%M"), now),
        _ => {}
    }
    let (key, days) = match key.find(['+', '-']) {
        Some(at) => (&key[..at], key[at..].parse::<i64>().ok()?),
        None => (key, 0),
    };
    (key == "date").then(|| {
        strftime(
            fmt.unwrap_or("%Y-%m-%d"),
            now + chrono::Duration::days(days),
        )
    })?
}

/// The front-matter key a template says its destination with.
const TARGET: &str = "accent-target";

/// A template split into the directive accent reads and the text a note made from it gets.
#[derive(Debug, PartialEq)]
pub struct Template {
    /// The `accent-target:` value as written, still to be rendered; `None` when the template
    /// says nothing about where its notes go.
    pub target: Option<String>,
    pub body: String,
}

/// Lift `accent-target:` out of a template's leading `---` block.
///
/// The directive is accent's, not the note's, so it never reaches the note: the line goes, and the
/// whole block goes with it when the line was all it held — together with the blank line that
/// separated the block from the text, so the note starts where its first heading does. Everything
/// else is copied byte for byte, including a second `accent-target:` and any `---` further down.
pub fn parse(text: &str) -> Template {
    let fence = |line: &str| line.trim_end() == "---";
    let mut lines = text.split_inclusive('\n');
    let Some(open) = lines.next().filter(|l| fence(l)) else {
        return verbatim(text);
    };
    let mut kept = String::new();
    let mut target = None;
    let mut read = open.len();
    for line in lines {
        read += line.len();
        if fence(line) {
            let rest = &text[read..];
            let body = match target.is_some() && kept.is_empty() {
                true => rest.strip_prefix('\n').unwrap_or(rest).to_string(),
                false => format!("{open}{kept}{line}{rest}"),
            };
            return Template { target, body };
        }
        match line.split_once(':') {
            Some((key, value)) if key.trim() == TARGET && target.is_none() => {
                target = Some(value.trim().to_string());
            }
            _ => kept.push_str(line),
        }
    }
    // An unclosed `---` is not front matter, so nothing in it was a directive.
    verbatim(text)
}

fn verbatim(text: &str) -> Template {
    Template {
        target: None,
        body: text.to_string(),
    }
}

/// The vault-relative paths a configured template name may mean, best first.
///
/// A bare `DailyNote.md` is what the preferences invite, and it means the file of that name in the
/// templates directory. The name is still tried verbatim first, so a setting that resolves against
/// the vault root today keeps resolving there.
pub fn candidates(templates_dir: &str, template: &str) -> Vec<String> {
    let mut out = vec![template.to_string()];
    if !templates_dir.is_empty() && !template.contains('/') {
        out.push(format!("{templates_dir}/{template}"));
    }
    out
}

/// `strftime`-style formatting, or `None` when the format string is invalid.
pub fn strftime(fmt: &str, now: NaiveDateTime) -> Option<String> {
    use std::fmt::Write;
    // Parsing first rejects unknown specifiers; writing through `write!` catches the ones that
    // parse but need a time zone (`%Z`), which a naive time cannot supply.
    let items = StrftimeItems::new(fmt).parse().ok()?;
    let mut out = String::new();
    write!(out, "{}", now.format_with_items(items.iter())).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    /// A Thursday afternoon, so `%A` and `%H:%M` have something to prove.
    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 3)
            .unwrap()
            .and_hms_opt(14, 5, 0)
            .unwrap()
    }

    #[test]
    fn render_expands_date_time_and_title() {
        let (out, cursor) = render(
            "# {{title}}\n\ndate: {{date}} time: {{time}}\n",
            "Weekly sync",
            now(),
        );
        assert_eq!(out, "# Weekly sync\n\ndate: 2026-09-03 time: 14:05\n");
        assert!(cursor.is_empty());
    }

    #[test]
    fn render_offsets_a_date_by_days() {
        let (out, _) = render("{{date-1}} {{date+3:%A}} {{date+x}} {{date1}}", "x", now());
        assert_eq!(out, "2026-09-02 Sunday {{date+x}} {{date1}}");
    }

    #[test]
    fn render_supports_custom_strftime() {
        let (out, _) = render("{{date:%A, %d %B %Y}} / {{time:%H%M}}", "x", now());
        assert_eq!(out, "Thursday, 03 September 2026 / 1405");
    }

    #[test]
    fn render_reports_every_cursor_offset_after_substitution() {
        let (out, cursors) = render("{{date}} log\n- {{cursor}}done {{cursor}}", "x", now());
        assert_eq!(out, "2026-09-03 log\n- done ");
        // Not 15: `{{date}}` is two bytes shorter than the date it expands to.
        assert_eq!(cursors, [17, 22]);
        assert_eq!(&out[17..], "done ");
    }

    #[test]
    fn render_leaves_unknown_and_invalid_placeholders_verbatim() {
        let (out, cursor) = render("{{nope}} {{date:%Q}} {{title}} {{", "Note", now());
        assert_eq!(out, "{{nope}} {{date:%Q}} Note {{");
        assert!(cursor.is_empty());
    }

    #[test]
    fn parse_lifts_the_target_out_of_the_front_matter() {
        let t = parse("---\ntags: [daily]\naccent-target: Daily/{{date}}.md\n---\n\n# x\n");
        assert_eq!(t.target.as_deref(), Some("Daily/{{date}}.md"));
        // The rest of the block is the note's own front matter and stays exactly as written.
        assert_eq!(t.body, "---\ntags: [daily]\n---\n\n# x\n");
    }

    #[test]
    fn parse_drops_a_block_the_target_was_alone_in() {
        let t = parse("---\naccent-target: Log.md\n---\n\n# Log\n");
        assert_eq!(t.target.as_deref(), Some("Log.md"));
        assert_eq!(t.body, "# Log\n");
    }

    #[test]
    fn parse_leaves_a_template_that_says_nothing_alone() {
        for text in ["# {{title}}\n", "---\ntags: [x]\n---\n\nbody\n"] {
            assert_eq!(
                parse(text),
                Template {
                    target: None,
                    body: text.to_string()
                }
            );
        }
    }

    #[test]
    fn parse_only_reads_a_closed_leading_block() {
        // Below the block, and in a block that never closes, the line is text like any other.
        for text in [
            "# x\n\naccent-target: Nope.md\n",
            "---\naccent-target: Nope.md\n\n# x\n",
        ] {
            assert_eq!(parse(text).target, None);
            assert_eq!(parse(text).body, text);
        }
    }

    #[test]
    fn candidates_fall_back_to_the_templates_directory() {
        assert_eq!(
            candidates("Templates", "DailyNote.md"),
            ["DailyNote.md", "Templates/DailyNote.md"]
        );
        // A path says where it means; an unset templates directory has nowhere else to look.
        assert_eq!(candidates("Templates", "Sub/X.md"), ["Sub/X.md"]);
        assert_eq!(candidates("", "X.md"), ["X.md"]);
    }

    #[test]
    fn strftime_rejects_a_bad_format() {
        assert_eq!(strftime("%Q", now()), None);
        assert_eq!(strftime("%Y", now()).as_deref(), Some("2026"));
    }
}
