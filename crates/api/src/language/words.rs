//! Words for prose: what the document already says, then what the dictionary knows.
//!
//! A note or a LaTeX file is answered by a provider that speaks its structure (wikilinks,
//! `\cite`) and by nothing for the prose in between. This is the second source, layered under
//! the first: the word being typed is completed from the words this document already uses, most
//! used first, and then from the system's hunspell word list, which is the same list libspelling
//! checks the text against. It is also the seam a model-backed suggestion would plug into: a
//! third source in [`Layered`], answering the same shape.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;

use super::external::map::line_of;
use super::{
    Completion, Completions, Fold, Fut, Hover, Kind, Language, Listing, Location, Pos, Range,
    Signature, Support, Symbol, latex,
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

/// The dictionary while word suggestions are on: read on the first word asked for, and let go
/// when they are switched off ([`forget_dictionary`]). One for the process, as the system's
/// dictionary is.
static DICTIONARY: Mutex<Option<Arc<Vec<String>>>> = Mutex::new(None);

/// The dictionary's stems, lower-cased and sorted, read if they are not held already.
///
/// ponytail: stems only. `abandon/DSG` is offered as `abandon`; the affixes that would make
/// `abandoned` are not expanded. Empty where no dictionary is installed.
fn dictionary() -> Arc<Vec<String>> {
    locked(&DICTIONARY)
        .get_or_insert_with(|| Arc::new(read_dictionary()))
        .clone()
}

/// Let the dictionary go: word suggestions are off, and nothing asks for it until they are on.
pub(crate) fn forget_dictionary() {
    if locked(&DICTIONARY).take().is_some() {
        tracing::debug!("dictionary let go");
    }
}

fn read_dictionary() -> Vec<String> {
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

    fn text(&self, rel: &str) -> Option<String> {
        locked(&self.docs).get(rel).map(|doc| doc.text.clone())
    }

    fn line(&self, rel: &str, line: u32) -> Option<String> {
        locked(&self.docs)
            .get(rel)
            .map(|doc| line_of(&doc.text, line).to_string())
    }

    /// The words that continue what is being typed: the document's own, most used first, then
    /// the dictionary's, in its order. The word under the caret is never offered to itself.
    pub(crate) fn completion(&self, rel: &str, pos: Pos) -> Completions {
        let docs = locked(&self.docs);
        let Some(doc) = docs.get(rel) else {
            return Completions::default();
        };
        let Some((start, typed)) = word_before(line_of(&doc.text, pos.line), pos.character) else {
            return Completions::default();
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
            pages: None,
        }
    }
}

/// Where a document's ghost session comes from whenever it has no live one: the vault's, started
/// if need be, or `None` while the vault has none to give (Ghost Text off, or given up on).
pub(crate) type Respawn =
    Box<dyn Fn() -> Pin<Box<dyn Future<Output = Option<Arc<dyn Language>>> + Send>> + Send + Sync>;

/// A prose document's providers, answering as one: the primary (a language server, or the index
/// for a note) for everything it does, with the words appended to its completion and the ghost
/// session answering beside both. A file with no primary at all (a `.txt`, a `.tex` without
/// texlab) still gets its words. Inside a LaTeX `\input{…}` the folder's files take the words'
/// place.
///
/// The ghost session hears the document's whole life — open, change, close — because it answers
/// about the buffer as it is now. What it does not hear is every save: it re-reads the vault on
/// one, so it is told when the user leaves the document instead. See [`Layered::settle`].
///
/// A ghost session that exits, or that the vault shut down with Ghost Text, is replaced on the
/// next suggestion by whatever the vault has then ([`Layered::ghost`]), and the primary never
/// hears of it: what the ghost session fails to take is no one else's failure.
pub(crate) struct Layered {
    primary: Option<Arc<dyn Language>>,
    /// The ghost session the document is open on, if it has one.
    ghost: Mutex<Option<Arc<dyn Language>>>,
    /// The vault's ghost session, asked for whenever [`Layered::ghost`] holds no live one;
    /// `None` where there is no ghost text to be had at all (no `merl-rt`).
    respawn: Option<Respawn>,
    /// The protocol's name for the document's language, to open it on a fresh ghost session.
    language_id: OnceLock<String>,
    words: Words,
    /// The Word Suggestions preference, the vault's: off, the words stay out of the answer.
    words_on: Arc<AtomicBool>,
    /// The vault's listing, for a LaTeX document's `\input{` ([`latex::inputs`]).
    listing: Option<Arc<Listing>>,
    /// The document was saved since the ghost session last heard about it.
    stale: AtomicBool,
}

impl Layered {
    pub(crate) fn new(
        primary: Option<Arc<dyn Language>>,
        ghost: Option<Arc<dyn Language>>,
        respawn: Option<Respawn>,
        words_on: Arc<AtomicBool>,
        listing: Option<Arc<Listing>>,
    ) -> Layered {
        Layered {
            primary,
            ghost: Mutex::new(ghost),
            respawn,
            language_id: OnceLock::new(),
            words: Words::default(),
            words_on,
            listing,
            stale: AtomicBool::new(false),
        }
    }

    /// The files `\input{` may name that texlab leaves out, while the caret is inside its braces.
    fn inputs(&self, rel: &str, pos: Pos) -> Option<Completions> {
        let listing = self.listing.as_ref()?;
        let line = self.words.line(rel, pos.line)?;
        let (dir, start) = latex::input_dir(rel, &line, pos.character)?;
        let rows = listing.list_dir(&dir).unwrap_or_else(|e| {
            tracing::debug!(dir, "listing for \\input: {e:#}");
            Vec::new()
        });
        let replace = Range {
            start: Pos {
                line: pos.line,
                character: start,
            },
            end: pos,
        };
        Some(Completions {
            items: latex::inputs(rows, replace),
            ..Completions::default()
        })
    }

    /// Tell the ghost session something, if there is a live one. A failure is logged and goes
    /// no further: a `merl-rt` that has exited must not take the primary's news down with it.
    fn tell_ghost(&self, tell: impl FnOnce(&dyn Language) -> Result<()>) {
        let ghost = locked(&self.ghost).clone();
        if let Some(ghost) = ghost.filter(|g| !g.is_dead())
            && let Err(e) = tell(ghost.as_ref())
        {
            tracing::debug!("ghost text: {e:#}");
        }
    }

    /// The ghost session to ask about `rel`: the one the document is open on while it lives,
    /// else the vault's, and the document opened on that one as it reads now.
    async fn ghost(&self, rel: &str) -> Option<Arc<dyn Language>> {
        let gone = match &*locked(&self.ghost) {
            Some(ghost) if !ghost.is_dead() => return Some(ghost.clone()),
            gone => gone.clone(),
        };
        let fresh = match &self.respawn {
            Some(respawn) => respawn().await,
            None => None,
        };
        let mut slot = locked(&self.ghost);
        let same = match (&*slot, &gone) {
            (Some(now), Some(gone)) => Arc::ptr_eq(now, gone),
            (now, gone) => now.is_none() && gone.is_none(),
        };
        // Another request got there first.
        if !same {
            return slot.clone();
        }
        if let Some(ghost) = &fresh {
            let (id, text) = (self.language_id.get(), self.words.text(rel));
            let id = id.map_or("", String::as_str);
            if let Err(e) = ghost.open(rel, id, text.unwrap_or_default()) {
                tracing::debug!("ghost text: {e:#}");
            }
        }
        slot.clone_from(&fresh);
        fresh
    }
}

impl Language for Layered {
    fn open(&self, rel: &str, language_id: &str, text: String) -> Result<Support> {
        self.words.open(rel, text.clone());
        let _ = self.language_id.set(language_id.to_string());
        self.tell_ghost(|g| g.open(rel, language_id, text.clone()).map(drop));
        let mut support = match &self.primary {
            Some(p) => p.open(rel, language_id, text)?,
            None => Support::default(),
        };
        // Whether ghost text can be had here at all; whether it is on is the UI's to ask.
        support.inline = self.respawn.is_some();
        Ok(support)
    }

    fn change(&self, rel: &str, text: String) -> Result<()> {
        self.words.open(rel, text.clone());
        self.tell_ghost(|g| g.change(rel, text.clone()));
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
        if self.stale.swap(false, Ordering::Relaxed) {
            self.tell_ghost(|g| g.saved(rel));
        }
        Ok(())
    }

    /// The primary's to answer: neither the words nor the ghost session diagnoses anything.
    fn rediagnose(&self, rel: &str) -> Result<()> {
        self.primary.as_ref().map_or(Ok(()), |p| p.rediagnose(rel))
    }

    fn close(&self, rel: &str) {
        self.words.close(rel);
        self.tell_ghost(|g| {
            g.close(rel);
            Ok(())
        });
        if let Some(p) = &self.primary {
            p.close(rel);
        }
    }

    fn inline_completion(&self, rel: &str, pos: Pos) -> Fut<'_, Option<String>> {
        let rel = rel.to_string();
        Box::pin(async move {
            match self.ghost(&rel).await {
                Some(g) => g.inline_completion(&rel, pos).await,
                None => Ok(None),
            }
        })
    }

    fn completion(&self, rel: &str, pos: Pos, trigger: Option<char>) -> Fut<'_, Completions> {
        let rel = rel.to_string();
        Box::pin(async move {
            // A primary that fails, a server still indexing say, takes nothing from the words.
            let mut answer = match &self.primary {
                Some(p) => p
                    .completion(&rel, pos, trigger)
                    .await
                    .inspect_err(|e| tracing::debug!("completion for {rel}: {e:#}"))
                    .unwrap_or_default(),
                None => Completions::default(),
            };
            // A path has no use for prose words.
            let mut words = match self.inputs(&rel, pos) {
                Some(files) => files,
                None if self.words_on.load(Ordering::Relaxed) => self.words.completion(&rel, pos),
                None => Completions::default(),
            };
            // Nor has a link or a tag: an item of the primary's starting before the word does is
            // completing more than the word, `[[My No` or `#project/ph`, and a word put in there
            // would write prose into the link.
            let start = words.items.first().map(|w| w.replace.start);
            if start.is_some_and(|start| answer.items.iter().any(|i| i.replace.start < start)) {
                words = Completions::default();
            }
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

    /// The primary's alone: a ghost session that exits is started again by itself.
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

    /// Word Suggestions off: the document's words and the dictionary's go from the answer, and
    /// on again brings them back.
    #[test]
    fn word_suggestions_switched_off_offer_no_words() {
        let on = Arc::new(AtomicBool::new(false));
        let doc = Layered::new(None, None, None, on.clone(), None);
        doc.open("a.md", "markdown", "theorem theory\nthe".into())
            .unwrap();
        let at = Pos {
            line: 1,
            character: 3,
        };
        let words = || {
            accent_lsp::runtime()
                .block_on(doc.completion("a.md", at, None))
                .unwrap()
                .items
                .len()
        };
        assert_eq!(words(), 0);
        on.store(true, Ordering::Relaxed);
        assert!(words() >= 2, "theorem and theory");
    }

    /// The dictionary is let go, and read anew on the next word asked for.
    #[test]
    fn the_dictionary_is_let_go_and_read_again() {
        let first = dictionary();
        forget_dictionary();
        assert!(!Arc::ptr_eq(&first, &dictionary()));
    }

    #[test]
    fn one_letter_is_too_soon() {
        let words = Words::default();
        words.open("a.txt", "hello\nh".to_string());
        let answer = words.completion(
            "a.txt",
            Pos {
                line: 1,
                character: 1,
            },
        );
        assert_eq!(answer, Completions::default());
    }

    /// A primary that fails is no reason to go without the words.
    #[test]
    fn a_failing_primary_still_leaves_the_words() {
        let primary = Fake::new("primary");
        primary.dead.store(true, Ordering::Relaxed);
        let doc = Layered::new(
            Some(primary),
            None,
            None,
            Arc::new(AtomicBool::new(true)),
            None,
        );
        doc.open("a.md", "markdown", "theorem theory\nthe".into())
            .unwrap_or_default();
        let at = Pos {
            line: 1,
            character: 3,
        };
        let answer = accent_lsp::runtime().block_on(doc.completion("a.md", at, None));
        assert!(answer.unwrap().items.len() >= 2, "theorem and theory");
    }

    /// Inside a link the note's rows are the whole answer: a word would be prose in the link.
    #[test]
    fn a_link_being_typed_gets_no_words() {
        let primary = Fake::new("primary");
        let line = |character| Pos { line: 0, character };
        locked(&primary.items).push(Completion {
            label: "My Note".into(),
            kind: Kind::File,
            detail: None,
            doc: None,
            filter: None,
            insert: "[[My Note]]".into(),
            is_snippet: false,
            replace: Range {
                start: line(0),
                end: line(7),
            },
            extra_edits: Vec::new(),
            resolve: None,
        });
        let doc = Layered::new(
            Some(primary),
            None,
            None,
            Arc::new(AtomicBool::new(true)),
            None,
        );
        doc.open("a.md", "markdown", "[[My No\nNotable notes".into())
            .unwrap();
        let labels = |character| {
            accent_lsp::runtime()
                .block_on(doc.completion("a.md", line(character), None))
                .unwrap()
                .items
                .into_iter()
                .map(|c| c.label)
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(7), ["My Note"]);
    }

    /// A provider that says what it heard and answers a suggestion with its own name, until it
    /// is made to exit.
    struct Fake {
        name: &'static str,
        dead: AtomicBool,
        heard: Mutex<Vec<String>>,
        /// What it completes with, wherever it is asked.
        items: Mutex<Vec<Completion>>,
    }

    impl Fake {
        fn new(name: &'static str) -> Arc<Fake> {
            Arc::new(Fake {
                name,
                dead: AtomicBool::new(false),
                heard: Mutex::new(Vec::new()),
                items: Mutex::new(Vec::new()),
            })
        }

        fn hear(&self, what: String) -> Result<()> {
            anyhow::ensure!(!self.is_dead(), "{} has exited", self.name);
            locked(&self.heard).push(what);
            Ok(())
        }

        fn heard(&self) -> Vec<String> {
            locked(&self.heard).clone()
        }
    }

    impl Language for Fake {
        fn open(&self, rel: &str, _: &str, text: String) -> Result<Support> {
            self.hear(format!("open {rel} {text}"))?;
            Ok(Support::default())
        }
        fn change(&self, rel: &str, text: String) -> Result<()> {
            self.hear(format!("change {rel} {text}"))
        }
        fn close(&self, _: &str) {}
        fn inline_completion(&self, _: &str, _: Pos) -> Fut<'_, Option<String>> {
            Box::pin(async move {
                anyhow::ensure!(!self.is_dead(), "{} has exited", self.name);
                Ok(Some(self.name.to_string()))
            })
        }
        fn completion(&self, _: &str, _: Pos, _: Option<char>) -> Fut<'_, Completions> {
            Box::pin(async {
                anyhow::ensure!(!self.is_dead(), "{} has exited", self.name);
                Ok(Completions {
                    items: locked(&self.items).clone(),
                    ..Completions::default()
                })
            })
        }
        fn resolve(&self, _: &str, item: Completion) -> Fut<'_, Completion> {
            Box::pin(async { Ok(item) })
        }
        fn signature_help(&self, _: &str, _: Pos) -> Fut<'_, Option<Signature>> {
            Box::pin(async { Ok(None) })
        }
        fn hover(&self, _: &str, _: Pos) -> Fut<'_, Option<Hover>> {
            Box::pin(async { Ok(None) })
        }
        fn definition(&self, _: &str, _: Pos) -> Fut<'_, Vec<Location>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn symbols(&self, _: &str) -> Fut<'_, Vec<Symbol>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn references(&self, _: &str, _: Pos) -> Fut<'_, Vec<Location>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn folds(&self, _: &str) -> Fut<'_, Vec<Fold>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn is_dead(&self) -> bool {
            self.dead.load(Ordering::Relaxed)
        }
    }

    /// A ghost session that exits is swapped for the vault's next one on the next suggestion,
    /// the document opened on it as it reads now, while the primary hears every change
    /// regardless. While the vault has none to give (Ghost Text off, or given up on), the
    /// document goes without and asks again next time, so one switched on again is taken up.
    #[test]
    fn a_ghost_session_that_exits_is_started_again_without_the_primary() {
        let (primary, first, second, third) = (
            Fake::new("primary"),
            Fake::new("first"),
            Fake::new("second"),
            Fake::new("third"),
        );
        // What the vault answers each time it is asked: a fresh session, none, then another.
        let answers = Arc::new(Mutex::new(vec![
            Some(third.clone()),
            None,
            Some(second.clone()),
        ]));
        let respawn: Respawn = Box::new({
            let answers = answers.clone();
            move || {
                let next = locked(&answers).pop().flatten();
                Box::pin(async move { next.map(|g| g as Arc<dyn Language>) })
            }
        });
        let doc = Layered::new(
            Some(primary.clone()),
            Some(first.clone()),
            Some(respawn),
            Arc::new(AtomicBool::new(true)),
            None,
        );
        let at = Pos::default();
        let suggest = || accent_lsp::runtime().block_on(doc.inline_completion("a.md", at));

        assert!(doc.open("a.md", "markdown", "one".into()).unwrap().inline);
        assert_eq!(suggest().unwrap(), Some("first".into()));
        first.dead.store(true, Ordering::Relaxed);
        doc.change("a.md", "one two".into()).unwrap();
        assert_eq!(primary.heard(), ["open a.md one", "change a.md one two"]);

        assert_eq!(suggest().unwrap(), Some("second".into()));
        assert_eq!(second.heard(), ["open a.md one two"]);
        assert_eq!(suggest().unwrap(), Some("second".into()));
        assert_eq!(
            locked(&answers).len(),
            2,
            "a live session is not asked for again"
        );

        second.dead.store(true, Ordering::Relaxed);
        assert_eq!(suggest().unwrap(), None, "the vault has none");
        assert_eq!(suggest().unwrap(), Some("third".into()), "and then has one");
        assert_eq!(third.heard(), ["open a.md one two"]);
    }

    /// Without a ghost session to be had at all (merl not installed, or not prose), nothing is
    /// armed.
    #[test]
    fn no_ghost_session_arms_nothing() {
        let doc = Layered::new(None, None, None, Arc::new(AtomicBool::new(true)), None);
        assert!(!doc.open("a.txt", "text", "one".into()).unwrap().inline);
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
