//! The outline, links and information dictionary `accent_pdf` reads, against what pdfium reads
//! of the same file: the reference they replaced, kept here to hold them to it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pdfium_render::prelude::*;

use super::super::*;

/// What pdfium makes of a file's outline, links and information dictionary, as accent read them
/// through pdfium-render before `accent_pdf` did (`ModDate` it never read).
struct Pdfium {
    outline: Vec<Outline>,
    links: Vec<Vec<Link>>,
    info: Info,
}

fn by_pdfium(doc: &PdfDocument<'_>) -> Pdfium {
    let mut outline = Vec::new();
    let mut stack: Vec<(PdfBookmark<'_>, usize)> = doc
        .bookmarks()
        .root()
        .map(|root| (root, 0))
        .into_iter()
        .collect();
    while let Some((node, depth)) = stack.pop() {
        if outline.len() >= 10_000 {
            break;
        }
        outline.push(Outline {
            depth,
            title: node.title().unwrap_or_default(),
            page: (node.destination())
                .and_then(|d| d.page_index().ok())
                .map(|i| i as usize),
        });
        if let Some(sibling) = node.next_sibling() {
            stack.push((sibling, depth));
        }
        if depth + 1 < 32
            && let Some(child) = node.first_child()
        {
            stack.push((child, depth + 1));
        }
    }
    let heights: Vec<f32> = doc.pages().iter().map(|p| p.height().value).collect();
    let links = (doc.pages().iter().enumerate())
        .map(|(page, p)| {
            let height = heights[page];
            let mut out = Vec::new();
            for a in p.annotations().iter() {
                if a.annotation_type() != PdfPageAnnotationType::Link {
                    continue;
                }
                let (Ok(bounds), Some(link)) = (
                    a.bounds(),
                    a.as_link_annotation().and_then(|l| l.link().ok()),
                ) else {
                    continue;
                };
                // Another file's destination: pdfium takes its page number for one of this
                // document's, `accent_pdf` makes no link of it.
                if matches!(
                    link.action(),
                    Some(PdfAction::RemoteDestination(_) | PdfAction::EmbeddedDestination(_))
                ) {
                    continue;
                }
                let target = if let Some(dest) = link.destination() {
                    let Ok(target) = dest.page_index() else {
                        continue;
                    };
                    let target = target as usize;
                    let top = match dest.view_settings() {
                        Ok(PdfDestinationViewSettings::SpecificCoordinatesAndZoom(_, y, _))
                        | Ok(PdfDestinationViewSettings::FitPageHorizontallyToWindow(y)) => {
                            y.map(|y| y.value)
                        }
                        Ok(PdfDestinationViewSettings::FitPageToRectangle(r)) => {
                            Some(r.top().value)
                        }
                        _ => None,
                    };
                    let target_height = heights.get(target).copied().unwrap_or(height);
                    let top = top.map(|y| target_height - y);
                    LinkTarget::Page { page: target, top }
                } else if let Some(uri) =
                    (link.action()).and_then(|a| a.as_uri_action().and_then(|u| u.uri().ok()))
                {
                    LinkTarget::Uri(uri)
                } else {
                    continue;
                };
                out.push(Link {
                    rect: Rect::from_pdf(bounds, height),
                    target,
                });
            }
            out
        })
        .collect();
    use PdfDocumentMetadataTagType as Tag;
    let tag = |kind| {
        let value = doc.metadata().get(kind)?;
        let value = value.value().trim();
        (!value.is_empty()).then(|| value.to_string())
    };
    let version = match doc.version() {
        PdfDocumentVersion::Pdf1_0 => Some("1.0"),
        PdfDocumentVersion::Pdf1_1 => Some("1.1"),
        PdfDocumentVersion::Pdf1_2 => Some("1.2"),
        PdfDocumentVersion::Pdf1_3 => Some("1.3"),
        PdfDocumentVersion::Pdf1_4 => Some("1.4"),
        PdfDocumentVersion::Pdf1_5 => Some("1.5"),
        PdfDocumentVersion::Pdf1_6 => Some("1.6"),
        PdfDocumentVersion::Pdf1_7 => Some("1.7"),
        PdfDocumentVersion::Pdf2_0 => Some("2.0"),
        PdfDocumentVersion::Unset | PdfDocumentVersion::Other(_) => None,
    };
    let info = Info {
        title: tag(Tag::Title),
        author: tag(Tag::Author),
        subject: tag(Tag::Subject),
        keywords: tag(Tag::Keywords),
        creator: tag(Tag::Creator),
        producer: tag(Tag::Producer),
        created: tag(Tag::CreationDate).as_deref().and_then(parse_date),
        modified: None,
        version: version.map(str::to_string),
    };
    Pdfium {
        outline,
        links,
        info,
    }
}

/// pdfium's reading of `bytes`, which it repairs where they are damaged.
fn pdfium_reads(bytes: &[u8]) -> Option<Pdfium> {
    let _guard = lock();
    let doc = pdfium().ok()?.load_pdf_from_byte_slice(bytes, None).ok()?;
    Some(by_pdfium(&doc))
}

/// The outline as the host reads it: the file alone, through `accent_pdf::read`.
fn host_outline(bytes: Vec<u8>) -> accent_pdf::Result<Vec<Outline>> {
    accent_pdf::read(Arc::new(bytes), |pdf| {
        let doc = accent_pdf::Document::new(pdf, None, &pdf.pages()?);
        let outline = doc.outline()?.into_iter();
        Ok(outline
            .map(|b| Outline {
                depth: b.depth,
                title: b.title,
                page: b.page,
            })
            .collect())
    })
}

/// How the window's reading of a document differs from pdfium's, one line a difference, and how
/// many links it found that pdfium missed: a destination named in UTF-8, which pdfium decodes as
/// PDFDocEncoding before looking it up and so misses in a name tree sorted by its bytes.
fn differences(doc: &PdfDoc, pdfium: &Pdfium) -> (Vec<String>, usize) {
    let (mut out, mut more) = (Vec::new(), 0);
    match doc.outline() {
        Ok(outline) if outline == pdfium.outline => {}
        outline => out.push(format!("outline {outline:?} pdfium {:?}", pdfium.outline)),
    }
    let info = Info {
        modified: None,
        ..doc.info()
    };
    if info != pdfium.info {
        let fields = |i: &Info| {
            let Info {
                title,
                author,
                subject,
                keywords,
                creator,
                producer,
                created,
                version,
                ..
            } = i.clone();
            [
                title,
                author,
                subject,
                keywords,
                creator,
                producer,
                created.map(|c| c.to_string()),
                version,
            ]
        };
        let names = [
            "title", "author", "subject", "keywords", "creator", "producer", "created", "version",
        ];
        for ((name, a), b) in names.iter().zip(fields(&info)).zip(fields(&pdfium.info)) {
            if a != b {
                out.push(format!("info {name} {a:?} pdfium {b:?}"));
            }
        }
    }
    let near = |a: f32, b: f32| (a - b).abs() < 0.01;
    let same = |a: &Link, b: &Link| {
        let (r, s) = (a.rect, b.rect);
        let rect = near(r.left, s.left) && near(r.top, s.top);
        let rect = rect && near(r.right, s.right) && near(r.bottom, s.bottom);
        let target = match (&a.target, &b.target) {
            (LinkTarget::Page { page: p, top: t }, LinkTarget::Page { page: q, top: u }) => {
                p == q
                    && t.zip(*u)
                        .map_or(t.is_none() && u.is_none(), |(t, u)| near(t, u))
            }
            (a, b) => a == b,
        };
        rect && target
    };
    let pages = |links: &[Link]| {
        (links.iter())
            .filter(|l| matches!(l.target, LinkTarget::Page { .. }))
            .count()
    };
    for (page, theirs) in pdfium.links.iter().enumerate() {
        match doc.links(page) {
            Ok(ours)
                if ours.len() == theirs.len()
                    && ours.iter().zip(theirs).all(|(a, b)| same(a, b)) => {}
            // pdfium's every one among ours, in order, and only page links more.
            Ok(ours)
                if {
                    let mut rest = ours.iter();
                    let kept = theirs.iter().all(|b| rest.any(|a| same(a, b)));
                    kept && ours.len() - theirs.len() == pages(&ours) - pages(theirs)
                } =>
            {
                more += ours.len() - theirs.len()
            }
            Ok(ours) => {
                let at = (ours.iter().zip(theirs)).position(|(a, b)| !same(a, b));
                let at = at.unwrap_or(ours.len().min(theirs.len()));
                out.push(format!(
                    "page {page} links {} pdfium {}, first apart at {at}: {:?} pdfium {:?}",
                    ours.len(),
                    theirs.len(),
                    ours.get(at),
                    theirs.get(at)
                ));
            }
            Err(e) => out.push(format!("page {page} links {e:#}")),
        }
    }
    (out, more)
}

/// The tiny fixture's outline and links read as pdfium reads them, and damaged three ways: its
/// offsets shifted, its end cut off, and its xref rows garbled. pdfium repairs each; the window,
/// which takes pdfium's repair (D5), and the host, which rebuilds the xref itself, read the same
/// outline from it.
#[test]
fn a_damaged_file_reads_as_pdfium_repairs_it() {
    if !available() {
        return eprintln!("skipping: no libpdfium");
    }
    let whole = super::tiny_pdf(false);
    let xref = whole.windows(4).rposition(|w| w == b"xref").unwrap();
    let shifted = [&whole[..9], b"%junk\n".as_slice(), &whole[9..]].concat();
    let cut = whole[..xref].to_vec();
    let mut garbled = whole.clone();
    for at in (xref + 30..garbled.len()).step_by(23).take(8) {
        garbled[at] = b'7';
    }
    let reference = pdfium_reads(&whole).unwrap();
    assert_eq!(reference.outline.len(), 2);
    for (what, bytes) in [
        ("whole", whole),
        ("shifted", shifted),
        ("cut", cut),
        ("garbled", garbled),
    ] {
        let pdfium = pdfium_reads(&bytes).expect("pdfium repairs it");
        assert_eq!(pdfium.outline, reference.outline, "{what}: pdfium's repair");
        let doc = super::doc_of(bytes.clone()).unwrap();
        assert_eq!(
            differences(&doc, &pdfium),
            (Vec::new(), 0),
            "{what}: the window"
        );
        assert_eq!(
            host_outline(bytes).unwrap(),
            pdfium.outline,
            "{what}: the host"
        );
    }
}

/// The corpus run, by hand, as `the_corpus_reads_in_accent_pdf_as_pdfium_reads_it`: every PDF
/// pdfium opens under `ACCENT_PDF_CORPUS` (never the user's vault), its outline, links and
/// information as the window reads them and its outline as the host does, against pdfium's.
#[test]
#[ignore = "ACCENT_PDF_CORPUS=<folder> cargo test -p accent-core --features pdf -- --ignored outlines"]
fn the_corpus_reads_its_outlines_links_and_information_as_pdfium_does() {
    fn pdfs(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let (path, kind) = (e.path(), e.file_type().unwrap());
            match (kind.is_dir(), path.extension().is_some_and(|x| x == "pdf")) {
                (true, _) => pdfs(&path, out),
                (false, true) if kind.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    let root = PathBuf::from(std::env::var_os("ACCENT_PDF_CORPUS").expect("ACCENT_PDF_CORPUS"));
    assert!(
        !root.to_string_lossy().contains("Sync/Notes"),
        "never the real vault"
    );
    if !available() {
        return eprintln!("skipping: no libpdfium");
    }
    let mut found = Vec::new();
    pdfs(&root, &mut found);
    let (mut read, mut marks, mut links, mut more, mut differ) = (0, 0, 0, 0, Vec::new());
    for path in &found {
        let bytes = std::fs::read(path).unwrap();
        let Some(pdfium) = pdfium_reads(&bytes) else {
            continue;
        };
        read += 1;
        marks += pdfium.outline.len();
        links += pdfium.links.iter().map(Vec::len).sum::<usize>();
        let mut lines = match PdfDoc::open(path) {
            Ok(doc) => {
                let (lines, found) = differences(&doc, &pdfium);
                more += found;
                lines
            }
            Err(e) => vec![format!("open {e:#}")],
        };
        match host_outline(bytes) {
            Ok(outline) if outline == pdfium.outline => {}
            outline => lines.push(format!("host outline {outline:?}")),
        }
        differ.extend(
            lines
                .into_iter()
                .map(|l| format!("{}: {l}", path.display())),
        );
    }
    for line in &differ {
        let line: String = line.chars().take(600).collect();
        eprintln!("{line}");
    }
    eprintln!(
        "{read} PDFs pdfium reads, {marks} bookmarks, {links} links: {} differences, {more} links \
         pdfium misses",
        differ.len()
    );
    assert!(differ.is_empty());
}
