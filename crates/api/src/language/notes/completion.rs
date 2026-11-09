//! Completion: links, tags and anchors as the caret's context asks for them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::Result;

use accent_core::index::Index;
use accent_core::markdown;
use accent_core::note::Syntax;
use accent_core::org;
use accent_core::path::{self, FileType, basename, parent_dir, stem};

use super::pure::{
    Trigger, block_links, context, fragment_end, front_tag, has_headings, heading_names,
    link_names, org_context, page_links, path_candidates, ranked_paths, tag_candidates, yaml_item,
};
use super::{Corpora, Corpus, Notes, empty_item, pdf_outline};
use crate::language::{Completion, Completions, Kind, Pos, Range, byte_of};
use crate::{Local, locked};

impl Notes {
    /// Every tag a completion offers in the note at `rel`, which holds `text`: those the notes
    /// most like it carry first ([`Index::similar_tags`]), less the ones it has already, then the
    /// rest by use. The suggestions are worked out once a note until [`Index::data_version`]
    /// changes, about 50 ms on a vault of 3600 notes, not at each keystroke.
    fn tags_for(&self, index: &Index, rel: &str, text: &str) -> Result<Vec<(String, i64)>> {
        let version = index.data_version()?;
        let suggested = {
            let mut cache = locked(&self.suggested);
            if cache.0 != version {
                *cache = (version, HashMap::new());
            }
            match cache.1.get(rel) {
                Some(tags) => tags.clone(),
                None => {
                    let tags = Arc::new(index.similar_tags(rel, text)?);
                    cache.1.insert(rel.to_string(), tags.clone());
                    tags
                }
            }
        };
        let own: HashSet<String> = Syntax::of(rel)
            .analyze(text)
            .tags
            .into_iter()
            .map(|tag| tag.name)
            .collect();
        let mut tags = index.tags()?;
        // A stable sort: the rest keep their order by use behind the suggestions.
        tags.sort_by_cached_key(|(name, _)| {
            suggested
                .iter()
                .position(|s| s == name)
                .filter(|_| !own.contains(name))
                .unwrap_or(usize::MAX)
        });
        Ok(tags)
    }

    /// [`front_tag`]'s completion: the tags [`Notes::tags_for`] offers, each written as the list
    /// writes an item, with no `#`.
    fn front_tags(&self, rel: &str, text: &str, caret: usize, pos: Pos) -> Result<Completions> {
        let Some((start, flow)) = front_tag(text, caret) else {
            return Ok(Completions::default());
        };
        // The item goes on past the caret to the `,` or `]` ending it in a flow list, to the end of
        // the line in a block one.
        let after = &text[caret..text[caret..].find('\n').map_or(text.len(), |i| caret + i)];
        let rest = match flow {
            true => &after[..after.find([',', ']']).unwrap_or(after.len())],
            false => after,
        }
        .trim_end();
        let tags = self.tags_for(&locked(&self.index), rel, text)?;
        let (hits, more) = tag_candidates(&tags, &text[start..caret]);
        let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
        let replace = Range {
            start: Pos {
                line: pos.line,
                character: text[line_start..start].chars().count() as u32,
            },
            end: Pos {
                line: pos.line,
                character: pos.character + rest.chars().count() as u32,
            },
        };
        Ok(Completions {
            items: hits
                .into_iter()
                .map(|name| Completion {
                    insert: yaml_item(&name, flow),
                    label: name,
                    kind: Kind::Tag,
                    replace,
                    ..empty_item()
                })
                .collect(),
            incomplete: more,
            fragment: Some(replace),
        })
    }

    /// Cache path-completion corpora until [`Index::data_version`] or the ignored
    /// listing changes. `wiki` ranks notes/PDFs; embed and markdown completions
    /// rank all files. Avoid rereading thousands of paths on each capped-list keystroke.
    fn corpus(&self, index: &Index, wiki: bool) -> Result<Arc<Corpus>> {
        let version = index.data_version()?;
        let listing = self.ignored.listing(false);
        let mut corpora = locked(&self.corpora);
        let walked = corpora
            .listing
            .as_ref()
            .is_some_and(|l| Arc::ptr_eq(l, &listing));
        if corpora.version != version || !walked {
            *corpora = Corpora {
                version,
                listing: Some(listing.clone()),
                ..Corpora::default()
            };
        }
        let slot = match wiki {
            true => &mut corpora.wiki,
            false => &mut corpora.files,
        };
        if let Some(corpus) = slot {
            return Ok(corpus.clone());
        }
        let (mut paths, mut missing, aliases) = match wiki {
            true => (
                index.note_and_pdf_paths()?,
                index.missing_notes()?,
                index.note_aliases()?,
            ),
            false => (index.file_paths(false)?, Vec::new(), Vec::new()),
        };
        // `[[` names a note in an ignored folder too, behind the files the index holds, and a
        // link to one is no note to write.
        let start = paths.len();
        if wiki {
            let held: HashSet<&str> = paths.iter().map(String::as_str).collect();
            let notes: Vec<String> = listing
                .notes()
                .filter(|rel| !held.contains(rel.as_str()))
                .cloned()
                .collect();
            paths.extend(notes);
            missing.retain(|rel| listing.resolve(rel).is_none());
        }
        let ignored = start..paths.len();
        // Behind the files, so one that is there leads a note only linked to at the same rank:
        // the ranking is stable.
        paths.extend(missing.iter().cloned());
        // The aliases last, ranked by their own names, and told apart by where they sit.
        let named = paths.len();
        paths.extend(aliases.iter().map(|(alias, _)| alias.clone()));
        let corpus = Arc::new(Corpus {
            paths,
            ignored,
            named,
            missing,
            aliases,
        });
        *slot = Some(corpus.clone());
        Ok(corpus)
    }

    pub(super) fn completion(&self, rel: &str, pos: Pos) -> Result<Completions> {
        let text = self.text_of(rel)?;
        let Some(caret) = byte_of(&text, pos) else {
            return Ok(Completions::default());
        };
        if Syntax::of(rel) == Syntax::Org {
            return self.org_completion(rel, &text, caret, pos);
        }
        let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
        let line_end = text[caret..].find('\n').map_or(text.len(), |i| caret + i);
        // The rows are matched by the fragment up to the caret, and replace all of it, what of it
        // follows the caret too.
        let line = &text[line_start..line_end];
        let Some((trigger, start, prefix)) = context(&line[..caret - line_start]) else {
            return self.front_tags(rel, &text, caret, pos);
        };
        let head = &line[..fragment_end(line, caret - line_start, context, markdown::is_tag_char)];
        let at = |byte: usize| Pos {
            line: pos.line,
            character: head[..byte].chars().count() as u32,
        };
        let mut replace = Range {
            start: at(start),
            end: at(head.len()),
        };
        // The `[[` or the `#` it opens with is not its text: a caret before that has left it.
        let lead = match trigger {
            Trigger::Wiki | Trigger::Embed => 2,
            Trigger::Tag => 1,
            Trigger::Anchor | Trigger::Path => 0,
        };
        let fragment = Some(Range {
            start: at(start + lead),
            end: replace.end,
        });

        match trigger {
            Trigger::Wiki | Trigger::Embed => {
                // `typing::pair` closed the `[` as it was typed, so the fragment usually ends in
                // front of the `]]` it left. The inserted link brings its own, so they go too.
                let eaten = line[head.len()..]
                    .chars()
                    .take(2)
                    .take_while(|c| *c == ']')
                    .count();
                replace.end.character += eaten as u32;
                if let Some((note, anchor)) = prefix.split_once('#') {
                    let answer = self.heading_links(rel, note, anchor.starts_with('^'), replace)?;
                    return Ok(Completions { fragment, ..answer });
                }

                let index = locked(&self.index);
                let corpus = self.corpus(&index, trigger == Trigger::Wiki)?;
                let Corpus {
                    paths,
                    ignored,
                    named,
                    missing,
                    aliases,
                } = &*corpus;
                let (ranked, more) = ranked_paths(paths, prefix);
                let mut items = Vec::with_capacity(ranked.len());
                for i in ranked {
                    // The link names the file, as any other row writes it, and reads as the
                    // alias: an alias is a name to find a note by, never a link target.
                    if let Some((alias, rel)) = i.checked_sub(*named).map(|j| &aliases[j]) {
                        let (name, path) = link_names(rel);
                        let target = match index.resolve_target(&name)?.as_ref() == Some(rel) {
                            true => name,
                            false => path,
                        };
                        items.push(Completion {
                            insert: format!("[[{target}|{alias}]]"),
                            detail: Some(rel.clone()),
                            filter: Some(format!("[[{alias}")),
                            label: alias.clone(),
                            kind: Kind::File,
                            replace,
                            ..empty_item()
                        });
                        continue;
                    }
                    let hit = paths[i].clone();
                    // A second link to a note not written yet, spelled from the root the way
                    // New File will place it, so both reach it once it is.
                    if missing.contains(&hit) {
                        let path = markdown::strip_ext(&hit);
                        items.push(Completion {
                            insert: format!("[[{path}]]"),
                            detail: Some(format!("{hit}, not created")),
                            filter: Some(format!("[[{hit}")),
                            label: stem(&hit),
                            kind: Kind::File,
                            replace,
                            ..empty_item()
                        });
                        continue;
                    }
                    let (name, path) = link_names(&hit);
                    // The bare name only while it reaches this file: where a shorter path
                    // answers to it first, the link has to spell the path out.
                    let bare = self.find(&index, &name)?.as_ref() == Some(&hit);
                    items.push(Completion {
                        insert: match bare {
                            true => format!("[[{name}]]"),
                            false => format!("[[{path}]]"),
                        },
                        // Said, as the tree dims it: search does not reach it.
                        detail: match ignored.contains(&i) {
                            true => Some(format!("{hit}, ignored")),
                            false => (!bare).then(|| hit.clone()),
                        },
                        // The popup narrows by what was typed since the `[[`, which a bare
                        // name never matches; the path lets a folder narrow it too. The path
                        // as the index holds it, extension and all, because that is what the
                        // row was ranked against: `[[Note.md` has to keep `Note.md` on screen.
                        filter: Some(format!("[[{hit}")),
                        label: name,
                        kind: Kind::File,
                        replace,
                        ..empty_item()
                    });
                }
                Ok(Completions {
                    items,
                    incomplete: more,
                    fragment,
                })
            }
            Trigger::Anchor => {
                // The path before the `#` names a note from this one's folder, and no path names
                // this note. A file that is not a note has no headings to offer.
                let dest = head[start..].split_once('#').map_or("", |(dest, _)| dest);
                let other = match dest {
                    "" => None,
                    _ => {
                        let target =
                            path::resolve(parent_dir(rel), &markdown::percent_decode(dest));
                        match self.find(&locked(&self.index), &target)? {
                            Some(note) if has_headings(&note) => {
                                Some((Syntax::of(&note), self.text_of(&note)?))
                            }
                            _ => return Ok(Completions::default()),
                        }
                    }
                };
                let (syntax, text) = match &other {
                    Some((syntax, other)) => (*syntax, other.as_str()),
                    None => (Syntax::Markdown, text.as_str()),
                };
                if prefix.starts_with('^') {
                    return Ok(Completions {
                        items: block_links(text, &format!("{dest}#"), "", replace),
                        fragment,
                        ..Completions::default()
                    });
                }
                let headings = syntax.analyze(text).headings;
                let slugs = markdown::slugs(headings.iter().map(|h| h.text.as_str()));
                let items = headings
                    .iter()
                    .zip(slugs)
                    .filter(|(h, _)| !h.text.trim().is_empty())
                    .map(|(h, slug)| {
                        let anchor = format!("#{slug}");
                        Completion {
                            label: h.text.trim().to_string(),
                            // What goes in is not what the row reads, so its anchor is shown too.
                            detail: Some(anchor.clone()),
                            filter: Some(format!("{dest}{anchor}")),
                            insert: format!("{dest}{anchor}"),
                            replace,
                            ..empty_item()
                        }
                    })
                    .collect();
                Ok(Completions {
                    items,
                    incomplete: false,
                    fragment,
                })
            }
            Trigger::Path => {
                let files = self.corpus(&locked(&self.index), false)?;
                // Matched against vault paths, so what was typed loses its encoding, and the `./`
                // and `../` a relative link opens with, which say where from rather than what.
                let query = markdown::percent_decode(prefix);
                let (hits, more) =
                    path_candidates(&files.paths, query.trim_start_matches(['.', '/']));
                let dir = parent_dir(rel);
                let items = hits
                    .into_iter()
                    .map(|hit| {
                        // From this note's folder, which is where the index resolves it from.
                        let path = path::relative(dir, &hit);
                        let link = markdown::percent_encode(&path);
                        // The popup narrows by what is typed after the `(`, the name as it reads
                        // or as the link spells it, so the row answers to both.
                        let filter = match path == link {
                            true => link.clone(),
                            false => format!("{path} {link}"),
                        };
                        Completion {
                            label: basename(&hit).to_string(),
                            // What goes in is not what the row reads, so it is shown as well.
                            detail: Some(link.clone()),
                            filter: Some(filter),
                            insert: link,
                            kind: Kind::File,
                            replace,
                            ..empty_item()
                        }
                    })
                    .collect();
                Ok(Completions {
                    items,
                    incomplete: more,
                    fragment,
                })
            }
            Trigger::Tag => {
                let tags = self.tags_for(&locked(&self.index), rel, &text)?;
                let (hits, more) = tag_candidates(&tags, prefix);
                Ok(Completions {
                    items: hits
                        .into_iter()
                        .map(|name| Completion {
                            insert: format!("#{name}"),
                            label: format!("#{name}"),
                            kind: Kind::Tag,
                            replace,
                            ..empty_item()
                        })
                        .collect(),
                    incomplete: more,
                    fragment,
                })
            }
        }
    }

    /// Org's completions ([`org_context`]): after `[[` a file, as org links one from this file's
    /// folder, `[[file:<path>][<title>]]`, a note's or an org file's title its description; in a
    /// heading's tags a tag org can spell, with the colon closing it.
    fn org_completion(&self, rel: &str, text: &str, caret: usize, pos: Pos) -> Result<Completions> {
        let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
        let line_end = text[caret..].find('\n').map_or(text.len(), |i| caret + i);
        let line = &text[line_start..line_end];
        let Some((trigger, start, prefix)) = org_context(&line[..caret - line_start]) else {
            return Ok(Completions::default());
        };
        let head = &line[..fragment_end(line, caret - line_start, org_context, org::is_tag_char)];
        let at = |byte: usize| Pos {
            line: pos.line,
            character: head[..byte].chars().count() as u32,
        };
        let mut replace = Range {
            start: at(start),
            end: at(head.len()),
        };
        // A link's text is past its `[[`; a tag's starts past its colon already.
        let lead = match trigger {
            Trigger::Wiki => 2,
            _ => 0,
        };
        let fragment = Some(Range {
            start: at(start + lead),
            end: replace.end,
        });
        let after = &line[head.len()..];
        let index = locked(&self.index);
        if trigger == Trigger::Tag {
            let tags: Vec<(String, i64)> = self
                .tags_for(&index, rel, text)?
                .into_iter()
                .filter(|(name, _)| name.chars().all(org::is_tag_char))
                .collect();
            let (hits, more) = tag_candidates(&tags, prefix);
            let close = match after.starts_with(':') {
                true => "",
                false => ":",
            };
            return Ok(Completions {
                items: hits
                    .into_iter()
                    .map(|name| Completion {
                        insert: format!("{name}{close}"),
                        label: name,
                        kind: Kind::Tag,
                        replace,
                        ..empty_item()
                    })
                    .collect(),
                incomplete: more,
                fragment,
            });
        }
        // `[[*Heading]]` and a search name a place in a file, not a file to list.
        if prefix.starts_with('*') || prefix.contains("::") {
            return Ok(Completions::default());
        }
        // The `]]` `typing::pair` left after the link's text: the link brings its own.
        let eaten = after.chars().take(2).take_while(|c| *c == ']').count();
        replace.end.character += eaten as u32;
        let files = self.corpus(&index, false)?;
        let query = prefix.strip_prefix("file:").unwrap_or(prefix);
        let (hits, more) = path_candidates(&files.paths, query.trim_start_matches(['.', '/']));
        let dir = parent_dir(rel);
        let mut items = Vec::with_capacity(hits.len());
        for hit in hits {
            let path = path::relative(dir, &hit);
            let title = match has_headings(&hit) {
                true => index.get_file(&hit)?.and_then(|row| row.title),
                false => None,
            };
            items.push(Completion {
                label: basename(&hit).to_string(),
                detail: Some(path.clone()),
                filter: Some(format!("[[file:{path}")),
                insert: org::link(&format!("file:{path}"), title.as_deref()),
                kind: Kind::File,
                replace,
                ..empty_item()
            });
        }
        Ok(Completions {
            items,
            incomplete: more,
            fragment,
        })
    }

    /// Complete headings or `^id` blocks while retaining the typed target.
    /// An empty target uses this note; PDF targets offer bookmark page anchors.
    /// Missing targets and other file types offer nothing.
    fn heading_links(
        &self,
        rel: &str,
        note: &str,
        blocks: bool,
        replace: Range,
    ) -> Result<Completions> {
        let target = match note {
            "" => rel.to_string(),
            _ => match self.find(&locked(&self.index), note)? {
                Some(target) => target,
                None => return Ok(Completions::default()),
            },
        };
        if path::file_type(&target) == FileType::Pdf {
            let outline = pdf_outline(&Local::join(&self.root, &target)?)?;
            return Ok(Completions {
                items: page_links(&outline, note, replace),
                ..Completions::default()
            });
        }
        if !has_headings(&target) {
            return Ok(Completions::default());
        }
        let text = self.text_of(&target)?;
        if blocks {
            return Ok(Completions {
                items: block_links(&text, &format!("[[{note}#"), "]]", replace),
                ..Completions::default()
            });
        }
        let items = heading_names(&Syntax::of(&target).analyze(&text).headings)
            .into_iter()
            .map(|name| {
                let link = format!("[[{note}#{name}");
                Completion {
                    label: name.to_string(),
                    insert: format!("{link}]]"),
                    filter: Some(link),
                    replace,
                    ..empty_item()
                }
            })
            .collect();
        Ok(Completions {
            items,
            ..Completions::default()
        })
    }
}
