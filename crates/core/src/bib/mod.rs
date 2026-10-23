//! Source-preserving BibTeX/BibLaTeX edits. Files, not the index, own the records.

mod files;
mod keys;
mod transfer;
pub use files::{Attachment, attachment_path, rebase_files};
pub use keys::PATTERN;

use crate::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub value: Range<usize>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub kind: String,
    pub key: String,
    pub span: Range<usize>,
    pub key_span: Range<usize>,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug)]
pub struct Library {
    pub source: String,
    pub entries: Vec<Entry>,
    pub(crate) strings: HashMap<String, (String, Range<usize>)>,
    preambles: Vec<(Range<usize>, Range<usize>)>,
}

fn invalid(why: impl Into<String>) -> Error {
    Error::Invalid(why.into())
}

impl Library {
    pub fn parse(source: &str) -> Result<Self> {
        let mut out = Self {
            source: source.into(),
            entries: Vec::new(),
            strings: HashMap::new(),
            preambles: Vec::new(),
        };
        let mut p = Parser {
            text: source,
            at: 0,
        };
        while p.at < source.len() {
            p.space();
            if p.at == source.len() {
                break;
            }
            if p.byte() != b'@' {
                p.at += 1;
                continue;
            }
            let start = p.at;
            p.at += 1;
            let kind = p.name()?.to_ascii_lowercase();
            p.space();
            let close = match p.byte() {
                b'{' => b'}',
                b'(' => b')',
                _ => return Err(p.error("expected { or (")),
            };
            p.at += 1;
            if kind == "comment" {
                if close == b'}' {
                    p.literal(b'{')?;
                } else {
                    let mut depth = 1;
                    while p.at < source.len() && depth > 0 {
                        match p.byte() {
                            b'(' => depth += 1,
                            b')' => depth -= 1,
                            _ => {}
                        }
                        p.at += 1;
                    }
                    if depth != 0 {
                        return Err(p.error("unterminated comment"));
                    }
                }
                continue;
            }
            if kind == "preamble" {
                let value = p.expression(close)?;
                if p.byte() == b',' {
                    p.at += 1;
                    p.space();
                }
                p.expect(close)?;
                out.preambles.push((value, start..p.at));
                continue;
            }
            if kind == "string" {
                let name = p.name()?.to_ascii_lowercase();
                p.space();
                p.expect(b'=')?;
                p.space();
                let value = p.expression(close)?;
                p.space();
                if p.byte() == b',' {
                    p.at += 1;
                }
                p.space();
                p.expect(close)?;
                if out
                    .strings
                    .insert(name.clone(), (source[value].into(), start..p.at))
                    .is_some()
                {
                    return Err(invalid(format!("duplicate string definition: {name}")));
                }
                continue;
            }
            p.space();
            let key_start = p.at;
            while p.at < source.len() && !matches!(p.byte(), b',' | b'}' | b')') {
                p.at += 1;
            }
            let key = source[key_start..p.at].trim();
            if !valid_key(key) {
                return Err(p.error("invalid citation key"));
            }
            let key_span = key_start..key_start + key.len();
            let mut fields = Vec::new();
            if p.byte() != close {
                p.expect(b',')?;
            }
            loop {
                p.space();
                if p.byte() == close {
                    p.at += 1;
                    break;
                }
                let name = p.name()?.to_ascii_lowercase();
                if fields.iter().any(|f: &Field| f.name == name) {
                    return Err(p.error("duplicate field"));
                }
                p.space();
                p.expect(b'=')?;
                p.space();
                let value = p.expression(close)?;
                if value.is_empty() {
                    return Err(p.error("missing field value"));
                }
                fields.push(Field { name, value });
                p.space();
                if p.byte() == b',' {
                    p.at += 1;
                } else if p.byte() != close {
                    return Err(p.error("expected comma"));
                }
            }
            out.entries.push(Entry {
                kind,
                key: key.into(),
                span: start..p.at,
                key_span,
                fields,
            });
        }
        Ok(out)
    }

    pub fn raw<'a>(&'a self, entry: &Entry, field: &str) -> Option<&'a str> {
        entry
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(field))
            .map(|f| &self.source[f.value.clone()])
    }

    pub fn value(&self, entry: &Entry, field: &str) -> String {
        self.raw(entry, field)
            .map(|s| self.expand(s, &mut HashSet::new()))
            .unwrap_or_default()
    }

    fn expand(&self, raw: &str, seen: &mut HashSet<String>) -> String {
        let mut p = Parser { text: raw, at: 0 };
        let mut result = String::new();
        while p.at < raw.len() {
            p.space();
            let start = p.at;
            if p.byte() == b'{' || p.byte() == b'"' {
                let quote = p.byte();
                p.at += 1;
                let inner = p.at;
                if p.literal(quote).is_err() {
                    return raw.into();
                }
                result.push_str(&raw[inner..p.at - 1]);
            } else {
                while p.at < raw.len() && p.byte() != b'#' {
                    p.at += 1;
                }
                let atom = raw[start..p.at].trim();
                let name = atom.to_ascii_lowercase();
                if let Some((value, _)) = self.strings.get(&name) {
                    if seen.insert(name.clone()) {
                        result.push_str(&self.expand(value, seen));
                        seen.remove(&name);
                    } else {
                        result.push_str(atom);
                    }
                } else {
                    result.push_str(atom);
                }
            }
            p.space();
            if p.byte() == b'#' {
                p.at += 1;
            }
        }
        result
    }

    pub fn set_field(&self, index: usize, name: &str, value: &str) -> Result<String> {
        // Parse the proposed value in context so braces/quotes cannot corrupt the library.
        let literal = format!("{{{value}}}");
        self.set_raw(index, name, &literal)
    }

    pub fn set_raw(&self, index: usize, name: &str, raw: &str) -> Result<String> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-:".contains(&b))
        {
            return Err(invalid("invalid field name"));
        }
        let probe = Self::parse(&format!("@misc{{probe,{name}={raw}}}"))?;
        if probe.entries.len() != 1 || probe.entries[0].fields.len() != 1 {
            return Err(invalid("invalid field value"));
        }
        let entry = self
            .entries
            .get(index)
            .ok_or_else(|| invalid("entry no longer exists"))?;
        let mut text = self.source.clone();
        if let Some(field) = entry
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
        {
            text.replace_range(field.value.clone(), raw);
        } else {
            let mut end = entry.span.end - 1;
            let last = entry
                .fields
                .last()
                .map_or(entry.key_span.end, |f| f.value.end);
            let mut parser = Parser {
                text: &self.source,
                at: last,
            };
            parser.space();
            if parser.byte() != b',' {
                text.insert(last, ',');
                end += 1;
            }
            text.insert_str(end, &format!("\n  {name} = {raw},\n"));
        }
        let parsed = Self::parse(&text)?;
        if parsed.entries.len() != self.entries.len() {
            return Err(invalid("invalid field value"));
        }
        Ok(text)
    }

    pub fn remove(&self, indices: &[usize]) -> Result<String> {
        let mut spans: Vec<_> = indices
            .iter()
            .map(|i| {
                self.entries
                    .get(*i)
                    .map(|e| e.span.clone())
                    .ok_or_else(|| invalid("entry no longer exists"))
            })
            .collect::<Result<_>>()?;
        spans.sort_by_key(|s| s.start);
        spans.dedup();
        let mut text = self.source.clone();
        for span in spans.into_iter().rev() {
            text.replace_range(span, "");
        }
        Ok(text)
    }

    /// Rename explicitly; callers must review the citations using the old key first.
    pub fn rename(&self, index: usize, key: &str) -> Result<String> {
        if !valid_key(key) {
            return Err(invalid("invalid citation key"));
        }
        if self
            .entries
            .iter()
            .enumerate()
            .any(|(i, e)| i != index && e.key == key)
        {
            return Err(invalid(format!("citation key already exists: {key}")));
        }
        let entry = self
            .entries
            .get(index)
            .ok_or_else(|| invalid("entry no longer exists"))?;
        let mut text = self.source.clone();
        text.replace_range(entry.key_span.clone(), key);
        Self::parse(&text)?.redirect(&entry.key, key)
    }

    /// Redirect inheritance when two records are merged, keeping unrelated expressions intact.
    pub fn redirect(&self, old: &str, new: &str) -> Result<String> {
        if !valid_key(new) {
            return Err(invalid("invalid citation key"));
        }
        let mut text = self.source.clone();
        let mut edits = Vec::new();
        for e in &self.entries {
            for f in &e.fields {
                if !matches!(f.name.as_str(), "crossref" | "xref" | "xdata") {
                    continue;
                }
                let value = self.value(e, &f.name);
                if !value.split(',').any(|k| k.trim() == old) {
                    continue;
                }
                let keys = value
                    .split(',')
                    .map(str::trim)
                    .map(|k| if k == old { new } else { k })
                    .collect::<Vec<_>>();
                edits.push((f.value.clone(), format!("{{{}}}", keys.join(","))));
            }
        }
        for (span, value) in edits.into_iter().rev() {
            text.replace_range(span, &value);
        }
        Ok(text)
    }

    pub fn issues(&self) -> Vec<(usize, String)> {
        let mut issues = Vec::new();
        let mut seen = HashSet::new();
        for (i, e) in self.entries.iter().enumerate() {
            if !seen.insert(&e.key) {
                issues.push((i, "Duplicate citation key".into()));
            }
            let inherited = self.raw(e, "crossref").is_some() || self.raw(e, "xdata").is_some();
            for field in ["title", "author", "year"] {
                if inherited || matches!(e.kind.as_str(), "xdata" | "set") {
                    continue;
                }
                let missing = self.value(e, field).is_empty()
                    && match field {
                        "author" => {
                            self.value(e, "editor").is_empty()
                                && !matches!(
                                    e.kind.as_str(),
                                    "misc" | "online" | "manual" | "proceedings"
                                )
                        }
                        "year" => self.value(e, "date").is_empty(),
                        _ => true,
                    };
                if missing {
                    issues.push((i, format!("Missing {field}")));
                }
            }
            if !self.value(e, "doi").is_empty() && doi(&self.value(e, "doi")).is_none() {
                issues.push((i, "Malformed DOI".into()));
            }
            for field in ["crossref", "xref", "xdata"] {
                for key in self
                    .value(e, field)
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if !self.entries.iter().any(|e| e.key == key) {
                        issues.push((i, format!("Missing {field} entry: {key}")));
                    }
                }
            }
        }
        issues
    }

    pub fn duplicates(&self) -> Vec<(usize, usize)> {
        let mut pairs = Vec::new();
        let mut identities: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, e) in self.entries.iter().enumerate() {
            let d = doi(&self.value(e, "doi"));
            let title = keys::plain(&self.value(e, "title"));
            let author = keys::plain(&self.value(e, "author"));
            let signature = format!("{title}|{author}|{}", self.year(e));
            let mut candidates = HashSet::new();
            for identity in d.iter().map(|d| format!("doi:{d}")).chain(
                (!title.is_empty() && !author.is_empty()).then(|| format!("title:{signature}")),
            ) {
                for &j in identities.get(&identity).into_iter().flatten() {
                    candidates.insert(j);
                }
                identities.entry(identity).or_default().push(i);
            }
            for j in candidates {
                let other = &self.entries[j];
                let different = ["doi", "version"].iter().any(|f| {
                    let a = self.value(e, f);
                    let b = self.value(other, f);
                    let (a, b) = if *f == "doi" {
                        (doi(&a).unwrap_or(a), doi(&b).unwrap_or(b))
                    } else {
                        (a, b)
                    };
                    !a.is_empty() && !b.is_empty() && a != b
                });
                if !different {
                    pairs.push((j, i));
                }
            }
        }
        pairs.sort_unstable();
        pairs
    }

    pub fn year(&self, e: &Entry) -> String {
        let year = self.value(e, "year");
        if year.is_empty() {
            self.value(e, "date").chars().take(4).collect()
        } else {
            year
        }
    }
}

pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key
            .chars()
            .any(|c| c.is_whitespace() || ",{}()\\\"#%=;".contains(c))
}

/// DOI identifiers and resolver URLs only; a publisher URL is not itself an identifier.
pub fn doi(input: &str) -> Option<String> {
    let input = input.trim();
    let lower = input.to_ascii_lowercase();
    let raw = [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi:",
    ]
    .iter()
    .find_map(|prefix| lower.strip_prefix(prefix).map(|_| &input[prefix.len()..]))
    .unwrap_or(input)
    .trim();
    let mut bytes = Vec::new();
    let mut i = 0;
    let data = raw.as_bytes();
    while i < data.len() {
        if data[i] == b'%' {
            bytes.push(u8::from_str_radix(raw.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            bytes.push(data[i]);
            i += 1;
        }
    }
    let value = String::from_utf8(bytes).ok()?.to_lowercase();
    let (prefix, suffix) = value.split_once('/')?;
    let digits = prefix.strip_prefix("10.")?;
    (digits.len() >= 4
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !suffix.is_empty()
        && !value.chars().any(|c| c.is_whitespace() || c.is_control()))
    .then_some(value)
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
}
impl Parser<'_> {
    fn byte(&self) -> u8 {
        self.text.as_bytes().get(self.at).copied().unwrap_or(0)
    }
    fn error(&self, why: &str) -> Error {
        invalid(format!("{why} at byte {}", self.at + 1))
    }
    fn expect(&mut self, b: u8) -> Result<()> {
        if self.byte() != b {
            return Err(self.error(&format!("expected {}", char::from(b))));
        }
        self.at += 1;
        Ok(())
    }
    fn space(&mut self) {
        loop {
            while self.byte().is_ascii_whitespace() {
                self.at += 1;
            }
            if self.byte() != b'%' {
                break;
            }
            while self.at < self.text.len() && self.byte() != b'\n' {
                self.at += 1;
            }
        }
    }
    fn name(&mut self) -> Result<String> {
        self.space();
        let start = self.at;
        while self.byte().is_ascii_alphanumeric() || b"_-:".contains(&self.byte()) {
            self.at += 1;
        }
        if start == self.at {
            return Err(self.error("expected a name"));
        }
        Ok(self.text[start..self.at].into())
    }
    fn literal(&mut self, open: u8) -> Result<()> {
        let mut depth = if open == b'{' { 1 } else { 0 };
        while self.at < self.text.len() {
            let b = self.byte();
            self.at += 1;
            match b {
                b'\\' => {
                    if self.at < self.text.len() {
                        self.at += 1;
                    }
                }
                b'{' => depth += 1,
                b'}' => {
                    if depth == 0 {
                        return Err(self.error("unbalanced brace"));
                    }
                    depth -= 1;
                    if open == b'{' && depth == 0 {
                        return Ok(());
                    }
                }
                b'"' if open == b'"' && depth == 0 => return Ok(()),
                _ => {}
            }
        }
        Err(self.error("unterminated value"))
    }
    fn expression(&mut self, close: u8) -> Result<Range<usize>> {
        let start = self.at;
        loop {
            self.space();
            match self.byte() {
                b'{' | b'"' => {
                    let open = self.byte();
                    self.at += 1;
                    self.literal(open)?;
                }
                b if b.is_ascii_alphanumeric() || b == b'_' => {
                    self.name()?;
                }
                _ => return Err(self.error("expected a literal or string name")),
            }
            let end = self.at;
            self.space();
            match self.byte() {
                b'#' => {
                    self.at += 1;
                }
                b',' => return Ok(start..end),
                b if b == close => return Ok(start..end),
                _ => return Err(self.error("expected #, comma or end of entry")),
            }
        }
    }
}

#[cfg(test)]
mod tests;
