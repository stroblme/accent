//! The stylesheet a note's page is drawn with: the preview's, and an exported page's on paper.
//!
//! It is generated from the editor's foreground, background, accent and font, because WebKit and a
//! browser cannot read GTK's CSS variables; so the page and the editor agree in every theme.

use crate::theme;
use accent_core::config::Config;
use gtk::{gdk, pango};

fn css_rgba(c: gdk::RGBA) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "rgba({}, {}, {}, {:.2})",
        byte(c.red()),
        byte(c.green()),
        byte(c.blue()),
        c.alpha()
    )
}

/// The page's share of its view in percent, as the editor's column takes of the editor, or
/// `None` under Full-Width Preview.
pub(crate) fn column(config: &Config) -> Option<u32> {
    (!config.full_width_preview).then_some(config.column_width)
}

/// [`theme_css`] for `fg` on `bg`, in the editor's font and the system accent.
pub(crate) fn stylesheet(fg: gdk::RGBA, bg: &str, share: Option<f64>) -> String {
    // The editor's own font, not the GNOME document font: DESIGN.md's Typography section gives
    // prose the monospace at the *size* of the document font, and the two panes are meant to agree
    // by construction. Asked of `editor::default_font`, which is where that decision is made, so a
    // change there reaches the preview without a second edit.
    let font = pango::FontDescription::from_string(&crate::editor::default_font());
    let family = font
        .family()
        .map(|f| f.to_string())
        .unwrap_or_else(crate::platform::monospace_family);
    let size = match font.size() as f64 / pango::SCALE as f64 {
        pt if pt > 0.0 => pt,
        _ => 11.0,
    };
    let accent = adw::StyleManager::default().accent_color_rgba();
    theme_css(fg, bg, accent, &family, size, share)
}

/// The sheet a paper preview is given, for an export that carries it in the file: WebKit applies
/// it as a user stylesheet, which the page's own DOM does not hold.
pub(crate) fn paper_css(config: &Config) -> String {
    let (bg, fg) = theme::paper();
    stylesheet(fg, bg, column(config).map(f64::from))
}

/// The foreground at `alpha`, as CSS: the same colour the editor dims with.
fn dim(c: gdk::RGBA, alpha: f32) -> String {
    css_rgba(crate::theme::at(c, alpha))
}

/// Generate preview styling from foreground, background, accent and document font.
/// Editor alpha and conflict tint rules are shared. Composite caption tints to opaque colours
/// so underlying side backgrounds cannot darken them. `share` is the page's cap in percent of the
/// view, `None` for no cap.
fn theme_css(
    fg: gdk::RGBA,
    bg: &str,
    accent: gdk::RGBA,
    family: &str,
    pt: f64,
    share: Option<f64>,
) -> String {
    let (text, accent) = (css_rgba(fg), css_rgba(accent));
    let (surface, quote, rule) = (dim(fg, 0.07), dim(fg, 0.6), dim(fg, 0.15));
    let page = gdk::RGBA::parse(bg).unwrap_or(gdk::RGBA::WHITE);
    let tints: String = ["current", "base", "incoming"]
        .into_iter()
        .zip(crate::conflict::tints(fg, page))
        .map(|(side, (body, head))| {
            format!(
                ".conflict-{side} {{ background: {}; }}\n\
                 .conflict-{side} > .conflict-label {{ background: {}; }}\n",
                css_rgba(body),
                css_rgba(crate::theme::over(head, page))
            )
        })
        .collect();
    let column = share.map_or_else(
        || "none".to_string(),
        |share| format!("max({}px, {share:.1}%)", crate::editor::COLUMN_FLOOR),
    );
    format!(
        "html {{ background: {bg}; color: {text}; font-family: \"{family}\", sans-serif; \
         font-size: {pt}pt; }}\n\
         /* The editor's column, its gutters inside the cap as they are inside the clamp. */\n\
         body {{ box-sizing: border-box; max-width: {column}; margin: 0 auto; \
         padding: 24px 48px 96px; line-height: 1.6; }}\n\
         /* Scroll-sync markers: the script scrolls the block around them, never the marker. */\n\
         span[data-line] {{ display: none; }}\n\
         h1, h2, h3, h4, h5, h6 {{ font-weight: 700; line-height: 1.25; margin: 1.2em 0 0.4em; }}\n\
         h1 {{ font-size: 1.6rem; }}\n\
         h2 {{ font-size: 1.4rem; }}\n\
         h3 {{ font-size: 1.2rem; }}\n\
         h4 {{ font-size: 1.1rem; }}\n\
         h5, h6 {{ font-size: 1rem; }}\n\
         p {{ margin: 0.8em 0; }}\n\
         ul, ol {{ padding-left: 1.4em; }}\n\
         /* A task item's checkbox is its bullet: it takes the marker's place in the gutter, so\n\
            its text lines up with a plain item's beside it. */\n\
         li.task-list-item {{ list-style: none; }}\n\
         li.task-list-item input {{ margin: 0 0 0 -1.4em; }}\n\
         /* `[/]`, a task in progress, has a box no checkbox state draws without a script: the\n\
            open box's size and corners, half filled in the done box's accent and faded as the\n\
            disabled boxes are. */\n\
         li.task-list-item input.in-progress {{ appearance: none; box-sizing: border-box; \
         width: 12px; height: 12px; border: 2px solid {accent}; border-radius: 3px; \
         background: linear-gradient(to right, {accent} 50%, transparent 50%); opacity: 0.5; }}\n\
         a {{ color: {accent}; text-decoration: underline; }}\n\
         /* A tag is drawn as the editor draws one: in the accent, not underlined. */\n\
         a.tag {{ text-decoration: none; }}\n\
         img {{ max-width: 100%; height: auto; }}\n\
         /* A display formula's box is its ink: unlike a line of prose it carries none of the\n\
            half-leading `line-height: 1.6` gives, so two of them would sit closer together than\n\
            two paragraphs, and two in one paragraph would touch. 1.2em is what a heading takes\n\
            above itself, and it is in `em` so it follows the document font. */\n\
         math[display=\"block\"] {{ margin: 1.2em 0; }}\n\
         /* Radicals and delimiters stretch only in a font with an OpenType MATH table, and the\n\
            runtime's `math` alias names none it ships: DejaVu is the GNOME runtime's. */\n\
         math {{ font-family: 'Latin Modern Math', 'STIX Two Math', 'DejaVu Math TeX Gyre', math; }}\n\
         code, pre, .math {{ font-family: monospace; font-size: 0.92em; }}\n\
         code {{ background: {surface}; border-radius: 4px; padding: 0.1em 0.3em; }}\n\
         pre {{ background: {surface}; border-radius: 6px; padding: 12px; overflow-x: auto; }}\n\
         pre code {{ background: none; padding: 0; }}\n\
         blockquote {{ margin: 1em 0; padding-left: 12px; border-left: 3px solid {accent}; \
         color: {quote}; font-style: italic; }}\n\
         /* A callout, of whichever type: a quote's bar over a faint tint, upright in the body\n\
            colour, its title in the accent. A flow root, so its margins stay inside the tint. */\n\
         blockquote.callout {{ display: flow-root; padding-right: 12px; border-radius: 0 6px 6px 0; \
         background: {surface}; color: {text}; font-style: normal; }}\n\
         .callout-title {{ color: {accent}; font-weight: 700; }}\n\
         hr {{ border: 0; border-top: 1px solid {rule}; margin: 24px 0; }}\n\
         table {{ border-collapse: collapse; }}\n\
         /* WebKit pads every cell with the table's cellpadding, 1px unless it says otherwise, as\n\
            a hint of the page's own: that outranks this user sheet's rule, `!important` aside. */\n\
         th, td {{ border: 1px solid {rule}; padding: 6px 12px !important; }}\n\
         th {{ font-weight: 700; }}\n\
         /* A table wider than the page scrolls sideways on its own, never the page: a block\n\
            hugging the table, which wraps its cells first, so one that fits is laid out as\n\
            before. On paper it stays a table and wraps as it can. */\n\
         @media screen {{\n\
         table {{ display: block; width: max-content; max-width: 100%; overflow-x: auto; }}\n\
         }}\n\
         /* A conflict block: its sides stacked in one rounded box, each under a caption naming\n\
            it. A side is a flow root, so its paragraphs' margins stay inside its tint. */\n\
         .conflict {{ margin: 1em 0; border-radius: 6px; overflow: auto; }}\n\
         .conflict > div {{ display: flow-root; padding: 0 12px; }}\n\
         .conflict-label {{ margin: 0 -12px; padding: 2px 12px; font-size: 0.85em; \
         font-weight: 700; }}\n\
         {tints}\
         /* On paper the page's margins are the print settings', a line of code has no sideways\n\
            scroll to hide in, and a heading or a figure is not cut from what it belongs to. A\n\
            cell's padding is tighter, so a table that has wrapped as far as it can still fits\n\
            the paper: 6px either side cut a 12-column one off A4, 4px did not. */\n\
         @media print {{\n\
         body {{ max-width: none; padding: 0; }}\n\
         pre {{ white-space: pre-wrap; overflow-wrap: anywhere; }}\n\
         h1, h2, h3, h4, h5, h6 {{ break-after: avoid; }}\n\
         pre, table, img, svg, .conflict, math[display=\"block\"] {{ break-inside: avoid; }}\n\
         th, td {{ padding: 2px 4px !important; }}\n\
         }}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FG: gdk::RGBA = gdk::RGBA::new(0.0, 0.0, 0.0, 0.8);
    const ACCENT: gdk::RGBA = gdk::RGBA::new(1.0, 0.0, 0.0, 1.0);

    /// Every `#rrggbb`-looking run in `css`.
    fn hex_literals(css: &str) -> Vec<&str> {
        css.match_indices('#')
            .map(|(i, _)| {
                let n = css[i + 1..]
                    .chars()
                    .take_while(char::is_ascii_hexdigit)
                    .count();
                &css[i..i + 1 + n]
            })
            .collect()
    }

    #[test]
    fn theme_css_paints_links_with_the_accent() {
        let css = theme_css(
            FG,
            theme::view_bg(true),
            ACCENT,
            "Cantarell",
            11.0,
            Some(50.0),
        );
        assert!(
            css.contains("a { color: rgba(255, 0, 0, 1.00); text-decoration: underline; }"),
            "{css}"
        );
        assert!(css.contains("a.tag { text-decoration: none; }"), "{css}");
        assert!(css.contains("border-left: 3px solid rgba(255, 0, 0, 1.00)"));
        assert!(
            css.contains(".callout-title { color: rgba(255, 0, 0, 1.00); font-weight: 700; }"),
            "{css}"
        );
    }

    /// A conflict block's boxes are the editor's tints of its sides, their captions the tints of
    /// the marker lines over the page, so the two panes show one block.
    #[test]
    fn theme_css_tints_a_conflict_as_the_editor_does() {
        let css = theme_css(
            FG,
            theme::view_bg(false),
            ACCENT,
            "Cantarell",
            11.0,
            Some(50.0),
        );
        let page = gdk::RGBA::parse(theme::view_bg(false)).unwrap();
        for (side, (body, head)) in ["current", "base", "incoming"]
            .into_iter()
            .zip(crate::conflict::tints(FG, page))
        {
            let rule = format!(".conflict-{side} {{ background: {}; }}", css_rgba(body));
            assert!(css.contains(&rule), "{rule} in {css}");
            let rule = format!(
                ".conflict-{side} > .conflict-label {{ background: {}; }}",
                css_rgba(theme::over(head, page))
            );
            assert!(css.contains(&rule), "{rule} in {css}");
        }
    }

    #[test]
    fn theme_css_writes_no_hex_but_the_background() {
        for bg in [theme::view_bg(false), theme::view_bg(true)] {
            let css = theme_css(FG, bg, ACCENT, "Cantarell", 11.0, Some(50.0));
            assert_eq!(hex_literals(&css), vec![bg], "stray hex with {bg}");
        }
        // Paper is white whatever the theme on screen.
        let (bg, ink) = theme::paper();
        assert_eq!(bg, theme::view_bg(false));
        let css = theme_css(ink, bg, ACCENT, "Cantarell", 11.0, Some(50.0));
        assert_eq!(hex_literals(&css), vec![bg]);
    }

    /// On screen the page takes the editor's column (`editor::column_max`), gutters included, or
    /// the whole view.
    #[test]
    fn theme_css_caps_the_page_as_the_editor_caps_its_column() {
        let css = theme_css(
            FG,
            theme::view_bg(false),
            ACCENT,
            "Cantarell",
            11.0,
            Some(55.0),
        );
        assert!(
            css.contains("body { box-sizing: border-box; max-width: max(480px, 55.0%);"),
            "{css}"
        );
        let css = theme_css(FG, theme::view_bg(false), ACCENT, "Cantarell", 11.0, None);
        assert!(
            css.contains("body { box-sizing: border-box; max-width: none;"),
            "{css}"
        );
    }

    /// A cell's padding outranks the 1 px WebKit gives every cell from the table's cellpadding.
    #[test]
    fn theme_css_pads_cells_over_webkits_cellpadding() {
        let css = theme_css(FG, theme::view_bg(false), ACCENT, "Cantarell", 11.0, None);
        assert!(css.contains("padding: 6px 12px !important; }"), "{css}");
    }

    /// A wide table scrolls on its own on screen, while on paper it is a table as any other.
    #[test]
    fn theme_css_scrolls_a_wide_table_on_its_own() {
        let css = theme_css(
            FG,
            theme::view_bg(false),
            ACCENT,
            "Cantarell",
            11.0,
            Some(50.0),
        );
        let rule =
            "table { display: block; width: max-content; max-width: 100%; overflow-x: auto; }";
        assert!(
            css.contains(&format!("@media screen {{\n{rule}\n}}")),
            "{css}"
        );
        let print = css.split_once("@media print").unwrap().1;
        assert!(!print.contains("display: block"), "{print}");
    }

    /// On paper the page is the paper's width, a long code line wraps rather than running off it,
    /// and no heading is left at the foot of a page away from what it heads.
    #[test]
    fn theme_css_lays_the_note_out_for_print() {
        let css = theme_css(
            FG,
            theme::view_bg(false),
            ACCENT,
            "Cantarell",
            11.0,
            Some(50.0),
        );
        let print = css.split_once("@media print").expect("a print block").1;
        for rule in [
            "body { max-width: none; padding: 0; }",
            "pre { white-space: pre-wrap; overflow-wrap: anywhere; }",
            "h1, h2, h3, h4, h5, h6 { break-after: avoid; }",
            "pre, table, img, svg, .conflict, math[display=\"block\"] { break-inside: avoid; }",
            "th, td { padding: 2px 4px !important; }",
        ] {
            assert!(print.contains(rule), "{rule} in {print}");
        }
    }

    #[test]
    fn theme_css_follows_its_inputs() {
        let (light, dark) = (theme::view_bg(false), theme::view_bg(true));
        let base = theme_css(FG, light, ACCENT, "Cantarell", 11.0, Some(50.0));
        let teal = gdk::RGBA::new(0.0, 0.5, 0.5, 1.0);
        assert_ne!(
            base,
            theme_css(FG, dark, ACCENT, "Cantarell", 11.0, Some(50.0))
        );
        assert_ne!(
            base,
            theme_css(FG, light, teal, "Cantarell", 11.0, Some(50.0))
        );
        assert_ne!(
            base,
            theme_css(FG, light, ACCENT, "Inter", 11.0, Some(50.0))
        );
        assert_ne!(
            base,
            theme_css(FG, light, ACCENT, "Cantarell", 13.0, Some(50.0))
        );
        assert!(theme_css(FG, light, ACCENT, "Inter", 13.0, Some(50.0)).contains("\"Inter\""));
    }
}
