//! Words for prose: what the document already says, then what the dictionary knows.
//!
//! A note or a LaTeX file is answered by a provider that speaks its structure (wikilinks,
//! `\cite`) and by nothing for the prose in between. This is the second source, layered under
//! the first: the word being typed is completed from the words this document already uses, most
//! used first, and then from the system's hunspell word list, which is the same list libspelling
//! checks the text against. It is also the seam a model-backed suggestion would plug into: a
//! third source in [`Layered`], answering the same shape.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;

use super::external::line_of;
use super::{
    Completion, Completions, Fold, Fut, Hover, Kind, Language, Location, Pos, Range, Signature,
    Support, Symbol,
};
use crate::locked;

/// Language ids whose files are prose enough to want words. `text` is what a file with no
/// language at all is opened as.
pub(crate) const PROSE: &[&str] = &["markdown", "latex", "bibtex", "rst", "text"];

/// Fewer typed characters than this offer nothing: one letter matches too much of any list.
const MIN_PREFIX: usize = 2;
/// Items a single answer holds; more than that says `incomplete` and is asked again as the
/// word grows.
const CAP: usize = 40;
/// A word has to be this long to be worth offering; shorter ones are typed faster than picked.
const MIN_WORD: usize = 3;

fn is_word(c: char) -> bool {
    c.is_alphabetic() || c == '\''
}

/// The word the caret is at the end of: where it starts on the line, and its text. `None` when
/// the caret is not inside one, when it is too short, or when what precedes it belongs to
/// another provider (`\cite`, `#tag`, `[[Note`, `@key`, a path).
pub(crate) fn word_before(line: &str, character: u32) -> Option<(u32, String)> {
    let chars: Vec<char> = line.chars().take(character as usize).collect();
    let start = chars
        .iter()
        .rposition(|c| !is_word(*c))
        .map_or(0, |i| i + 1);
    if chars.len() - start < MIN_PREFIX {
        return None;
    }
    if start > 0 && matches!(chars[start - 1], '\\' | '#' | '[' | '@' | '/') {
        return None;
    }
    Some((start as u32, chars[start..].iter().collect()))
}

/// Every word of `text`, spelled as the document spells it, with the key a query matches on and
/// how often it occurs.
///
/// The lower-cased key is stored rather than folded per query: a completion is asked for on every
/// keystroke and would otherwise re-case every distinct word in the document each time.
fn counts(text: &str) -> HashMap<String, (String, usize)> {
    let mut counts: HashMap<String, (String, usize)> = HashMap::new();
    for word in text.split(|c: char| !is_word(c)) {
        if word.chars().count() >= MIN_WORD {
            counts
                .entry(word.to_string())
                .or_insert_with(|| (word.to_lowercase(), 0))
                .1 += 1;
        }
    }
    counts
}

/// Where the system keeps the dictionary of the session's language, hunspell's plain-text
/// `.dic`: one stem per line with its affix flags after a slash, and the count on the first.
fn dictionary_path() -> Option<PathBuf> {
    let lang = std::env::var("LANG")
        .ok()
        .and_then(|l| l.split('.').next().map(str::to_string))
        .filter(|l| !l.is_empty() && l != "C" && l != "POSIX")
        .unwrap_or_else(|| "en_US".to_string());
    ["/usr/share/hunspell", "/usr/share/myspell/dicts"]
        .iter()
        .map(|dir| PathBuf::from(dir).join(format!("{lang}.dic")))
        .find(|p| p.is_file())
}

/// The dictionary's stems, lower-cased, sorted, read once for the life of the process.
///
/// ponytail: stems only. `abandon/DSG` is offered as `abandon`; the affixes that would make
/// `abandoned` are not expanded. Empty where no dictionary is installed.
fn dictionary() -> &'static [String] {
    static DICT: OnceLock<Vec<String>> = OnceLock::new();
    DICT.get_or_init(|| {
        let Some(path) = dictionary_path() else {
            return Vec::new();
        };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut words: Vec<String> = text
            .lines()
            .skip(1)
            .map(|l| l.split('/').next().unwrap_or_default().to_lowercase())
            .filter(|w| w.chars().count() >= MIN_WORD && w.chars().all(is_word))
            .collect();
        words.sort();
        words.dedup();
        tracing::debug!("{} dictionary words from {}", words.len(), path.display());
        words
    })
}

/// A candidate written the way the typed prefix is: `The` completes to `Theorem`, `the` to
/// `theorem`; a word the document spells its own way is left alone.
fn cased(word: &str, prefix: &str) -> String {
    let upper = prefix.chars().next().is_some_and(char::is_uppercase);
    let mut chars = word.chars();
    match (upper, chars.next()) {
        (true, Some(first)) if first.is_lowercase() => {
            first.to_uppercase().collect::<String>() + chars.as_str()
        }
        _ => word.to_string(),
    }
}

/// One open document: its text, for the line the caret is on, and its words, for the ranking.
struct Doc {
    text: String,
    /// As written -> (what a query matches, how often it occurs).
    counts: HashMap<String, (String, usize)>,
}

/// The word source for one vault's prose documents.
#[derive(Default)]
pub(crate) struct Words {
    docs: Mutex<HashMap<String, Doc>>,
}

impl Words {
    pub(crate) fn open(&self, rel: &str, text: String) {
        let counts = counts(&text);
        locked(&self.docs).insert(rel.to_string(), Doc { text, counts });
    }

    pub(crate) fn close(&self, rel: &str) {
        locked(&self.docs).remove(rel);
    }

    /// The words that continue what is being typed: the document's own, most used first, then
    /// the dictionary's, in its order. The word under the caret is never offered to itself.
    pub(crate) fn completion(&self, rel: &str, pos: Pos) -> Completions {
        let docs = locked(&self.docs);
        let Some(doc) = docs.get(rel) else {
            return Completions::default();
        };
        let Some((start, typed)) = word_before(line_of(&doc.text, pos.line), pos.character) else {
            // Nothing yet, but say so as "not yet": the popup asks once when a word starts and
            // only narrows after that, so an answer that closed the question at one letter
            // would never see the second.
            return Completions {
                items: Vec::new(),
                incomplete: true,
            };
        };
        let prefix = typed.to_lowercase();
        let replace = Range {
            start: Pos {
                line: pos.line,
                character: start,
            },
            end: pos,
        };

        let mut own: Vec<(&String, &(String, usize))> = doc
            .counts
            .iter()
            .filter(|(w, (low, n))| low.starts_with(&prefix) && (**w != typed || *n > 1))
            .collect();
        own.sort_by(|a, b| b.1.1.cmp(&a.1.1).then_with(|| a.0.cmp(b.0)));
        let mut seen: HashSet<&str> = HashSet::new();
        let mut labels: Vec<String> = own
            .into_iter()
            .take(CAP)
            .filter(|(_, (low, _))| seen.insert(low))
            .map(|(w, _)| w.clone())
            .collect();

        let dict = dictionary();
        let from = dict.partition_point(|w| w.as_str() < prefix.as_str());
        for word in dict[from..].iter().take_while(|w| w.starts_with(&prefix)) {
            if labels.len() >= CAP {
                break;
            }
            // The dictionary knows the word being typed too; it is not a suggestion.
            if *word != prefix && seen.insert(word) {
                labels.push(cased(word, &typed));
            }
        }

        let incomplete = labels.len() >= CAP;
        Completions {
            items: labels
                .into_iter()
                .map(|word| Completion {
                    label: word.clone(),
                    kind: Kind::Text,
                    detail: None,
                    doc: None,
                    filter: None,
                    insert: word,
                    is_snippet: false,
                    replace,
                    extra_edits: Vec::new(),
                    resolve: None,
                })
                .collect(),
            incomplete,
        }
    }
}

/// A prose document's providers, answering as one: the primary (a language server, or the index
/// for a note) for everything it does, with the words appended to its completion and the ghost
/// session answering beside both. A file with no primary at all (a `.txt`, a `.tex` without
/// texlab) still gets its words.
///
/// The ghost session hears the document's whole life — open, change, close — because it answers
/// about the buffer as it is now. What it does not hear is every save: it re-reads the vault on
/// one, so it is told when the user leaves the document instead. See [`Layered::settle`].
pub(crate) struct Layered {
    primary: Option<Arc<dyn Language>>,
    ghost: Option<Arc<dyn Language>>,
    words: Words,
    /// The document was saved since the ghost session last heard about it.
    stale: AtomicBool,
}

impl Layered {
    pub(crate) fn new(
        primary: Option<Arc<dyn Language>>,
        ghost: Option<Arc<dyn Language>>,
    ) -> Layered {
        Layered {
            primary,
            ghost,
            words: Words::default(),
            stale: AtomicBool::new(false),
        }
    }
}

impl Language for Layered {
    fn open(&self, rel: &str, language_id: &str, text: String) -> Result<Support> {
        self.words.open(rel, text.clone());
        if let Some(g) = &self.ghost {
            g.open(rel, language_id, text.clone())?;
        }
        let mut support = match &self.primary {
            Some(p) => p.open(rel, language_id, text)?,
            None => Support::default(),
        };
        support.inline = self.ghost.is_some();
        Ok(support)
    }

    fn change(&self, rel: &str, text: String) -> Result<()> {
        self.words.open(rel, text.clone());
        if let Some(g) = &self.ghost {
            g.change(rel, text.clone())?;
        }
        match &self.primary {
            Some(p) => p.change(rel, text),
            None => Ok(()),
        }
    }

    fn saved(&self, rel: &str) -> Result<()> {
        self.stale.store(true, Ordering::Relaxed);
        self.primary.as_ref().map_or(Ok(()), |p| p.saved(rel))
    }

    /// The one save the ghost session is told about, and only if there was one: it re-reads the
    /// whole vault on a `didSave`, which is a second's work on a large one. Leaving the document
    /// is the moment where that is affordable and where it is worth doing.
    fn settle(&self, rel: &str) -> Result<()> {
        match &self.ghost {
            Some(g) if self.stale.swap(false, Ordering::Relaxed) => g.saved(rel),
            _ => Ok(()),
        }
    }

    fn close(&self, rel: &str) {
        self.words.close(rel);
        if let Some(g) = &self.ghost {
            g.close(rel);
        }
        if let Some(p) = &self.primary {
            p.close(rel);
        }
    }

    fn inline_completion(&self, rel: &str, pos: Pos) -> Fut<'_, Option<String>> {
        match &self.ghost {
            Some(g) => g.inline_completion(rel, pos),
            None => Box::pin(async { Ok(None) }),
        }
    }

    fn completion(&self, rel: &str, pos: Pos, trigger: Option<char>) -> Fut<'_, Completions> {
        let rel = rel.to_string();
        Box::pin(async move {
            let mut answer = match &self.primary {
                Some(p) => p.completion(&rel, pos, trigger).await?,
                None => Completions::default(),
            };
            let words = self.words.completion(&rel, pos);
            let taken: HashSet<String> = answer.items.iter().map(|c| c.label.clone()).collect();
            answer.items.extend(
                words
                    .items
                    .into_iter()
                    .filter(|c| !taken.contains(&c.label)),
            );
            answer.incomplete |= words.incomplete;
            Ok(answer)
        })
    }

    fn resolve(&self, rel: &str, item: Completion) -> Fut<'_, Completion> {
        match &self.primary {
            Some(p) if item.resolve.is_some() => p.resolve(rel, item),
            _ => Box::pin(async move { Ok(item) }),
        }
    }

    fn signature_help(&self, rel: &str, pos: Pos) -> Fut<'_, Option<Signature>> {
        match &self.primary {
            Some(p) => p.signature_help(rel, pos),
            None => Box::pin(async { Ok(None) }),
        }
    }

    fn hover(&self, rel: &str, pos: Pos) -> Fut<'_, Option<Hover>> {
        match &self.primary {
            Some(p) => p.hover(rel, pos),
            None => Box::pin(async { Ok(None) }),
        }
    }

    fn definition(&self, rel: &str, pos: Pos) -> Fut<'_, Vec<Location>> {
        match &self.primary {
            Some(p) => p.definition(rel, pos),
            None => Box::pin(async { Ok(Vec::new()) }),
        }
    }

    fn symbols(&self, rel: &str) -> Fut<'_, Vec<Symbol>> {
        match &self.primary {
            Some(p) => p.symbols(rel),
            None => Box::pin(async { Ok(Vec::new()) }),
        }
    }

    fn references(&self, rel: &str, pos: Pos) -> Fut<'_, Vec<Location>> {
        match &self.primary {
            Some(p) => p.references(rel, pos),
            None => Box::pin(async { Ok(Vec::new()) }),
        }
    }

    fn folds(&self, rel: &str) -> Fut<'_, Vec<Fold>> {
        match &self.primary {
            Some(p) => p.folds(rel),
            None => Box::pin(async { Ok(Vec::new()) }),
        }
    }

    /// ponytail: the ghost session is not counted. A dead merl would otherwise restart the
    /// primary with it; it stays dead until the tab is reopened, which is the cheaper mistake.
    fn is_dead(&self) -> bool {
        self.primary.as_ref().is_some_and(|p| p.is_dead())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_before_the_caret_is_found_unless_it_is_someone_elses() {
        assert_eq!(word_before("see the", 7), Some((4, "the".into())));
        assert_eq!(word_before("see t", 5), None, "too short");
        assert_eq!(word_before("see the ", 8), None, "not inside a word");
        assert_eq!(word_before("\\cite", 5), None, "a command is texlab's");
        assert_eq!(word_before("a #tag", 6), None, "a tag is the note's");
        assert_eq!(word_before("[[Note", 6), None, "a link is the note's");
        assert_eq!(word_before("héllo wörld", 11), Some((6, "wörld".into())));
    }

    #[test]
    fn the_documents_own_words_come_first_most_used_first() {
        let words = Words::default();
        words.open("a.md", "theorem theory theorem\nthe".to_string());
        let answer = words.completion(
            "a.md",
            Pos {
                line: 1,
                character: 3,
            },
        );
        let labels: Vec<&str> = answer.items.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(&labels[..2], ["theorem", "theory"]);
        assert!(
            !labels.contains(&"the"),
            "the word being typed is not offered to itself"
        );
        assert_eq!(answer.items[0].replace.start.character, 0);
        assert_eq!(answer.items[0].replace.end.character, 3);
    }

    #[test]
    fn one_letter_is_too_soon_but_not_the_end_of_the_question() {
        let words = Words::default();
        words.open("a.txt", "hello\nh".to_string());
        let answer = words.completion(
            "a.txt",
            Pos {
                line: 1,
                character: 1,
            },
        );
        assert!(answer.items.is_empty());
        assert!(answer.incomplete, "the next letter has to ask again");
    }

    #[test]
    fn a_capital_prefix_gets_a_capital_word() {
        assert_eq!(cased("theorem", "The"), "Theorem");
        assert_eq!(cased("theorem", "the"), "theorem");
        assert_eq!(
            cased("NASA", "na"),
            "NASA",
            "the document's own spelling stays"
        );
    }
}
