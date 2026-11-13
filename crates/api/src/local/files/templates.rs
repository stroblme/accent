//! Notes made from templates, their placeholders expanded and their cursor stops found, and the
//! notes a template's `accent-target:` says it makes.

use accent_core::path::stem;
use accent_core::template;

use super::{Local, Msg, named};
use crate::paths::with_md;
use crate::{FileKind, Result, fs};

impl Local {
    /// A template's text, whether it is named by a vault-relative path or by a bare file name in
    /// the templates directory. Failing, it names both places it looked.
    fn read_template(&self, name: &str) -> Result<String> {
        // The config lock is taken before the filesystem, never inside the index lock.
        let tried = template::candidates(&self.config().templates_dir, name);
        for rel in &tried {
            let path = self.resolve(rel)?;
            if path.is_file() {
                let (text, _) = fs::read_note(&path).map_err(named(rel))?;
                return Ok(text);
            }
        }
        Err(accent_core::Error::NotFound(format!(
            "the template {name} (looked at {})",
            tried.join(" and ")
        ))
        .into())
    }

    /// A template's text as a note called `title` would get it: the body with every placeholder
    /// expanded, and the byte offsets of its `{{cursor}}` stops. What Insert Template puts at
    /// the caret, and what [`create_note`](Self::create_note) writes.
    pub fn render_template(&self, template: &str, title: &str) -> Result<(String, Vec<usize>)> {
        // Through `parse` first: `accent-target:` is accent's directive, not the note's text.
        Ok(template::render(
            &template::parse(&self.read_template(template)?).body,
            title,
            chrono::Local::now().naive_local(),
        ))
    }

    /// Create a file with an optional template, returning its path and cursor
    /// stops. Preserve the supplied name: `notes` does not become `notes.md`.
    /// Markdown callers add the extension (`note_from_template` does).
    pub fn create_note(&self, rel: &str, template: Option<&str>) -> Result<(String, Vec<usize>)> {
        let rel = rel.to_string();
        let (text, cursor) = match template {
            Some(t) => self.render_template(t, &stem(&rel))?,
            None => (String::new(), Vec::new()),
        };
        fs::create_note(&self.resolve(&rel)?, &text).map_err(named(&rel))?;
        self.post(Msg::Update {
            rel: rel.clone(),
            own: true,
        });
        Ok((rel, cursor))
    }

    /// Render a template's `accent-target:`, or `None` if absent. Expand dates
    /// as in the body; `{{title}}` uses the template name because no note exists yet.
    pub fn template_target(&self, template: &str) -> Result<Option<String>> {
        let Some(target) = template::parse(&self.read_template(template)?).target else {
            return Ok(None);
        };
        let (rel, _) =
            template::render(&target, &stem(template), chrono::Local::now().naive_local());
        Ok(Some(with_md(&rel)))
    }

    /// Create the template's target or open it unchanged if already present,
    /// so repeated daily-note requests preserve existing content. `None` means
    /// no target directive and asks the caller to supply a name.
    pub fn note_from_template(&self, template: &str) -> Result<Option<(String, Vec<usize>)>> {
        let Some(rel) = self.template_target(template)? else {
            return Ok(None);
        };
        if self.resolve(&rel)?.exists() {
            return Ok(Some((rel, Vec::new())));
        }
        self.create_note(&rel, Some(template)).map(Some)
    }

    /// The markdown files directly inside the configured templates directory.
    pub fn templates(&self) -> Result<Vec<String>> {
        // The config lock is taken before the index lock, never inside it.
        let dir = self.config().templates_dir;
        Ok(self
            .index()
            .list_files(&dir)?
            .into_iter()
            .filter(|f| f.kind == FileKind::Markdown)
            .map(|f| f.rel_path)
            .collect())
    }

    /// The templates whose `accent-target:` says where their notes go.
    pub fn template_targets(&self) -> Result<Vec<String>> {
        Ok(self
            .templates()?
            .into_iter()
            .filter(|t| matches!(self.template_target(t), Ok(Some(_))))
            .collect())
    }
}
