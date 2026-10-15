//! Full-text and regex search over the indexed bodies, and the pure text functions that cut
//! and mark a snippet the way the FTS tokenizer folds it.

use super::{Index, Match, SearchHit};
use crate::Result;
use crate::search::Regex;
use crate::walk::FileKind;
use rusqlite::{OptionalExtension, params};
use std::ops::Range;

/// Bytes of a matched line [`Index::grep`] keeps before and after the match. A note can hold a
/// single line megabytes long (an embedded data URI), and a sidebar row must not carry all of it.
const CLIP_BEFORE: usize = 40;

const CLIP_AFTER: usize = 200;

/// Shortest mid-word query supported by the trigram index. Shorter queries would require
/// reading every body.
pub const MIN_INFIX: usize = 3;

/// Maximum grep rows per file, preventing one file from filling the result list.
pub(super) const PER_FILE: usize = 5;

impl Index {
    /// Search indexed titles and bodies as an ordered phrase ([`fts_query`]), returning byte
    /// ranges. Directories, PDFs, conflict copies and binaries have no searchable body.
    ///
    /// Exclude ignored non-note files unless `include_ignored`; filter before applying `limit`.
    ///
    /// Exact titles lead, then SQLite `bm25` ranks matches with title weight 10 and body weight 1.
    ///
    /// Rank before reading bodies and build snippets locally to avoid FTS5 `snippet()`'s costly
    /// prefix-position merging.
    ///
    /// If no prefix result exists, [`infix`](Self::infix) handles mid-word matches. Otherwise
    /// [`search_mid_word`](Self::search_mid_word) adds them later.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        include_ignored: bool,
    ) -> Result<Vec<SearchHit>> {
        let phrase = fold(&terms(query).join(" "));
        let hits = self.ranked(query, &phrase, limit, include_ignored)?;
        let hits = match hits.is_empty() {
            true => self.infix(query, limit, include_ignored)?,
            false => hits,
        };
        self.per_match(hits, &phrase, limit)
    }

    /// The files holding any of `words`, each matched as the start of a word, best first by
    /// `bm25` with the title weighted as [`search`](Self::search) weights it, at most `limit`:
    /// one row per file, `(rel_path, title)`, for a question asked in words rather than as a
    /// phrase. What git ignores is left out, a note never.
    pub fn rank_files(
        &self,
        words: &[String],
        limit: usize,
    ) -> Result<Vec<(String, Option<String>)>> {
        let q: Vec<String> = words
            .iter()
            .filter(|w| !w.trim().is_empty())
            .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
            .collect();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare_cached(
            "SELECT f.rel_path, f.title FROM notes_fts JOIN files f ON f.id = notes_fts.rowid
             WHERE notes_fts MATCH ?1 AND (f.git_ignored = 0 OR f.kind = ?2)
             ORDER BY bm25(notes_fts, 1.0, 10.0) LIMIT ?3",
        )?;
        let rows = st.query_map(
            params![q.join(" OR "), FileKind::Markdown.as_i64(), limit as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// What the trigram index adds below a [`search`](Self::search) that found something: the
    /// files holding the query mid-word, in [`infix`](Self::infix)'s order, leaving out the
    /// `skip` files already listed, at most `limit` rows. `toggle` finds `retoggle.md` here.
    ///
    /// Nothing where the prefix index found nothing, because `search` has already answered that
    /// query from the trigram index and this would list the same files again.
    pub fn search_mid_word(
        &self,
        query: &str,
        limit: usize,
        include_ignored: bool,
        skip: &[String],
    ) -> Result<Vec<SearchHit>> {
        let phrase = fold(&terms(query).join(" "));
        if self.ranked(query, &phrase, 1, include_ignored)?.is_empty() {
            return Ok(Vec::new());
        }
        let hits = self.infix(query, limit + skip.len(), include_ignored)?;
        let hits = hits.into_iter().filter(|h| !skip.contains(&h.rel_path));
        self.per_match(hits.take(limit).collect(), &phrase, limit)
    }

    /// The files the prefix index ranks for `query`, best first, at most `limit` of them: one hit
    /// each, before [`per_match`](Self::per_match) makes rows of them.
    fn ranked(
        &self,
        query: &str,
        phrase: &str,
        limit: usize,
        include_ignored: bool,
    ) -> Result<Vec<SearchHit>> {
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare_cached(
            "SELECT f.rel_path, f.title, snippet_window(notes_fts.body, ?4),
                    phrase_start(notes_fts.body, ?4)
             FROM notes_fts JOIN files f ON f.id = notes_fts.rowid
             WHERE notes_fts MATCH ?1 AND notes_fts.rowid IN (
                 SELECT notes_fts.rowid FROM notes_fts JOIN files g ON g.id = notes_fts.rowid
                  WHERE notes_fts MATCH ?1 AND (?5 OR g.git_ignored = 0 OR g.kind = ?6)
                  ORDER BY lower(ifnull(g.title, '')) = lower(?2) DESC, bm25(notes_fts, 1.0, 10.0)
                  LIMIT ?3)
             ORDER BY lower(ifnull(f.title, '')) = lower(?2) DESC, bm25(notes_fts, 1.0, 10.0)",
        )?;
        // What the query matched is one phrase, so that is what the window is cut around and what
        // the snippet marks. It goes in folded, and with its whitespace squeezed, because that is
        // how `snippet_window` reads the body it looks through.
        let rows = st.query_map(
            params![
                q,
                query.trim(),
                limit as i64,
                phrase,
                include_ignored,
                FileKind::Markdown.as_i64(),
            ],
            |r| hit(r, phrase),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Expand ranked files into occurrence rows, at most [`PER_FILE`] per file and `limit`
    /// overall, counting omitted occurrences on the last row. Title-only hits retain one row
    /// quoting the body start.
    ///
    /// Read bodies only for listed files through keyed lookups, keeping large bodies out of the
    /// ranking sorter.
    fn per_match(
        &self,
        hits: Vec<SearchHit>,
        phrase: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let mut st = self.conn.prepare_cached(
            "SELECT n.body FROM notes n JOIN files f ON f.id = n.file_id WHERE f.rel_path = ?1",
        )?;
        let mut out: Vec<SearchHit> = Vec::with_capacity(hits.len());
        for hit in hits {
            let room = limit.saturating_sub(out.len());
            if room == 0 {
                break;
            }
            let body: Option<String> = st.query_row([&hit.rel_path], |r| r.get(0)).optional()?;
            let rows = body.map_or_else(Vec::new, |body| {
                let cap = room.min(PER_FILE);
                phrase_hits(&hit.rel_path, hit.title.as_deref(), &body, phrase, 0, cap)
            });
            match rows.is_empty() {
                true => out.push(hit),
                false => out.extend(rows),
            }
        }
        Ok(out)
    }

    /// Find literal mid-word substrings with the trigram index, avoiding a scan of every body.
    /// [`search`](Self::search) uses this when prefix search is empty;
    /// [`search_mid_word`](Self::search_mid_word) adds results after typing stops.
    ///
    /// Matches fold case and Latin diacritics, treating `%` and `_` literally. Trigram
    /// candidates are verified with [`folded_find`] through `phrase_start` rather than an
    /// unfolded wildcard `LIKE`.
    ///
    /// Without term statistics (`detail=none`), title matches lead, then shorter files.
    fn infix(&self, query: &str, limit: usize, include_ignored: bool) -> Result<Vec<SearchHit>> {
        let needle = terms(query).join(" ");
        let chars: Vec<char> = needle.chars().collect();
        if chars.len() < MIN_INFIX {
            return Ok(Vec::new());
        }
        // One quoted term per window: `detail='none'` answers no phrase of several.
        let windows: Vec<String> = chars
            .windows(MIN_INFIX)
            .map(|w| format!("\"{}\"", String::from_iter(w).replace('"', "\"\"")))
            .collect();
        let mut st = self.conn.prepare_cached(
            "SELECT f.rel_path, f.title, snippet_window(n.body, ?2), phrase_start(n.body, ?2)
             FROM notes n JOIN files f ON f.id = n.file_id
             WHERE n.file_id IN (
                 SELECT t.rowid FROM notes_tri t JOIN files g ON g.id = t.rowid
                  WHERE notes_tri MATCH ?1
                    AND (phrase_start(t.body, ?2) IS NOT NULL
                         OR phrase_start(t.title, ?2) IS NOT NULL)
                    AND (?4 OR g.git_ignored = 0 OR g.kind = ?5)
                  ORDER BY phrase_start(ifnull(g.title, ''), ?2) IS NOT NULL DESC, g.size
                  LIMIT ?3)
             ORDER BY phrase_start(ifnull(f.title, ''), ?2) IS NOT NULL DESC, f.size",
        )?;
        let phrase = fold(&needle);
        let rows = st.query_map(
            params![
                windows.join(" AND "),
                &phrase,
                limit as i64,
                include_ignored,
                FileKind::Markdown.as_i64(),
            ],
            |r| hit(r, &phrase),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// List up to `limit` regex matches in `rel_path` order and count all matches, including
    /// those omitted. [`grep_paths`](Self::grep_paths) visits the same bodies for Replace All.
    ///
    /// Bodies stream from the index without disk reads or materialising the vault. Ignored-file
    /// handling matches [`search`](Self::search), always retaining markdown notes.
    pub fn grep(
        &self,
        re: &Regex,
        limit: usize,
        include_ignored: bool,
    ) -> Result<(Vec<Match>, usize)> {
        let mut st = self.conn.prepare_cached(GREP_SQL)?;
        let mut rows = st.query(params![include_ignored, FileKind::Markdown.as_i64()])?;
        let (mut out, mut total) = (Vec::new(), 0usize);
        while let Some(row) = rows.next()? {
            let body: String = row.get(2)?;
            // Tested before the other columns are fetched: most notes do not match, and their
            // path and title would be allocated for nothing.
            if !re.is_match(&body) {
                continue;
            }
            let (rel_path, title): (String, Option<String>) = (row.get(0)?, row.get(1)?);
            Self::matches_in(
                &rel_path,
                title.as_deref(),
                &body,
                re,
                limit,
                &mut out,
                &mut total,
            );
        }
        Ok((out, total))
    }

    /// Append matches from one supplied body to `out` and count them in `total`, without reading
    /// disk or the index. This also supports unindexed files.
    ///
    /// `limit` caps the combined output; `total` continues counting. [`PER_FILE`] leaves room
    /// for other files, with omitted matches recorded in [`Match::more`] on the last listed row.
    pub fn matches_in(
        rel: &str,
        title: Option<&str>,
        body: &str,
        re: &Regex,
        limit: usize,
        out: &mut Vec<Match>,
        total: &mut usize,
    ) {
        let cap = limit.saturating_sub(out.len()).min(PER_FILE);
        let (rows, found) = Self::matches_from(rel, title, body, re, 0, cap);
        *total += found;
        out.extend(rows);
    }

    /// The matches of `re` in one body that start at byte `from` or later: at most `cap` rows,
    /// the rest counted into the last one's [`Match::more`], and how many there are in all.
    /// [`matches_in`](Self::matches_in) lists a file's first few through it, and the sidebar the
    /// rest once the reader opens that file's "+N more" row.
    ///
    /// The regex still runs from the top of the body, so `^` and `\b` read the text before `from`
    /// as the first pass did and the matches are the ones it counted.
    pub fn matches_from(
        rel: &str,
        title: Option<&str>,
        body: &str,
        re: &Regex,
        from: usize,
        cap: usize,
    ) -> (Vec<Match>, usize) {
        // `find_iter` walks forward, so the line number follows it instead of being counted
        // from the start of the note for every hit.
        let mut lines = Lines::new(body);
        let (mut out, mut total): (Vec<Match>, usize) = (Vec::new(), 0);
        for m in re.find_iter(body).skip_while(|m| m.start() < from) {
            total += 1;
            if out.len() >= cap {
                if let Some(last) = out.last_mut() {
                    last.more += 1;
                }
                continue;
            }
            let (line, line_text, range) = lines.at(m.start(), m.end());
            out.push(Match {
                rel_path: rel.to_string(),
                title: title.map(str::to_string),
                line,
                line_text,
                range,
                offset: m.start(),
                more: 0,
            });
        }
        (out, total)
    }

    /// [`matches_from`](Self::matches_from) for the rows [`search`](Self::search) lists: the
    /// ranked query's occurrences in one body from byte `from` on, one row each, at most `cap`
    /// and the rest counted onto the last. The phrase is folded as `search` folds it, so the rows
    /// go on where that file's listed ones stopped.
    pub fn phrase_hits_from(
        rel: &str,
        body: &str,
        query: &str,
        from: usize,
        cap: usize,
    ) -> Vec<SearchHit> {
        let phrase = fold(&terms(query).join(" "));
        phrase_hits(rel, None, body, &phrase, from, cap)
    }

    /// The files whose indexed body matches at all, in `rel_path` order: the bodies
    /// [`grep`](Self::grep) reads under the same `include_ignored`, so a Replace All rewrites
    /// exactly what that count promised. Uncapped on purpose: a global replace has to visit every
    /// file, not only the ones the sidebar had room to list.
    pub fn grep_paths(&self, re: &Regex, include_ignored: bool) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached(GREP_SQL)?;
        let mut rows = st.query(params![include_ignored, FileKind::Markdown.as_i64()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let body: String = row.get(2)?;
            if re.is_match(&body) {
                out.push(row.get(0)?);
            }
        }
        Ok(out)
    }
}

/// Every indexed path, title and text, for the sidebar's regex scan. Ordered so a capped list
/// and an uncapped one agree on which matches they drop.
///
/// `?1` drops the git-ignored exclusion, `?2` is [`FileKind::Markdown`] — the escape that keeps a
/// note in the results whatever ignores it.
const GREP_SQL: &str = "SELECT f.rel_path, f.title, n.body
     FROM notes n JOIN files f ON f.id = n.file_id
     WHERE ?1 OR f.git_ignored = 0 OR f.kind = ?2
     ORDER BY f.rel_path";

/// A walk down a body's matches, handing each one the line it starts on. Both searches walk their
/// matches forward, so the line number follows the walk instead of being counted from the top of
/// the note for every one of them.
struct Lines<'a> {
    body: &'a str,
    /// How far the line counter has read. Matches arrive in order, so it never reads twice.
    cursor: usize,
    line: u32,
    line_start: usize,
}

impl<'a> Lines<'a> {
    fn new(body: &'a str) -> Self {
        Self {
            body,
            cursor: 0,
            line: 1,
            line_start: 0,
        }
    }

    /// The 1-based line the match at `start..end` begins on, that line clipped to what a sidebar
    /// row can show, and where the match sits in the clipped text. A match running past the end
    /// of its line — a phrase the note wrote across two — is marked to the end of the line, and
    /// one that starts in the `\r\n` the row trims is an empty mark at its end.
    fn at(&mut self, start: usize, end: usize) -> (u32, String, Range<usize>) {
        while self.cursor < start {
            if self.body.as_bytes()[self.cursor] == b'\n' {
                self.line += 1;
                self.line_start = self.cursor + 1;
            }
            self.cursor += 1;
        }
        let rest = &self.body[self.line_start..];
        let line_text = rest
            .split('\n')
            .next()
            .unwrap_or(rest)
            .trim_end_matches('\r');
        let from = (start - self.line_start).min(line_text.len());
        let to = (end - self.line_start).min(line_text.len());
        let (text, range) = clip(line_text, from..to);
        (self.line, text, range)
    }
}

/// Every occurrence of the folded `phrase` in `body` from byte `from` on as a hit of its own: the
/// line it sits on, clipped the way an exact search clips one, with that occurrence alone marked
/// in it. At most `cap` rows, the occurrences past that counted onto the last row.
///
/// Empty where the body does not hold the phrase — a hit on the title, or one folded across a
/// stretch [`folded_find`] cannot put back together — and the caller keeps its file row then.
fn phrase_hits(
    rel: &str,
    title: Option<&str>,
    body: &str,
    phrase: &str,
    from: usize,
    cap: usize,
) -> Vec<SearchHit> {
    let mut out: Vec<SearchHit> = Vec::new();
    let mut lines = Lines::new(body);
    // A place read off an earlier copy of the file may be past its end or inside a character.
    let mut from = from.min(body.len());
    while !body.is_char_boundary(from) {
        from += 1;
    }
    // A zero-length match would never advance, so an empty query lists nothing rather than looping.
    while let Some((at, len)) = folded_find(&body[from..], phrase).filter(|&(_, len)| len > 0) {
        let start = from + at;
        from = start + len;
        if out.len() >= cap {
            if let Some(last) = out.last_mut() {
                last.more += 1;
            }
            continue;
        }
        let (line, text, range) = lines.at(start, start + len);
        out.push(SearchHit {
            rel_path: rel.to_string(),
            title: title.map(str::to_string),
            // The guillemets the UI turns into bold, around this occurrence and not around every
            // one on the line: two matches on one line are two rows, and each says which it is.
            snippet: format!(
                "{}«{}»{}",
                &text[..range.start],
                &text[range.clone()],
                &text[range.end..]
            ),
            at: Some(start..start + len),
            line: Some(line),
            more: 0,
        });
    }
    out
}

/// The slice of a matched line worth putting in a sidebar row, and where the match sits in it.
/// An elided end is marked with an ellipsis, so a clipped line does not read as the whole line.
fn clip(line: &str, range: Range<usize>) -> (String, Range<usize>) {
    let mut start = range.start.saturating_sub(CLIP_BEFORE);
    while start > 0 && !line.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = range.end.saturating_add(CLIP_AFTER).min(line.len());
    while end < line.len() && !line.is_char_boundary(end) {
        end += 1;
    }
    let lead = match start {
        0 => "",
        _ => "…",
    };
    let tail = match end < line.len() {
        true => "…",
        false => "",
    };
    let text = format!("{lead}{}{tail}", &line[start..end]);
    // `start` is never past the match, so the shift back onto the clipped text cannot underflow.
    let at = |i: usize| i - start + lead.len();
    (text, at(range.start)..at(range.end))
}

/// One row of either search: `rel_path, title, snippet_window(body, phrase), phrase_start(body,
/// phrase)`, turned into the hit the sidebar paints. `phrase` arrives folded, the way both SQL
/// functions read it.
fn hit(r: &rusqlite::Row<'_>, phrase: &str) -> rusqlite::Result<SearchHit> {
    let window: String = r.get(2)?;
    let start: Option<i64> = r.get(3)?;
    Ok(SearchHit {
        rel_path: r.get(0)?,
        title: r.get(1)?,
        // The window holds the same first occurrence `phrase_start` found — it starts
        // [`SNIPPET_LEAD`] characters ahead of it — so the phrase's length is measured over those
        // few hundred characters rather than over the note a second time. Folding can make it
        // differ from the needle's, which is why it is measured.
        at: start.map(|s| {
            let s = s as usize;
            let len = folded_find(&window, phrase).map_or(phrase.len(), |(_, n)| n);
            s..s + len
        }),
        snippet: mark_phrase(&window, phrase),
        // A file row until [`Index::per_match`] has read the body and found where in it the
        // phrase occurs; what is left of one is the hit whose body does not hold it at all.
        line: None,
        more: 0,
    })
}

/// The query's words: the tokens [`fts_query`] runs into one phrase, and — joined by a single
/// space — the needle the snippet is cut and marked around.
fn terms(query: &str) -> Vec<&str> {
    query.split_whitespace().collect()
}

/// How much of a note a snippet quotes, and how much of that comes before the term it found.
const SNIPPET_CHARS: usize = 240;

const SNIPPET_LEAD: usize = 40;

/// U+00C0..U+017F (Latin-1 Supplement and Latin Extended-A) folded to their unaccented ASCII
/// base, in code-point order; `_` means the character keeps itself, because nothing in ASCII
/// stands for it (æ, ð, ø, ß, ł and the two multiplication signs). Generated from Unicode NFD by
/// dropping the combining marks, which is what `remove_diacritics=2` does.
const LATIN_BASE: &[u8; 192] = b"aaaaaa_ceeeeiiii_nooooo__uuuuy__aaaaaa_ceeeeiiii_nooooo__uuuuy_y\
                                 aaaaaaccccccccdd__eeeeeeeeeegggggggghh__iiiiiiiii___jjkk_llllll_\
                                 ___nnnnnn___oooooo__rrrrrrsssssssstttt__uuuuuuuuuuuuwwyyyzzzzzz_";

/// Fold characters consistently for snippet selection and highlighting: lowercase and remove
/// Latin diacritics.
///
/// Approximates SQLite's unexposed `unicode61 remove_diacritics 2` table for Latin ranges, using
/// lowercase elsewhere.
fn fold_char(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match u32::from(c).checked_sub(0xC0).map(|i| i as usize) {
        Some(i) if i < LATIN_BASE.len() && LATIN_BASE[i] != b'_' => char::from(LATIN_BASE[i]),
        _ => c,
    }
}

pub(super) fn fold(s: &str) -> String {
    s.chars().map(fold_char).collect()
}

/// How many bytes of `hay` the folded `needle` matches at its start, or `None` if it does not.
/// `needle` is folded already; folding `hay` lazily is what keeps the byte offsets those of the
/// original text, which a fold that shortens `café` to `cafe` would otherwise lose.
///
/// A space in `needle` matches any run of whitespace, so a phrase still marks where the note wrote
/// it across two lines — which is how FTS5 matched it in the first place, tokens being adjacent
/// whatever separates them.
fn folded_prefix(hay: &str, needle: &str) -> Option<usize> {
    let mut hay = hay.chars().peekable();
    let mut used = 0;
    for w in needle.chars() {
        if w == ' ' {
            let before = used;
            while hay.peek().is_some_and(|c| c.is_whitespace()) {
                used += hay.next().expect("peeked").len_utf8();
            }
            if used == before {
                return None;
            }
            continue;
        }
        let c = hay.next()?;
        if fold_char(c) != w {
            return None;
        }
        used += c.len_utf8();
    }
    Some(used)
}

/// First folded match in `hay`, as its byte offset and source-byte length. Folding and
/// whitespace runs can change length relative to `needle`. An ASCII first-character check avoids
/// full folding at most positions.
pub(super) fn folded_find(hay: &str, needle: &str) -> Option<(usize, usize)> {
    let first = needle.chars().next().filter(|&w| w != ' ');
    hay.char_indices()
        .filter(|&(_, c)| first.is_none_or(|w| !c.is_ascii() || c.to_ascii_lowercase() == w))
        .find_map(|(i, _)| folded_prefix(&hay[i..], needle).map(|n| (i, n)))
}

/// Snippet around the first folded match: [`SNIPPET_LEAD`] preceding characters and
/// [`SNIPPET_CHARS`] total, with `…` at clipped ends. `needle` is already folded; local matching
/// handles diacritics that SQLite's ASCII-only `lower()` cannot.
pub(super) fn snippet_window(body: &str, needle: &str) -> String {
    // Characters, not bytes: the window is cut by character position on both ends.
    let hit = match folded_find(body, needle) {
        Some((at, _)) => body[..at].chars().count(),
        None => 0,
    };
    let start = hit.saturating_sub(SNIPPET_LEAD);
    let mut rest = body.chars().skip(start);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(rest.by_ref().take(SNIPPET_CHARS));
    if rest.next().is_some() {
        out.push('…');
    }
    out
}

/// Wrap every occurrence of the query phrase in the guillemets the UI turns into bold, the way
/// FTS5's own `snippet()` did. The input is the window [`snippet_window`] already cut, so this is
/// a pass over a row of text rather than over a note, and it folds the same way that cut did.
///
/// The phrase is marked as one run: the query matched those words in that order, and marking them
/// separately would highlight text the query did not find.
fn mark_phrase(window: &str, phrase: &str) -> String {
    let mut out = String::with_capacity(window.len() + 8);
    let mut rest = window;
    while !rest.is_empty() {
        // A zero-length match — an empty query — would mark nothing and never advance.
        match folded_prefix(rest, phrase).filter(|n| *n > 0) {
            Some(n) => {
                out.push('«');
                out.push_str(&rest[..n]);
                out.push('»');
                rest = &rest[n..];
            }
            None => {
                let c = rest.chars().next().expect("rest is not empty");
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

/// Quote a query as one safe FTS5 phrase with its last token matched as a prefix, e.g. `toggle
/// split vie` becomes `"toggle split vie"*`. Ordered words preserve sentence matching; the
/// prefix supports queries during typing. Operators are treated literally, preventing syntax
/// errors from user punctuation.
fn fts_query(q: &str) -> String {
    let toks = terms(q);
    if toks.is_empty() {
        return String::new();
    }
    format!("\"{}\"*", toks.join(" ").replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Change;
    use crate::index::testing::{fixture, open};
    use std::fs;

    /// Any of the words finds a file, each as the start of a word, and one row per file.
    #[test]
    fn rank_files_finds_any_word_once_per_file() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let words = |w: &[&str]| w.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(
            ix.rank_files(&words(&["ferr"]), 10).unwrap(),
            [("sub/Beta.md".to_string(), Some("Beta".to_string()))]
        );
        let both = ix
            .rank_files(&words(&["ferris", "alpha", "body"]), 10)
            .unwrap();
        let mut paths: Vec<&str> = both.iter().map(|(p, _)| p.as_str()).collect();
        paths.sort();
        assert_eq!(paths, ["a.md", "sub/Beta.md"]);
        assert!(ix.rank_files(&words(&[" "]), 10).unwrap().is_empty());
    }

    #[test]
    fn fts_search_finds_note_bodies() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let hits = ix.search("ferris", 10, false).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rel_path, "sub/Beta.md");
        assert!(hits[0].snippet.contains("ferris"), "{:?}", hits[0].snippet);

        // Conflicts are stored but never searchable.
        assert!(ix.search("conflicted", 10, false).unwrap().is_empty());
        // Garbage in must not be a SQL/FTS syntax error.
        assert!(
            ix.search("\"unbalanced AND *", 10, false)
                .unwrap()
                .is_empty()
        );
        assert!(ix.search("", 10, false).unwrap().is_empty());
    }

    #[test]
    fn a_snippet_marks_the_phrase_and_not_its_words_apart() {
        let phrase = fold("Ferris the crab");
        assert_eq!(
            mark_phrase("a FERRIS THE CRAB here", &phrase),
            "a «FERRIS THE CRAB» here"
        );
        // The words on their own are not what the query matched, so they are not marked.
        assert_eq!(
            mark_phrase("a crab, and Ferris too", &phrase),
            "a crab, and Ferris too"
        );
        // A phrase the note wrapped across two lines is one match, the way FTS5 read it.
        assert_eq!(
            mark_phrase("a ferris\nthe  crab here", &phrase),
            "a «ferris\nthe  crab» here"
        );
        assert_eq!(mark_phrase("as is", &fold("")), "as is");
        // The index folds diacritics to find the note, so the marking folds them to show why.
        assert_eq!(
            mark_phrase("un café au coin", &fold("cafe au")),
            "un «café au» coin"
        );
        assert_eq!(mark_phrase("ÄHNLICH", &fold("ahnlich")), "«ÄHNLICH»");
    }

    /// Phrase search must match adjacent ordered words rather than independent terms anywhere in
    /// a note.
    #[test]
    fn a_ranked_query_matches_the_whole_phrase() {
        let vault = tempfile::tempdir().unwrap();
        fs::write(
            vault.path().join("shortcuts.md"),
            "# Shortcuts\nUse Toggle Split View to open a second pane.\n",
        )
        .unwrap();
        fs::write(
            vault.path().join("scattered.md"),
            "# Notes\nToggle the sidebar. Split the day in two. A view of the lake.\n",
        )
        .unwrap();
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let hits = ix.search("Toggle Split Vie", 10, false).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.rel_path.as_str()).collect::<Vec<_>>(),
            ["shortcuts.md"],
            "only the note that runs the words together matches: {hits:?}"
        );
        assert!(hits[0].snippet.contains("«Toggle Split Vie»"), "{hits:?}");

        // The hit opens where the phrase is, not at the top of the note.
        let body = fs::read_to_string(vault.path().join("shortcuts.md")).unwrap();
        let at = hits[0].at.clone().expect("the body holds the phrase");
        assert_eq!(&body[at], "Toggle Split Vie");

        // A one-word query is what it always was.
        assert_eq!(ix.search("split", 10, false).unwrap().len(), 2);
    }

    #[test]
    fn folding_is_lower_case_without_the_latin_diacritics() {
        assert_eq!(fold("Café ÎLE Straße łódź"), "cafe ile straße łodz");
        // One character in, one out: the window is cut by character position, not by byte.
        assert_eq!(fold("Ünïcode").chars().count(), "Ünïcode".chars().count());
    }

    /// A window cut out of the middle of a note says so at the end it cut; one that starts where
    /// the note does must not claim otherwise.
    #[test]
    fn a_snippet_says_which_ends_it_cut() {
        assert_eq!(snippet_window("a café here", "cafe"), "a café here");

        let long: String = "wide ".repeat(200) + "café";
        let tail = snippet_window(&long, "cafe");
        assert!(tail.starts_with('…') && !tail.ends_with('…'), "{tail}");
        assert!(tail.contains("café"), "{tail}");

        // A term the note does not hold quotes the note's start, so only its end was cut.
        let head = snippet_window(&long, "zzz");
        assert!(!head.starts_with('…') && head.ends_with('…'), "{head}");
    }

    /// The whole of the diacritic path, through SQLite: the tokenizer finds the note through the
    /// fold, so the window has to be cut around the word that was found and the word marked.
    #[test]
    fn a_hit_found_by_folding_is_quoted_around_the_word_it_found() {
        let vault = tempfile::tempdir().unwrap();
        fs::write(
            vault.path().join("Paris.md"),
            format!("# Paris\n{}\nun café au coin\n", "filler prose ".repeat(60)),
        )
        .unwrap();
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let hits = ix.search("cafe", 10, false).unwrap();
        assert_eq!(hits.len(), 1, "the fold has to find it: {hits:?}");
        // The row is the line the word sits on, with the word marked as the note spells it.
        assert_eq!(
            hits[0].snippet, "un «café» au coin",
            "{:?}",
            hits[0].snippet
        );
        assert_eq!(hits[0].line, Some(3));
    }

    /// The ranked search lists what an exact one lists: a row per occurrence, not a row per file.
    #[test]
    fn a_ranked_search_lists_one_row_per_occurrence() {
        let vault = tempfile::tempdir().unwrap();
        fs::write(
            vault.path().join("a.md"),
            "# Alpha\nferris and ferris\nlater ferris\n",
        )
        .unwrap();
        // A title nothing in the body repeats, which is the hit that stays one row per file.
        fs::write(vault.path().join("b.md"), "# Ferris\nnothing else here\n").unwrap();
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let hits = ix.search("ferris", 10, false).unwrap();
        let rows: Vec<_> = hits
            .iter()
            .map(|h| (h.rel_path.as_str(), h.line, h.snippet.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("b.md", Some(1), "# «Ferris»"),
                ("a.md", Some(2), "«ferris» and ferris"),
                ("a.md", Some(2), "ferris and «ferris»"),
                ("a.md", Some(3), "later «ferris»"),
            ],
            "{hits:?}"
        );
        // The range still addresses the note, so each row opens on its own occurrence.
        let body = fs::read_to_string(vault.path().join("a.md")).unwrap();
        for hit in hits.iter().filter(|h| h.rel_path == "a.md") {
            let at = hit.at.clone().expect("a row is a place in the body");
            assert_eq!(&body[at], "ferris");
        }

        // The list's own cap counts rows, not files.
        let few = ix.search("ferris", 2, false).unwrap();
        assert_eq!(few.len(), 2, "{few:?}");
    }

    /// One file saying the query a hundred times is [`PER_FILE`] rows and a count, the way an
    /// exact search caps one: the rest of the ranking has to have room left.
    #[test]
    fn one_ranked_file_cannot_fill_the_list_on_its_own() {
        let vault = tempfile::tempdir().unwrap();
        fs::write(
            vault.path().join("generated.md"),
            "ferris\n".repeat(100).as_str(),
        )
        .unwrap();
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let hits = ix.search("ferris", 100, false).unwrap();
        assert_eq!(hits.len(), PER_FILE, "{hits:?}");
        assert_eq!(hits.last().map(|h| h.more), Some(100 - PER_FILE));
    }

    /// A file's "+N more" row, opened: the matches past its last listed row, found again in its
    /// text the way the listed ones were, at most as many as asked and the rest counted onto the
    /// last of them.
    #[test]
    fn the_rest_of_a_file_picks_up_where_its_rows_stopped() {
        let body = "ferris\n".repeat(12);
        let re = crate::search::pattern("ferris", crate::search::Options::default()).unwrap();
        let (listed, total) = Index::matches_from("a.md", None, &body, &re, 0, PER_FILE);
        assert_eq!((listed.len(), total, listed[4].more), (PER_FILE, 12, 7));
        let from = listed[4].offset + 1;
        let (rest, _) = Index::matches_from("a.md", None, &body, &re, from, 100);
        let lines: Vec<u32> = rest.iter().map(|m| m.line).collect();
        assert_eq!(lines, (6..=12).collect::<Vec<_>>());
        let (step, _) = Index::matches_from("a.md", None, &body, &re, from, 3);
        assert_eq!((step.len(), step[2].more), (3, 4));

        // The ranked phrase picks up where its last listed occurrence ends.
        let hits = Index::phrase_hits_from("a.md", &body, "Ferris", 0, PER_FILE);
        assert_eq!(hits[4].more, 7);
        let from = hits[4].at.clone().unwrap().end;
        let rest = Index::phrase_hits_from("a.md", &body, "Ferris", from, 100);
        let lines: Vec<Option<u32>> = rest.iter().map(|h| h.line).collect();
        assert_eq!(lines, (6..=12).map(Some).collect::<Vec<_>>());
        // A file changed since may put the place inside a character; that is no panic.
        assert_eq!(
            Index::phrase_hits_from("a.md", "é ferris", "ferris", 1, 9).len(),
            1
        );
    }

    #[test]
    fn grep_lists_one_row_per_match_with_its_line() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        fs::write(
            vault.path().join("a.md"),
            "# Alpha\nferris and ferris\nlater ferris\n",
        )
        .unwrap();
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let re = crate::search::pattern("ferris", crate::search::Options::default()).unwrap();
        let (hits, total) = ix.grep(&re, 10, false).unwrap();
        assert_eq!(total, 4, "three in a.md, one in sub/Beta.md: {hits:?}");
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0].rel_path, "a.md");
        assert_eq!((hits[0].line, hits[1].line, hits[2].line), (2, 2, 3));
        assert_eq!(&hits[0].line_text[hits[0].range.clone()], "ferris");
        assert_eq!(&hits[1].line_text[hits[1].range.clone()], "ferris");
        // The offset addresses the note, not the line, so opening it can place the caret.
        let body = fs::read_to_string(vault.path().join("a.md")).unwrap();
        assert_eq!(&body[hits[2].offset..hits[2].offset + 6], "ferris");

        // The cap truncates the list but not the count a Replace All is measured against.
        let (few, total) = ix.grep(&re, 2, false).unwrap();
        assert_eq!((few.len(), total), (2, 4));
        assert_eq!(ix.grep_paths(&re, false).unwrap(), ["a.md", "sub/Beta.md"]);
    }

    /// Per-file caps must leave room for other files and later unindexed results, while
    /// reporting omitted matches.
    #[test]
    fn one_file_cannot_fill_the_list_on_its_own() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        fs::write(
            vault.path().join("generated.md"),
            "ferris\n".repeat(100).as_str(),
        )
        .unwrap();
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let re = crate::search::pattern("ferris", crate::search::Options::default()).unwrap();
        let (hits, total) = ix.grep(&re, 100, false).unwrap();
        let listed = |rel: &str| hits.iter().filter(|m| m.rel_path == rel).count();
        assert_eq!(listed("generated.md"), PER_FILE);
        assert_eq!(
            listed("sub/Beta.md"),
            1,
            "the other file still fits: {hits:?}"
        );
        assert_eq!(total, 101, "the count a Replace All is measured against");
        // The rows the cap left out are named on the file's last row, not dropped in silence.
        let tail: Vec<usize> = hits
            .iter()
            .filter(|m| m.rel_path == "generated.md")
            .map(|m| m.more)
            .collect();
        assert_eq!(tail, [0, 0, 0, 0, 95]);

        // The list's own cap still holds, and it is shared: a file that fills it leaves the tail
        // count on whatever row it reached.
        let (few, _) = ix.grep(&re, 3, false).unwrap();
        assert_eq!(few.len(), 3);
        assert_eq!(few[2].more, 97);
    }

    /// Matches beginning in trimmed CRLF bytes must produce a valid empty range instead of
    /// panicking when sliced.
    #[test]
    fn a_match_in_a_crlf_line_ending_stays_inside_its_row() {
        let re = crate::search::pattern(
            r"\n",
            crate::search::Options {
                regex: true,
                ..Default::default()
            },
        )
        .unwrap();
        let (mut out, mut total) = (Vec::new(), 0);
        Index::matches_in(
            "crlf.md",
            None,
            "one\r\ntwo\r\n",
            &re,
            10,
            &mut out,
            &mut total,
        );
        assert_eq!(out.len(), 2);
        for m in &out {
            assert_eq!(m.line_text.get(m.range.clone()), Some(""), "{m:?}");
        }
    }

    #[test]
    fn clip_keeps_the_match_visible_in_a_long_line() {
        let line = format!("{}MATCH{}", "ä".repeat(500), "b".repeat(500));
        let at = line.find("MATCH").unwrap();
        let (text, range) = clip(&line, at..at + 5);
        assert_eq!(&text[range], "MATCH");
        assert!(text.starts_with('…') && text.ends_with('…'), "{text}");
        assert!(text.len() < 300, "{}", text.len());

        // A short line is passed through untouched.
        let (text, range) = clip("a MATCH b", 2..7);
        assert_eq!((text.as_str(), &text[range]), ("a MATCH b", "MATCH"));
    }

    /// Two notes for the ranking tests: the query is `target.md`'s title, and also a phrase
    /// `spam.md` repeats. Ranking on the body alone sorts these the wrong way round. `target.md`
    /// also carries the one phrase only the trigram index can find the middle of.
    fn ranking_vault() -> (tempfile::TempDir, tempfile::TempDir) {
        let vault = tempfile::tempdir().unwrap();
        fs::write(
            vault.path().join("target.md"),
            "# Quantum Coherence Ledger\nA short paragraph on where the numbers come from.\n\
             The toggle split view keeps both panes in step.\n",
        )
        .unwrap();
        // A long note that says the words a handful of times, which is what beat the note the
        // user was looking for before the title was indexed.
        fs::write(
            vault.path().join("spam.md"),
            format!(
                "# Meeting Notes\n{}",
                "quantum coherence ledger, plus a sentence of unrelated meeting prose. ".repeat(5)
            ),
        )
        .unwrap();
        (vault, tempfile::tempdir().unwrap())
    }

    #[test]
    fn exact_title_outranks_a_body_that_repeats_the_words() {
        let (vault, db) = ranking_vault();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // Ranking is over files; the rows are the matches in them, so the files are read off the
        // rows without their repeats.
        let hits = ix.search("Quantum Coherence Ledger", 10, false).unwrap();
        assert_eq!(files(&hits), ["target.md", "spam.md"], "{hits:?}");

        // Case and stray whitespace must not lose the exact-title match.
        let hits = ix.search("  quantum COHERENCE ledger ", 10, false).unwrap();
        assert_eq!(hits[0].rel_path, "target.md", "{hits:?}");
    }

    #[test]
    fn partial_title_match_outranks_a_body_match() {
        let (vault, db) = ranking_vault();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // Not the whole title, so only the bm25 title weight can decide this one.
        let hits = ix.search("coherence ledger", 10, false).unwrap();
        assert_eq!(files(&hits), ["target.md", "spam.md"], "{hits:?}");
    }

    /// The files a search answered with, in the order it ranked them and without the repeats the
    /// per-match rows make of one file.
    fn files(hits: &[SearchHit]) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for hit in hits {
            if out.last() != Some(&hit.rel_path.as_str()) {
                out.push(&hit.rel_path);
            }
        }
        out
    }

    /// The `notes_tri` half of the schema: a term index answers "starts with", so `oggle split`
    /// found nothing where `toggle split` found the note.
    #[test]
    fn a_query_that_starts_mid_word_finds_the_note_a_whole_word_finds() {
        let (vault, db) = ranking_vault();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let word = ix.search("toggle split", 10, false).unwrap();
        assert_eq!(word.len(), 1, "{word:?}");
        let mid = ix.search("oggle split", 10, false).unwrap();
        assert_eq!(mid.len(), 1, "{mid:?}");
        assert_eq!(mid[0].rel_path, word[0].rel_path);
        // Trigrams reach inside a word from either end, which a reversed-token column would not.
        assert_eq!(ix.search("ggle spl", 10, false).unwrap().len(), 1);

        // The hit is still a hit: the snippet marks what was found and the row opens on it.
        assert!(mid[0].snippet.contains('\u{ab}'), "{:?}", mid[0].snippet);
        let body = fs::read_to_string(vault.path().join("target.md")).unwrap();
        let at = mid[0].at.clone().expect("the body holds it");
        assert_eq!(&body[at], "oggle split");

        // A vault that does not hold the words is still no result, and nothing shorter than a
        // trigram is asked of the index at all.
        assert!(ix.search("zqxjv split", 10, false).unwrap().is_empty());
        assert!(ix.search("gg", 10, false).unwrap().is_empty());
    }

    /// `toggle` is a prefix hit in `target.md` and a mid-word one in `retoggle.md`: the prefix hit
    /// comes first, the mid-word one is listed below it, and neither twice.
    /// The mid-word path folds as the ranked one does, case and Latin diacritics, and takes the
    /// query as it is written: `%` and `_` are characters, not `LIKE` wildcards.
    #[test]
    fn a_mid_word_query_folds_and_is_literal() {
        let vault = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("u.md", "the Unicafé opens\n"),
            ("mini.md", "a minicafeteria\n"),
            ("share.md", "a 50% share\n"),
            ("five.md", "a 500 share\n"),
        ] {
            fs::write(vault.path().join(name), body).unwrap();
        }
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let hits = ix.search("CAFE", 10, false).unwrap();
        assert_eq!(files(&hits), ["mini.md", "u.md"], "shortest first");
        assert!(hits[1].snippet.contains("Uni«café»"), "{:?}", hits[1]);
        let hits = ix.infix("50%", 10, false).unwrap();
        assert_eq!(files(&hits), ["share.md"]);
    }

    #[test]
    fn mid_word_hits_come_below_the_prefix_ones_and_never_repeat_them() {
        let (vault, db) = ranking_vault();
        fs::write(vault.path().join("retoggle.md"), "Retoggle the pane.\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let prefix = ix.search("toggle", 10, false).unwrap();
        assert_eq!(files(&prefix), ["target.md"]);
        let skip: Vec<String> = files(&prefix).iter().map(|f| f.to_string()).collect();
        let mid = ix.search_mid_word("toggle", 10, false, &skip).unwrap();
        assert_eq!(files(&mid), ["retoggle.md"], "{mid:?}");
        assert_eq!(&mid[0].snippet, "Re«toggle» the pane.");

        // A query only the trigram index answers was answered by `search` already.
        assert_eq!(
            files(&ix.search("oggle split", 10, false).unwrap()),
            ["target.md"]
        );
        assert!(
            ix.search_mid_word("oggle split", 10, false, &[])
                .unwrap()
                .is_empty()
        );
    }

    /// The delete half of the FTS triggers, which is the half that fails silently: a stale title
    /// row would keep answering searches for a name the note no longer has.
    #[test]
    fn retitling_a_note_leaves_no_stale_title_in_the_index() {
        let (vault, db) = ranking_vault();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        fs::write(
            vault.path().join("target.md"),
            "# Photon Budget\nA short paragraph on where the numbers come from.\n",
        )
        .unwrap();
        assert_eq!(
            ix.update_file(vault.path(), "target.md").unwrap(),
            Change::Updated(FileKind::Markdown)
        );

        assert_eq!(
            ix.search("Photon Budget", 10, false).unwrap()[0].rel_path,
            "target.md"
        );
        let hits = ix.search("Quantum Coherence Ledger", 10, false).unwrap();
        assert_eq!(
            files(&hits),
            ["spam.md"],
            "the old title is still in the index"
        );
        // And no stale trigram either, which is what would answer the old title mid-word.
        let mid = ix.search("oherence Ledger", 10, false).unwrap();
        assert_eq!(
            files(&mid),
            ["spam.md"],
            "the old title is still in the trigram index"
        );
        for t in ["notes_fts", "notes_tri"] {
            ix.conn
                .execute(
                    &format!("INSERT INTO {t}({t}) VALUES('integrity-check')"),
                    [],
                )
                .unwrap();
        }
    }

    #[test]
    fn snippet_comes_from_the_body_not_the_title() {
        let vault = tempfile::tempdir().unwrap();
        // No H1 and no frontmatter, so the title is the file stem and lives only in the title
        // column: a hit on it can only quote the body.
        fs::write(
            vault.path().join("Kryptonite Ledger.md"),
            "plain prose about a lattice, never naming the file\n",
        )
        .unwrap();
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let hits = ix.search("kryptonite", 10, false).unwrap();
        assert_eq!(hits.len(), 1, "the title must be searchable: {hits:?}");
        assert!(
            hits[0].snippet.starts_with("plain prose"),
            "{:?}",
            hits[0].snippet
        );
        assert!(
            !hits[0].snippet.contains("Kryptonite"),
            "the snippet must quote the body, not the title: {:?}",
            hits[0].snippet
        );
        // And a hit the body does not hold names no place to open at, so it opens at the top.
        assert_eq!(hits[0].at, None);

        // The trigram index carries the title column as well, so the same title is reachable
        // from its middle — the body it would otherwise be found through never spells it.
        let hits = ix.search("ryptonite Ledg", 10, false).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rel_path, "Kryptonite Ledger.md");
    }
}
