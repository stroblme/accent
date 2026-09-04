//! Template expansion for new notes and daily notes.

use chrono::NaiveDateTime;
use chrono::format::StrftimeItems;

/// Expand a template. Returns the text and, when the template had a `{{cursor}}`, the byte offset
/// where the caret belongs.
///
/// A placeholder accent does not know, or one with a format string chrono rejects, is copied out
/// verbatim: a typo in a template should be visible in the new note, not destroy its text.
pub fn render(text: &str, title: &str, now: NaiveDateTime) -> (String, Option<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut cursor = None;
    let mut rest = text;

    while let Some(open) = rest.find("{{") {
        let Some(close) = rest[open + 2..].find("}}") else {
            break;
        };
        let name = &rest[open + 2..open + 2 + close];
        out.push_str(&rest[..open]);
        match expand(name, title, now) {
            Some(value) => out.push_str(&value),
            // The marker itself never reaches the note; only the first one places the caret.
            None if name == "cursor" => {
                cursor.get_or_insert(out.len());
            }
            None => out.push_str(&rest[open..open + 2 + close + 2]),
        }
        rest = &rest[open + 2 + close + 2..];
    }
    out.push_str(rest);
    (out, cursor)
}

fn expand(name: &str, title: &str, now: NaiveDateTime) -> Option<String> {
    match name {
        "date" => strftime("%Y-%m-%d", now),
        "time" => strftime("%H:%M", now),
        "title" => Some(title.to_string()),
        _ => match name.split_once(':') {
            Some(("date" | "time", fmt)) => strftime(fmt, now),
            _ => None,
        },
    }
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
        assert_eq!(cursor, None);
    }

    #[test]
    fn render_supports_custom_strftime() {
        let (out, _) = render("{{date:%A, %d %B %Y}} / {{time:%H%M}}", "x", now());
        assert_eq!(out, "Thursday, 03 September 2026 / 1405");
    }

    #[test]
    fn render_reports_cursor_offset_after_substitution() {
        let (out, cursor) = render("{{date}} log\n- {{cursor}}done", "x", now());
        assert_eq!(out, "2026-09-03 log\n- done");
        // Not 15: `{{date}}` is two bytes shorter than the date it expands to.
        assert_eq!(cursor, Some(17));
        assert_eq!(&out[17..], "done");
    }

    #[test]
    fn render_leaves_unknown_and_invalid_placeholders_verbatim() {
        let (out, cursor) = render("{{nope}} {{date:%Q}} {{title}} {{", "Note", now());
        assert_eq!(out, "{{nope}} {{date:%Q}} Note {{");
        assert_eq!(cursor, None);
    }

    #[test]
    fn strftime_rejects_a_bad_format() {
        assert_eq!(strftime("%Q", now()), None);
        assert_eq!(strftime("%Y", now()).as_deref(), Some("2026"));
    }
}
