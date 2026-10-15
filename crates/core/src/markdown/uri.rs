//! The preview's own addresses read back. [`to_html`](super::to_html) writes `accent://open/…`
//! for a wikilink, `accent://tag/…` for a tag and `accent://file/…` for an embed, and a page
//! loaded at [`base_uri`] resolves an ordinary relative link to `accent://file/…` too. Both apps
//! route a click on a rendered note by [`click`], so a link goes the same way on either.

use super::{is_url, percent_decode, percent_encode};
use crate::path::parent_dir;

/// What a click on a link in a rendered note asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Click {
    /// A vault file or a wikilink target, with the `#anchor` that names a heading or a PDF
    /// passage decoded and joined back on.
    Open(String),
    /// A tag, whose notes are listed.
    Tag(String),
    /// A place on the page in front, which the view scrolls to itself.
    Scroll,
    /// An address the system opens: the web, mail, a phone number ([`is_url`]).
    Leave,
    /// Anything else, which goes nowhere.
    Nowhere,
}

/// Where the link `uri` goes from the page loaded at `page`. A view's own load of the page, and
/// whether a person clicked at all, are the view's to tell apart.
pub fn click(uri: &str, page: &str) -> Click {
    match accent_uri(uri) {
        Some(("tag", tag)) => Click::Tag(tag),
        _ if same_page(uri, page) => Click::Scroll,
        // The anchor is kept for a PDF, which needs the page as well as the file, and for a
        // note, whose heading it names. Rejoined here rather than kept by `accent_uri`, since
        // an image is fetched by path and a `#` in one is not a place.
        Some(("open" | "file", target)) => match uri.split_once('#') {
            Some((_, anchor)) if !anchor.is_empty() => {
                Click::Open(format!("{target}#{}", percent_decode(anchor)))
            }
            _ => Click::Open(target),
        },
        _ if is_url(uri) => Click::Leave,
        _ => Click::Nowhere,
    }
}

/// The vault path an `accent://file/` request names, decoded, without its `?query` or
/// `#fragment`; `None` for any other address.
pub fn accent_file(uri: &str) -> Option<String> {
    match accent_uri(uri)? {
        ("file", rel) => Some(rel),
        _ => None,
    }
}

/// Where relative links in `rel`'s markdown resolve from: the note's own directory.
pub fn base_uri(rel: &str) -> String {
    match parent_dir(rel) {
        "" => "accent://file/".to_string(),
        dir => format!("accent://file/{}/", percent_encode(dir)),
    }
}

/// Split `accent://<host>/<path>` into host and percent-decoded path, dropping `?query` and
/// `#fragment`. `None` for anything that is not an `accent:` URI.
fn accent_uri(uri: &str) -> Option<(&str, String)> {
    let rest = uri.strip_prefix("accent://")?;
    let head = rest.split_once(['?', '#']).map_or(rest, |(h, _)| h);
    let (host, path) = head.split_once('/').unwrap_or((head, ""));
    Some((host, percent_decode(path)))
}

/// Whether `uri` is a place on the page `current` shows: the same document with a `#fragment`.
/// `current` may carry a fragment of its own, from the last anchor followed.
fn same_page(uri: &str, current: &str) -> bool {
    let page = current.split_once('#').map_or(current, |(doc, _)| doc);
    uri.split_once('#').is_some_and(|(doc, _)| doc == page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_uri_decodes_the_target_and_drops_the_anchor() {
        assert_eq!(
            accent_uri("accent://open/Deep%20Work#Section%20One"),
            Some(("open", "Deep Work".to_string()))
        );
        assert_eq!(
            accent_uri("accent://open/Notes-PHD/Deep%20Work.md"),
            Some(("open", "Notes-PHD/Deep Work.md".to_string()))
        );
        assert_eq!(accent_uri("https://example.com/x"), None);
    }

    /// An image is fetched by the path it names: a `+` is a plus, not a space, and a malformed
    /// escape stays as written rather than eating the next character.
    #[test]
    fn an_image_request_names_its_file() {
        assert_eq!(
            accent_file("accent://file/fig+1.png").as_deref(),
            Some("fig+1.png")
        );
        assert_eq!(
            accent_file("accent://file/img%20a.png?v=1").as_deref(),
            Some("img a.png")
        );
        assert_eq!(
            accent_file("accent://file/100%25%zz").as_deref(),
            Some("100%%zz")
        );
        assert_eq!(accent_file("accent://open/Note"), None);
    }

    /// An ordinary link arrives resolved against the note's folder, a wikilink by name; both
    /// keep the anchor whole, a PDF passage's `&` and a heading's `+` included.
    #[test]
    fn a_link_opens_its_target_with_its_anchor() {
        let page = "accent://file/Notes/";
        let open = |uri: &str| click(uri, page);
        assert_eq!(
            open("accent://file/Notes/Other.md"),
            Click::Open("Notes/Other.md".into())
        );
        assert_eq!(
            open("accent://open/Other%20Note#Head%20One"),
            Click::Open("Other Note#Head One".into())
        );
        assert_eq!(
            open("accent://open/paper.pdf#page=3&selection=4,0,4,11"),
            Click::Open("paper.pdf#page=3&selection=4,0,4,11".into())
        );
        assert_eq!(
            open("accent://open/Note#C++"),
            Click::Open("Note#C++".into())
        );
        assert_eq!(open("accent://tag/c%2B%2B"), Click::Tag("c++".into()));
    }

    #[test]
    fn an_anchor_into_the_page_on_screen_stays_on_it() {
        let page = "accent://file/Notes/";
        assert_eq!(click("accent://file/Notes/#intro", page), Click::Scroll);
        // After the first jump the view's own URI carries the fragment it scrolled to.
        assert!(same_page(
            "accent://file/Notes/#outro",
            "accent://file/Notes/#intro"
        ));
        assert!(!same_page("accent://file/Notes/Other.md#intro", page));
        // No fragment is a load of the directory, not a place on the page.
        assert!(!same_page(page, page));
    }

    /// A click hands the system what Go to Definition would, and nothing the page could run or
    /// read the disk with.
    #[test]
    fn a_click_leaves_for_what_the_editor_would_and_nothing_else() {
        for url in ["https://e.org", "mailto:a@b.c", "tel:+123", "sms:+123"] {
            assert_eq!(click(url, ""), Click::Leave, "{url}");
        }
        for not in [
            "javascript:alert(1)",
            "javascript://%0aalert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
        ] {
            assert_eq!(click(not, ""), Click::Nowhere, "{not}");
        }
    }

    #[test]
    fn base_uri_is_the_notes_own_directory() {
        assert_eq!(base_uri("Index.md"), "accent://file/");
        assert_eq!(base_uri("Notes/Deep Work.md"), "accent://file/Notes/");
        assert_eq!(base_uri("a b/c d/Note.md"), "accent://file/a%20b/c%20d/");
        // A `#` in a folder's name is part of the path, not where the page's fragment begins.
        assert_eq!(base_uri("C#/Note.md"), "accent://file/C%23/");
    }
}
