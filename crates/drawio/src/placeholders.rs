// Derived from draw.io js/grapheditor/Graph.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Placeholders: a cell whose object has `placeholders="1"` shows `%name%` in its label as the
//! value of `name` on it or the nearest object above it, and `%page%`, `%pagenumber%`,
//! `%date%` and their kin as the page and the moment it is shown in. They are filled in only as
//! the page is drawn; the model, the label editor and the index keep what is written.

use std::borrow::Cow;

use crate::model::{Cell, Page, Value, attr};

/// Where and when a page is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// The page's place in its file, from 0.
    pub page: usize,
    /// How many pages the file has.
    pub pages: usize,
    /// The time `%date%`, `%time%`, `%timestamp%` and `%date{mask}%` show; without it they are
    /// left as written.
    pub now: Option<Now>,
}

/// A lone page and no clock.
impl Default for Context {
    fn default() -> Context {
        Context {
            page: 0,
            pages: 1,
            now: None,
        }
    }
}

/// A moment and the local time zone it is read in; the toolkit reads its clock, the crate does
/// the calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Now {
    /// Milliseconds since the Unix epoch.
    pub unix_ms: i64,
    /// The local time zone's offset from UTC, in minutes east of it.
    pub offset_minutes: i32,
}

/// The label `cell` of `page` shows: with its placeholders filled in when its object asks for
/// them, else as written.
// Graph.getLabel, Graph.js 10913-10924; Graph.convertValueToString, Graph.js 12295-12336
pub(crate) fn label<'a>(page: &Page, cell: &'a Cell, ctx: &Context) -> Cow<'a, str> {
    let Value::Object { attrs, .. } = &cell.value else {
        return Cow::Borrowed(cell.label());
    };
    if attr(attrs, "placeholders") != Some("1") {
        return Cow::Borrowed(cell.label());
    }
    // `placeholder="name"` shows that attribute alone, found as a `%name%` would be.
    if let Some(name) = attr(attrs, "placeholder") {
        return Cow::Owned(inherited(page, cell, name).unwrap_or_default().to_string());
    }
    Cow::Owned(replace(page, cell, cell.label(), ctx))
}

/// `text` with each `%name%` in it replaced, those that name nothing left as written; `%%name%`
/// keeps `%name%` (`Graph.replacePlaceholders`, Graph.js 11243-11385).
// ponytail: `%width_mm%` and the like (a size in other units), `%length%` (an edge's), the
// `vars` URL parameter and a translated diagram's `name_<lang>` attributes are not read.
fn replace(page: &Page, cell: &Cell, text: &str, ctx: &Context) -> String {
    let mut out = String::new();
    let mut last = 0;
    let mut i = 0;
    while let Some(at) = text[i..].find('%').map(|at| i + at) {
        let Some(end) = placeholder_end(text, at) else {
            i = at + 1;
            continue;
        };
        let whole = &text[at..end];
        i = end;
        if whole == "%label%" || whole == "%tooltip%" {
            continue;
        }
        // A `%` just before it, not the end of the one before, escapes it.
        let escaped = at > last && text.as_bytes()[at - 1] == b'%';
        out.push_str(&text[last..at]);
        last = end;
        if escaped {
            out.push_str(&whole[1..]);
            continue;
        }
        let name = &whole[1..whole.len() - 1];
        match value(page, cell, name, ctx) {
            Some(v) => out.push_str(&v),
            None => out.push_str(whole),
        }
    }
    out.push_str(&text[last..]);
    out
}

/// Where the placeholder starting at the `%` at `at` ends, one past its closing `%`, if one
/// starts there (`Graph.placeholderPattern`: `%(date\{.*\}|[^%\{\}"'=;]+)%`).
fn placeholder_end(text: &str, at: usize) -> Option<usize> {
    let rest = &text[at + 1..];
    // `date{…}` reaches the last `}%` on its line, the regex's `.*` being greedy.
    if rest.starts_with("date{") {
        let line = &rest[..rest.find('\n').unwrap_or(rest.len())];
        if let Some(close) = line.rfind("}%").filter(|&c| c >= "date{".len()) {
            return Some(at + 1 + close + 2);
        }
    }
    let close = rest.find('%')?;
    let name = &rest[..close];
    let plain = !name.is_empty() && !name.contains(['{', '}', '"', '\'', '=', ';']);
    plain.then_some(at + 1 + close + 1)
}

/// What `%name%` stands for in `cell`'s label.
fn value(page: &Page, cell: &Cell, name: &str, ctx: &Context) -> Option<String> {
    if name == "id" {
        return Some(cell.id.clone());
    }
    // A vertex's size, never an attribute of that name.
    let size = |key: &str| {
        cell.geometry
            .as_ref()
            .filter(|_| name == key)
            .map(|g| number(if key == "width" { g.width } else { g.height }))
    };
    if cell.vertex && name.starts_with("width") {
        return size("width");
    }
    if cell.vertex && name.starts_with("height") {
        return size("height");
    }
    if name.starts_with("length") {
        return None;
    }
    if !name.contains('{')
        && let Some(v) = inherited(page, cell, name)
    {
        return Some(v.to_string());
    }
    for counter in ["pagecount", "pagenumber"] {
        if let Some(rest) = name.strip_prefix(counter).filter(|r| !r.is_empty()) {
            let n = global(counter, page, ctx)?.parse::<i64>().ok()?;
            return offset(rest).map(|d| (n + d).to_string());
        }
    }
    global(name, page, ctx)
}

/// The signed count after `pagenumber` in `%pagenumber + 1%` (`[\s]*([+-])[\s]*(\d+)`).
fn offset(rest: &str) -> Option<i64> {
    let rest = rest.trim_start();
    let (sign, digits) = match rest.as_bytes().first()? {
        b'+' => (1, &rest[1..]),
        b'-' => (-1, &rest[1..]),
        _ => return None,
    };
    let digits = digits.trim_start();
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    Some(sign * digits[..end].parse::<i64>().ok()?)
}

/// The value of attribute `name` on `cell`'s object or the nearest object above it; an
/// attribute present but empty is `""`.
fn inherited<'a>(page: &'a Page, cell: &'a Cell, name: &str) -> Option<&'a str> {
    let mut current = Some(cell);
    while let Some(c) = current {
        if let Value::Object { attrs, .. } = &c.value
            && let Some(v) = attr(attrs, name)
        {
            return Some(v);
        }
        current = c.parent.as_deref().and_then(|p| page.cell(p));
    }
    None
}

/// A name draw.io fills in for every cell: the page and the moment it is shown in
/// (`Graph.getGlobalVariable`, Graph.js 10987-11010, and the editor's own, EditorUi.js
/// 16218-16250).
// ponytail: `%filename%` is not filled in; the crate is not told the file's name.
fn global(name: &str, page: &Page, ctx: &Context) -> Option<String> {
    match name {
        "page" => return Some(page.name().to_string()),
        "pagenumber" => return Some((ctx.page + 1).to_string()),
        "pagecount" => return Some(ctx.pages.to_string()),
        _ => {}
    }
    let now = ctx.now?;
    // ponytail: draw.io writes these in the browser's locale; accent writes them as US English
    // does, which is what draw.io shows in an English browser.
    let mask = match name {
        "date" => "m/d/yyyy",
        "time" => "h:MM:ss TT",
        "timestamp" => "m/d/yyyy, h:MM:ss TT",
        _ => name.strip_prefix("date{")?.strip_suffix('}')?,
    };
    Some(format_date(now, mask))
}

/// A whole number without a fraction, as JavaScript prints one.
fn number(v: f64) -> String {
    match v.fract() == 0.0 && v.abs() < 1e15 {
        true => format!("{}", v as i64),
        false => v.to_string(),
    }
}

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// `now` written by `mask` (`Graph.formatDate`, Graph.js 11015-11120, Steven Levithan's
/// `dateFormat`): `d`/`dd`/`ddd`/`dddd` the day, `m`… the month, `yy`/`yyyy`, `h`/`hh` and
/// `H`/`HH` the hour, `MM` minutes, `ss` seconds, `l`/`L` milliseconds, `t`/`tt`/`T`/`TT` am/pm,
/// `Z` the zone, `o` its offset, `S` the day's ordinal suffix, quoted text as it is; a named mask
/// (`isoDate`, `longTime`, …) stands for its pattern and a `UTC:` prefix reads the time in UTC.
pub(crate) fn format_date(now: Now, mask: &str) -> String {
    let mask = match mask {
        "" | "default" => "ddd mmm dd yyyy HH:MM:ss",
        "shortDate" => "m/d/yy",
        "mediumDate" => "mmm d, yyyy",
        "longDate" => "mmmm d, yyyy",
        "fullDate" => "dddd, mmmm d, yyyy",
        "shortTime" => "h:MM TT",
        "mediumTime" => "h:MM:ss TT",
        "longTime" => "h:MM:ss TT Z",
        "isoDate" => "yyyy-mm-dd",
        "isoTime" => "HH:MM:ss",
        "isoDateTime" => "yyyy-mm-dd'T'HH:MM:ss",
        "isoUtcDateTime" => "UTC:yyyy-mm-dd'T'HH:MM:ss'Z'",
        other => other,
    };
    let (mask, utc) = match mask.strip_prefix("UTC:") {
        Some(rest) => (rest, true),
        None => (mask, false),
    };
    let offset = if utc { 0 } else { now.offset_minutes };
    let t = Civil::of(now.unix_ms + i64::from(offset) * 60_000);
    let pad = |v: i64, n: usize| format!("{v:0n$}");
    let h12 = if t.hour % 12 == 0 { 12 } else { t.hour % 12 };
    // `L`: the milliseconds in hundredths once they take three digits.
    let centis = match t.milli > 99 {
        true => (t.milli as f64 / 10.0).round() as i64,
        false => t.milli,
    };
    let flag = |token: &str| -> Option<String> {
        Some(match token {
            "d" => t.day.to_string(),
            "dd" => pad(t.day, 2),
            "ddd" => DAYS[t.weekday as usize][..3].to_string(),
            "dddd" => DAYS[t.weekday as usize].to_string(),
            "m" => t.month.to_string(),
            "mm" => pad(t.month, 2),
            "mmm" => MONTHS[t.month as usize - 1][..3].to_string(),
            "mmmm" => MONTHS[t.month as usize - 1].to_string(),
            "yy" => {
                let y = t.year.to_string();
                y[y.len().min(2)..].to_string()
            }
            "yyyy" => t.year.to_string(),
            "h" => h12.to_string(),
            "hh" => pad(h12, 2),
            "H" => t.hour.to_string(),
            "HH" => pad(t.hour, 2),
            "M" => t.minute.to_string(),
            "MM" => pad(t.minute, 2),
            "s" => t.second.to_string(),
            "ss" => pad(t.second, 2),
            "l" => pad(t.milli, 3),
            "L" => pad(centis, 2),
            "t" => (if t.hour < 12 { "a" } else { "p" }).to_string(),
            "tt" => (if t.hour < 12 { "am" } else { "pm" }).to_string(),
            "T" => (if t.hour < 12 { "A" } else { "P" }).to_string(),
            "TT" => (if t.hour < 12 { "AM" } else { "PM" }).to_string(),
            // ponytail: the JS takes the zone's name from the browser's date string, "GMT+0200"
            // for most zones but "PDT" for the North American ones; this is always the former.
            "Z" if utc => "UTC".to_string(),
            "Z" => format!("GMT{}", signed_offset(offset)),
            "o" => signed_offset(offset),
            "S" => {
                let d = t.day;
                let i = if d % 10 > 3 || d % 100 - d % 10 == 10 {
                    0
                } else {
                    d % 10
                };
                ["th", "st", "nd", "rd"][i as usize].to_string()
            }
            _ => return None,
        })
    };
    let mut out = String::new();
    let mut rest = mask;
    while let Some(c) = rest.chars().next() {
        let token = date_token(rest, c);
        match flag(token) {
            Some(v) => out.push_str(&v),
            // A quoted run loses its quotes; anything else is itself.
            None if token.len() > 1 => out.push_str(&token[1..token.len() - 1]),
            None => out.push_str(token),
        }
        rest = &rest[token.len()..];
    }
    out
}

/// The token `rest` starts with, first character `c`: `formatDate`'s
/// `d{1,4}|m{1,4}|yy(?:yy)?|([HhMsTt])\1?|[LloSZ]|"[^"]*"|'[^']*'`, else `c` alone.
fn date_token(rest: &str, c: char) -> &str {
    let run = |max: usize| rest.chars().take(max).take_while(|&x| x == c).count();
    let len = match c {
        'd' | 'm' => run(4),
        'y' if rest.starts_with("yyyy") => 4,
        'y' if rest.starts_with("yy") => 2,
        'H' | 'h' | 'M' | 's' | 'T' | 't' => run(2),
        '"' | '\'' => rest[1..].find(c).map_or(1, |end| end + 2),
        _ => c.len_utf8(),
    };
    &rest[..len]
}

/// `+hhmm` or `-hhmm` for an offset of `minutes` east of UTC.
fn signed_offset(minutes: i32) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let m = minutes.abs();
    format!("{sign}{:02}{:02}", m / 60, m % 60)
}

/// A wall-clock time broken into its calendar fields.
struct Civil {
    year: i64,
    /// 1 to 12.
    month: i64,
    day: i64,
    /// 0 for Sunday.
    weekday: i64,
    hour: i64,
    minute: i64,
    second: i64,
    milli: i64,
}

impl Civil {
    /// The proleptic Gregorian date and time `ms` milliseconds after 1970-01-01 00:00 (Howard
    /// Hinnant's `civil_from_days`).
    fn of(ms: i64) -> Civil {
        let days = ms.div_euclid(86_400_000);
        let in_day = ms.rem_euclid(86_400_000);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        Civil {
            year: yoe + era * 400 + i64::from(month <= 2),
            month,
            day: doy - (153 * mp + 2) / 5 + 1,
            // 1970-01-01 was a Thursday.
            weekday: (days + 4).rem_euclid(7),
            hour: in_day / 3_600_000,
            minute: in_day / 60_000 % 60,
            second: in_day / 1000 % 60,
            milli: in_day % 1000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;

    /// 2026-09-30 13:04:05.007 in a zone two hours east of UTC.
    const NOW: Now = Now {
        unix_ms: 1_790_766_245_007,
        offset_minutes: 120,
    };

    #[test]
    fn dates_are_written_by_draw_ios_masks() {
        let f = |mask: &str| format_date(NOW, mask);
        assert_eq!(f(""), "Wed Sep 30 2026 13:04:05");
        assert_eq!(f("isoDate"), "2026-09-30");
        assert_eq!(f("h:MM TT"), "1:04 PM");
        assert_eq!(f("dddd, dS mmmm yy"), "Wednesday, 30th September 26");
        assert_eq!(
            f("yyyy-mm-dd'T'HH:MM:ss.l o Z"),
            "2026-09-30T13:04:05.007 +0200 GMT+0200"
        );
        assert_eq!(f("UTC:HH:MM Z"), "11:04 UTC");
        assert_eq!(f("\"at\" y"), "at y", "a quoted run and a lone y are text");
        let leap = Now {
            unix_ms: 1_709_164_800_000,
            offset_minutes: 0,
        };
        assert_eq!(format_date(leap, "ddd d mmm yyyy"), "Thu 29 Feb 2024");
        let before = Now {
            unix_ms: -1000,
            offset_minutes: 0,
        };
        assert_eq!(format_date(before, "isoDateTime"), "1969-12-31T23:59:59");
    }

    fn object(id: &str, parent: &str, label: &str, attrs: &[(&str, &str)]) -> Cell {
        let mut cell = Cell::new_vertex(id, parent, Rect::new(0.0, 0.0, 120.0, 40.0), "", "");
        let mut all = vec![("label".to_string(), label.to_string())];
        all.extend(attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        cell.value = Value::Object {
            tag: "object".into(),
            attrs: all,
        };
        cell
    }

    #[test]
    fn placeholders_fill_in_from_the_objects_above_and_the_page() {
        let mut page = Page::blank("Title", "p");
        page.cells.push(object(
            "g",
            "1",
            "",
            &[("author", "Ada"), ("placeholders", "1")],
        ));
        let cell = object(
            "c",
            "g",
            "%author% · %id% · %missing% · %%author% · %page% %pagenumber%/%pagecount% · %pagenumber + 1% · %width% · %date%",
            &[("placeholders", "1")],
        );
        page.cells.push(cell);
        let ctx = Context {
            page: 2,
            pages: 5,
            now: None,
        };
        let shown = label(&page, &page.cells[3], &ctx);
        assert_eq!(
            shown,
            "Ada · c · %missing% · %author% · Title 3/5 · 4 · 120 · %date%"
        );
        let dated = Context {
            now: Some(NOW),
            ..ctx
        };
        let cell = object(
            "d",
            "1",
            "%date% %time% %date{yyyy}%",
            &[("placeholders", "1")],
        );
        assert_eq!(label(&page, &cell, &dated), "9/30/2026 1:04:05 PM 2026");
    }

    #[test]
    fn only_a_cell_that_asks_has_its_placeholders_filled_in() {
        let page = Page::blank("P", "p");
        let plain = object("a", "1", "%id%", &[]);
        assert_eq!(label(&page, &plain, &Context::default()), "%id%");
        let named = object(
            "b",
            "1",
            "ignored",
            &[
                ("placeholders", "1"),
                ("placeholder", "who"),
                ("who", "Bob"),
            ],
        );
        assert_eq!(label(&page, &named, &Context::default()), "Bob");
    }
}
