//! The incremental writer held to pdfium, on encrypted and broken files too, and the corpus and
//! spike runs by hand.

use super::*;

/// `base` with a red line across its first page, from a tenth of the way across and a fifth up its
/// visible box to as far from the other corner, as an `/Ink` with an appearance stream of its
/// own, appended through `accent-pdf`: the shape the pen's strokes are to take once it writes them
/// itself.
fn with_appended_ink(base: &[u8]) -> accent_pdf::Result<Vec<u8>> {
    use accent_pdf::{Content, Dict, Object, Update, form};
    let pdf = accent_pdf::Pdf::open(std::sync::Arc::new(base.to_vec()))?;
    let Some(&page) = pdf.pages()?.first() else {
        return Err(accent_pdf::Error::Broken("no page".into()));
    };
    let visible = match pdf.inherited(page, b"CropBox")? {
        Some(crop) => Some(crop),
        None => pdf.inherited(page, b"MediaBox")?,
    };
    let visible = match visible {
        Some(Object::Ref(r)) => Some(pdf.get(r)?),
        visible => visible,
    };
    let edges: Vec<f32> = (visible
        .as_ref()
        .and_then(Object::as_array)
        .unwrap_or_default())
    .iter()
    .map(|v| match v {
        Object::Int(i) => *i as f32,
        Object::Real(r) => *r as f32,
        _ => 0.0,
    })
    .collect();
    let [x0, y0, x1, y1] = edges[..] else {
        return Err(accent_pdf::Error::Broken("no page box".into()));
    };
    let (w, h) = (x1 - x0, y1 - y0);
    let (from, to) = ((x0 + w * 0.1, y0 + h * 0.2), (x0 + w * 0.9, y0 + h * 0.8));
    let width = (w.abs().min(h.abs()) / 10.0).clamp(1.0, 4.0);
    let real = |v: f32| Object::Real(f64::from(v));
    let rect = [from.0 - width, from.1 - width, to.0 + width, to.1 + width];
    let mut update = Update::new(&pdf);
    let mut drawing = Content::default();
    drawing
        .stroke_rgb([1.0, 0.0, 0.0])
        .width(width)
        .cap(1)
        .join(1);
    drawing.move_to(from).line_to(to).stroke();
    let appearance = update.add(Object::Stream(form(
        rect,
        drawing.finish(),
        Dict::default(),
    )));
    let name = |n: &[u8]| Object::Name(n.to_vec());
    let ink = update.add(Object::Dict(Dict(vec![
        (b"Type".to_vec(), name(b"Annot")),
        (b"Subtype".to_vec(), name(b"Ink")),
        (b"Rect".to_vec(), Object::Array(rect.map(real).to_vec())),
        (
            b"InkList".to_vec(),
            Object::Array(vec![Object::Array(
                [from.0, from.1, to.0, to.1].map(real).to_vec(),
            )]),
        ),
        (
            b"C".to_vec(),
            Object::Array([1.0, 0.0, 0.0].map(real).to_vec()),
        ),
        (
            b"BS".to_vec(),
            Object::Dict(Dict(vec![(b"W".to_vec(), real(width))])),
        ),
        (b"F".to_vec(), Object::Int(4)),
        (b"P".to_vec(), Object::Ref(page)),
        (
            b"AP".to_vec(),
            Object::Dict(Dict(vec![(b"N".to_vec(), Object::Ref(appearance))])),
        ),
    ])));
    let Some(Object::Dict(mut dict)) = update.get(&pdf, page)?.as_dict().cloned().map(Object::Dict)
    else {
        return Err(accent_pdf::Error::Broken(
            "a page that is no dictionary".into(),
        ));
    };
    match dict.get(b"Annots").cloned() {
        Some(Object::Ref(list)) => {
            let mut annots = update
                .get(&pdf, list)?
                .as_array()
                .unwrap_or_default()
                .to_vec();
            annots.push(Object::Ref(ink));
            update.set(list, Object::Array(annots));
        }
        Some(Object::Array(mut annots)) => {
            annots.push(Object::Ref(ink));
            dict.set(b"Annots", Object::Array(annots));
        }
        _ => dict.set(b"Annots", Object::Array(vec![Object::Ref(ink)])),
    }
    update.set(page, Object::Dict(dict));
    Ok([base, &update.section()].concat())
}

/// The red pixels of the first page as pdfium draws it.
fn red_pixels(bytes: Vec<u8>) -> Result<usize> {
    let img = PdfDoc::from_bytes(bytes)?.render_page(0, 1.0, Theme::Plain)?;
    let pixels = img.data.as_chunks::<4>().0;
    Ok(pixels
        .iter()
        .filter(|px| px[0] > 200 && px[1] < 100 && px[2] < 100)
        .count())
}

#[test]
fn pdfium_draws_an_ink_appended_through_accent_pdf() {
    if !available() {
        return eprintln!("skipping: no libpdfium");
    }
    let base = tiny_pdf(false);
    let appended = with_appended_ink(&base).unwrap();
    assert!(
        appended.starts_with(&base),
        "the file before the update is left as it was"
    );
    assert_eq!(red_pixels(base).unwrap(), 0);
    assert!(red_pixels(appended).unwrap() > 300, "the line is drawn");
}

/// Encrypted files under the empty user password: ghostscript's RC4 (revisions 2 and 3, see
/// `accent-pdf`'s tests) and AES-128 and AES-256 as `accent-pdf` builds them (revisions 4 and 6).
fn encrypted() -> Vec<Vec<u8>> {
    let mut all = vec![
        include_bytes!("../../../../pdf/fixtures/gs-rc4-40.pdf").to_vec(),
        include_bytes!("../../../../pdf/fixtures/gs-rc4-128-table.pdf").to_vec(),
    ];
    all.extend([4, 6].map(accent_pdf::fixtures::encrypted_document));
    all
}

/// pdfium opens each encrypted file, and draws an `/Ink` appended to it, which `accent-pdf`
/// encrypts as the file is.
#[test]
fn pdfium_draws_an_ink_appended_to_an_encrypted_file() {
    if !available() {
        return eprintln!("skipping: no libpdfium");
    }
    for (i, base) in encrypted().into_iter().enumerate() {
        let appended = with_appended_ink(&base).unwrap();
        let before = red_pixels(base).unwrap();
        assert!(red_pixels(appended).unwrap() > before + 300, "file {i}");
    }
}

/// What pdfium writes into an encrypted file stays encrypted, and `accent-pdf` reads it.
#[test]
fn accent_pdf_reads_what_pdfium_saves_encrypted() {
    if !available() {
        return eprintln!("skipping: no libpdfium");
    }
    let red = InkStyle {
        width: 2.0,
        rgba: [255, 0, 0, 255],
        multiply: false,
    };
    for (i, base) in encrypted().into_iter().enumerate() {
        let mut doc = PdfDoc::from_bytes(base).unwrap();
        doc.add_ink(0, &[(20.0, 20.0), (60.0, 40.0)], red, 1.0)
            .unwrap();
        let saved = doc.save().unwrap();
        assert!(
            memchr::memmem::find(&saved, b"/Encrypt").is_some(),
            "file {i}"
        );
        let pdf = accent_pdf::Pdf::open(std::sync::Arc::new(saved)).unwrap();
        let page = pdf.get(pdf.pages().unwrap()[0]).unwrap();
        let annots = page.as_dict().and_then(|p| p.get(b"Annots")).cloned();
        assert!(annots.is_some(), "file {i}: the stroke's page lists it");
    }
}

/// A save is the file as it was read with one section after it, however often the session saves:
/// the second save's file still starts with the original bytes and holds one update, not two.
#[test]
fn a_save_appends_one_section_and_keeps_the_original_bytes() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    let path = dir.path().join("tiny.pdf");
    let original = std::fs::read(&path).unwrap();
    let read = crate::fs::Etag::of(&path).unwrap();
    let pen = InkStyle {
        width: 2.0,
        rgba: [255, 0, 0, 255],
        multiply: false,
    };
    let updates = |file: &[u8]| memchr::memmem::find_iter(file, b"%%EOF").count();

    doc.add_ink(0, &[(20.0, 20.0), (60.0, 40.0)], pen, 1.0)
        .unwrap();
    let saved = doc.save_to(&path, Some(read)).unwrap();
    doc.add_ink(0, &[(20.0, 60.0), (60.0, 80.0)], pen, 1.0)
        .unwrap();
    doc.save_to(&path, Some(saved)).unwrap();
    let twice = std::fs::read(&path).unwrap();
    assert!(twice.starts_with(&original), "the file as it was read");
    assert_eq!(updates(&twice), updates(&original) + 1, "one section");
    assert_eq!(PdfDoc::open(&path).unwrap().inks(0).unwrap().len(), 2);
    // A save over a file that changed since is refused, as every save is.
    assert!(matches!(
        doc.save_to(&path, Some(saved)),
        Err(Error::ChangedOnDisk { .. })
    ));
}

/// A file whose xref does not lead where it says is read through pdfium's repair once: the first
/// save writes it whole, and a session on the saved file appends to it as to any other.
#[test]
fn a_broken_file_is_normalised_once() {
    if !available() {
        return eprintln!("skipping: no libpdfium");
    }
    // `startxref` ten bytes short of the table, which pdfium finds by scanning the file.
    let good = String::from_utf8(tiny_pdf(false)).unwrap();
    let (head, xref) = good.rsplit_once("startxref\n").unwrap();
    let at: usize = xref.lines().next().unwrap().parse().unwrap();
    let broken = format!("{head}startxref\n{}\n%%EOF\n", at - 10).into_bytes();
    assert!(matches!(
        accent_pdf::Pdf::open(std::sync::Arc::new(broken.clone())),
        Err(accent_pdf::Error::Broken(_))
    ));
    let pen = InkStyle {
        width: 2.0,
        rgba: [255, 0, 0, 255],
        multiply: false,
    };
    let (dir, mut doc) = open_pdf(&broken).unwrap();
    let path = dir.path().join("tiny.pdf");
    assert_eq!(page_texts(&doc), ["Hello accent", "Second page"]);
    doc.add_ink(0, &[(20.0, 20.0), (60.0, 40.0)], pen, 1.0)
        .unwrap();
    doc.save_to(&path, None).unwrap();
    let whole = std::fs::read(&path).unwrap();
    assert!(!whole.starts_with(&broken), "written whole");
    accent_pdf::Pdf::open(std::sync::Arc::new(whole.clone())).unwrap();

    let mut doc = PdfDoc::open(&path).unwrap();
    doc.add_ink(1, &[(20.0, 20.0), (60.0, 40.0)], pen, 1.0)
        .unwrap();
    doc.save_to(&path, None).unwrap();
    let again = PdfDoc::open(&path).unwrap();
    assert!(
        std::fs::read(&path).unwrap().starts_with(&whole),
        "appended"
    );
    assert_eq!(
        (again.inks(0).unwrap().len(), again.inks(1).unwrap().len()),
        (1, 1)
    );
}

/// Rendering must not change saved annotations: what pdfium generates as it draws — appearance
/// streams, `PDFIUM_HasGeneratedAP`, popups — stays out of the file, which pdfium never writes.
#[test]
fn renders_leave_nothing_in_a_save() {
    let Some((dir, doc)) = open_tiny_with(true) else {
        return;
    };
    let unrendered = doc.save().unwrap();
    // Inside the highlight, above the line of text it covers.
    let yellow = |page: &RgbaImage| {
        let at = (38 * 2 * page.width as usize + 100 * 2) * 4;
        let px = &page.data[at..at + 3];
        px[0] > 200 && px[1] > 200 && px[2] < 100
    };
    assert!(yellow(&doc.render_page(0, 2.0, Theme::Plain).unwrap()));
    for _ in 0..10 {
        doc.render_tile(0, 2.0, 0, 0, 100, 100, Theme::Plain)
            .unwrap();
    }
    assert!(yellow(&doc.render_page(0, 2.0, Theme::Plain).unwrap()));
    let saved = doc.save().unwrap();
    assert_eq!(object(&saved, 6), object(&unrendered, 6));
    assert_eq!(saved.len(), unrendered.len());
    let text = String::from_utf8_lossy(&saved).into_owned();
    assert!(!text.contains("PDFIUM_HasGeneratedAP"), "{text}");
    let reopened = reopen(&dir, &doc);
    assert!(yellow(&reopened.render_page(0, 2.0, Theme::Plain).unwrap()));
}

/// The corpus run, by hand: every PDF under `ACCENT_PDF_CORPUS` (never the user's vault) read by
/// `accent-pdf`, against the pages pdfium counts, and each one that agrees given a red `/Ink`
/// through `accent-pdf`'s update, which pdfium must draw and `accent-pdf` read back. Every PDF
/// pdfium opens opens as a document too, a damaged one repaired, and takes a stroke that a save
/// carries. A file that
/// needs a password is counted, and a broken one listed for its repair through pdfium; any other
/// disagreement on a file pdfium opens fails.
#[test]
#[ignore = "ACCENT_PDF_CORPUS=<folder> cargo test -p accent-core --features pdf -- --ignored corpus"]
fn the_corpus_reads_in_accent_pdf_as_pdfium_reads_it() {
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
    let (mut agree, mut locked, mut unread) = (0, 0, 0);
    let (mut broken, mut wrong) = (Vec::new(), Vec::new());
    let red = InkStyle {
        width: 2.0,
        rgba: [255, 0, 0, 255],
        multiply: false,
    };
    for path in &found {
        let pdfium = {
            let _guard = lock();
            let doc = pdfium().unwrap().load_pdf_from_file(path, None);
            doc.map(|doc| doc.pages().len() as usize)
        };
        let Ok(pdfium) = pdfium else {
            unread += 1;
            continue;
        };
        let session = (|| -> Result<bool> {
            let mut doc = PdfDoc::open(path)?;
            let (had, (w, h)) = (doc.inks(0)?.len(), doc.page_size(0)?);
            doc.add_ink(0, &[(w * 0.1, h * 0.2), (w * 0.9, h * 0.8)], red, 1.0)?;
            let saved = PdfDoc::from_bytes(doc.save()?)?;
            Ok(saved.page_count() == pdfium && saved.inks(0)?.len() == had + 1)
        })();
        if !matches!(session, Ok(true)) {
            wrong.push(format!("{}: session {session:?}", path.display()));
        }
        let file = std::fs::File::open(path).unwrap();
        match accent_pdf::Pdf::open(std::sync::Arc::new(file)).and_then(|pdf| pdf.pages()) {
            Ok(pages) if pages.len() == pdfium => {
                let base = std::fs::read(path).unwrap();
                let red = |bytes| red_pixels(bytes).map_err(|e| e.to_string());
                let ours = |bytes: &[u8]| {
                    let pdf = accent_pdf::Pdf::open(std::sync::Arc::new(bytes.to_vec()));
                    pdf.and_then(|pdf| pdf.pages())
                        .map(|pages| pages.len())
                        .ok()
                };
                let drawn = with_appended_ink(&base)
                    .map_err(|e| e.to_string())
                    .and_then(|appended| Ok((ours(&appended), red(base)?, red(appended)?)));
                match drawn {
                    Ok((reread, before, after)) if after > before && reread == Some(pdfium) => {
                        agree += 1
                    }
                    drawn => wrong.push(format!("{}: ink {drawn:?}", path.display())),
                }
            }
            Err(accent_pdf::Error::Unsupported(what)) if what.contains("password") => locked += 1,
            Err(accent_pdf::Error::Broken(why)) => {
                broken.push(format!("{}: {why}", path.display()))
            }
            ours => wrong.push(format!(
                "{}: pdfium {pdfium}, ours {ours:?}",
                path.display()
            )),
        }
    }
    eprintln!(
        "{} PDFs: {agree} agree, {locked} need a password, {unread} pdfium cannot open, {} broken, \
         {} differ",
        found.len(),
        broken.len(),
        wrong.len()
    );
    for w in broken.iter().chain(&wrong) {
        eprintln!("  {w}");
    }
    assert!(wrong.is_empty());
}

/// What a session costs, by hand, on `ACCENT_PDF_SPIKE` (one PDF, copied into `TMPDIR` first):
/// the open and its parts, measured again warm, a stroke (written into the update and pdfium
/// opened again), a reopen, the first tile after one against the same tile warm, a save, and the
/// section it appends. Medians and p90 over 15 runs; run it in release.
#[test]
#[ignore = "ACCENT_PDF_SPIKE=<file.pdf> cargo test --release -p accent-core --features pdf -- --ignored spike --nocapture"]
fn the_spike_times_a_session() {
    let from = PathBuf::from(std::env::var_os("ACCENT_PDF_SPIKE").expect("ACCENT_PDF_SPIKE"));
    assert!(
        !from.to_string_lossy().contains("Sync/Notes"),
        "never the real vault"
    );
    if !available() {
        return eprintln!("skipping: no libpdfium");
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spike.pdf");
    let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
    let mut times: std::collections::BTreeMap<&str, Vec<f64>> = Default::default();
    let mut section = 0;
    let pen = InkStyle {
        width: 2.0,
        rgba: [255, 0, 0, 255],
        multiply: false,
    };
    for _ in 0..15 {
        std::fs::copy(&from, &path).unwrap();
        let read = crate::fs::Etag::of(&path).unwrap();
        let t = std::time::Instant::now();
        let mut doc = PdfDoc::open(&path).unwrap();
        times.entry("1 open").or_default().push(ms(t));
        let t = std::time::Instant::now();
        let file = std::fs::File::open(&path).unwrap();
        let pdf = accent_pdf::Pdf::open(std::sync::Arc::new(file)).unwrap();
        times.entry("2 open's xref").or_default().push(ms(t));
        let t = std::time::Instant::now();
        pdf.pages().unwrap();
        times.entry("2 open's page tree").or_default().push(ms(t));
        let t = std::time::Instant::now();
        let file = std::fs::File::open(&path).unwrap();
        let guard = lock();
        drop(pdfium().unwrap().load_pdf_from_reader(file, None).unwrap());
        drop(guard);
        times.entry("2 open's pdfium").or_default().push(ms(t));
        let t = std::time::Instant::now();
        doc.page_sizes().unwrap();
        times.entry("3 page sizes").or_default().push(ms(t));
        doc.render_tile(0, 2.0, 0, 0, 512, 512, Theme::Plain)
            .unwrap();
        let t = std::time::Instant::now();
        doc.render_tile(0, 2.0, 0, 0, 512, 512, Theme::Plain)
            .unwrap();
        times.entry("4 tile warm").or_default().push(ms(t));
        let t = std::time::Instant::now();
        doc.add_ink(0, &[(40.0, 40.0), (200.0, 120.0)], pen, 1.0)
            .unwrap();
        times.entry("5 stroke and reopen").or_default().push(ms(t));
        let t = std::time::Instant::now();
        doc.render_tile(0, 2.0, 0, 0, 512, 512, Theme::Plain)
            .unwrap();
        times.entry("6 tile after reopen").or_default().push(ms(t));
        let t = std::time::Instant::now();
        doc.reopen().unwrap();
        times.entry("7 reopen").or_default().push(ms(t));
        let t = std::time::Instant::now();
        doc.save_to(&path, Some(read)).unwrap();
        times.entry("8 save").or_default().push(ms(t));
        section = std::fs::metadata(&path).unwrap().len() - std::fs::metadata(&from).unwrap().len();
    }
    eprintln!("{}: section {section} B", from.display());
    for (what, mut all) in times {
        all.sort_by(f64::total_cmp);
        let p90 = all[(all.len() * 9 / 10).min(all.len() - 1)];
        eprintln!("  {what:<22} {:>8.2} / {p90:>8.2} ms", all[all.len() / 2]);
    }
}
