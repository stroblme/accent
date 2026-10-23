use super::*;
impl Library {
    /// A move must not strand inheritance references in the source bibliography.
    pub fn move_remainder(&self, selected: &[usize]) -> Result<String> {
        let removed: HashSet<_> = selected
            .iter()
            .filter_map(|i| self.entries.get(*i))
            .map(|e| e.key.as_str())
            .collect();
        for (i, e) in self.entries.iter().enumerate() {
            if selected.contains(&i) {
                continue;
            }
            for field in ["crossref", "xref", "xdata"] {
                for key in self.value(e, field).split(',').map(str::trim) {
                    if removed.contains(key) {
                        return Err(invalid(format!(
                            "{} still depends on {key}; copy instead or select both entries",
                            e.key
                        )));
                    }
                }
            }
        }
        self.remove(selected)
    }

    /// Copy selected records plus their inheritance and string dependencies. Never overwrite a key.
    pub fn copy_entries(&self, selected: &[usize], destination: &str) -> Result<String> {
        let dest = Library::parse(destination)?;
        let mut needed = HashSet::new();
        let mut pending = selected.to_vec();
        while let Some(i) = pending.pop() {
            let e = self
                .entries
                .get(i)
                .ok_or_else(|| invalid("entry no longer exists"))?;
            if !needed.insert(i) {
                continue;
            }
            for field in ["crossref", "xref", "xdata"] {
                for key in self
                    .value(e, field)
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let j = self
                        .entries
                        .iter()
                        .position(|e| e.key == key)
                        .ok_or_else(|| invalid(format!("missing dependency: {key}")))?;
                    pending.push(j);
                }
            }
        }
        let mut strings = HashSet::new();
        let mut spans = Vec::new();
        if !needed.is_empty() {
            for (value, span) in &self.preambles {
                if !dest
                    .preambles
                    .iter()
                    .any(|(other, _)| dest.source[other.clone()] == self.source[value.clone()])
                {
                    spans.push(span.clone());
                }
                self.used_strings(&self.source[value.clone()], &mut strings);
            }
        }
        for &i in &needed {
            for f in &self.entries[i].fields {
                self.used_strings(&self.source[f.value.clone()], &mut strings);
            }
        }
        for name in strings {
            let (value, span) = &self.strings[&name];
            match dest.strings.get(&name) {
                Some((other, _)) if other != value => {
                    return Err(invalid(format!("string definition conflicts: {name}")));
                }
                None => spans.push(span.clone()),
                _ => {}
            }
        }
        for i in needed {
            let e = &self.entries[i];
            if let Some(existing) = dest.entries.iter().find(|other| other.key == e.key) {
                if dest.source[existing.span.clone()] != self.source[e.span.clone()] {
                    return Err(invalid(format!(
                        "citation key already exists: {} (compare or rename before copying)",
                        e.key
                    )));
                }
            } else {
                spans.push(e.span.clone());
            }
        }
        spans.sort_by_key(|s| s.start);
        let mut text = destination.to_string();
        for span in spans {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push('\n');
            text.push_str(&self.source[span]);
            text.push('\n');
        }
        Ok(text)
    }

    fn used_strings(&self, raw: &str, used: &mut HashSet<String>) {
        let mut p = Parser { text: raw, at: 0 };
        while p.at < raw.len() {
            p.space();
            if p.byte() == b'{' || p.byte() == b'"' {
                let open = p.byte();
                p.at += 1;
                if p.literal(open).is_err() {
                    break;
                }
            } else if p.byte().is_ascii_alphanumeric() {
                let start = p.at;
                while p.at < raw.len() && p.byte() != b'#' {
                    p.at += 1;
                }
                let name = raw[start..p.at].trim().to_ascii_lowercase();
                if let Some((value, _)) = self.strings.get(&name)
                    && used.insert(name)
                {
                    self.used_strings(value, used);
                }
            } else {
                p.at += 1;
            }
        }
    }
}
