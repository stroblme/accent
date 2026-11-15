use super::*;

#[test]
fn field_edits_preserve_every_unrelated_byte() {
    let raw = "% @article{ignored,}\n@string{j = {A Journal}}\n@article(old,\n TITLE = {A {Protected} title}, % keep\n journal=j # { Supplement},\n custom = \"a {\\\"o} b\",\n year=2020\n)\n";
    let bib = Library::parse(raw).unwrap();
    assert_eq!(bib.entries.len(), 1);
    assert_eq!(
        bib.value(&bib.entries[0], "journal"),
        "A Journal Supplement"
    );
    let changed = bib.set_field(0, "year", "2026").unwrap();
    assert_eq!(changed, raw.replace("year=2020", "year={2026}"));
    assert!(Library::parse("@article{x,title={broken}").is_err());
    assert!(Library::parse("@article{x,title={a},title={b}}").is_err());
}

#[test]
fn copy_keeps_dependencies_and_refuses_collisions() {
    let raw = "@string{j={Journal}}\n@book{parent,title=j,year={2020}}\n@inbook{child,crossref={parent},title={Chapter}}\n@book{other,title={Unselected}}";
    let bib = Library::parse(raw).unwrap();
    let copied = bib.copy_entries(&[1], "% destination\n").unwrap();
    assert!(copied.starts_with("% destination\n"));
    assert!(copied.contains("@string"));
    assert!(copied.contains("@book{parent"));
    assert!(!copied.contains("@book{other"));
    assert!(
        bib.copy_entries(&[1], "@book{parent,title={Different}}")
            .is_err()
    );
    assert!(bib.copy_entries(&[1], "@string{j={Different}}").is_err());
}

#[test]
fn identity_and_keys_preserve_distinct_versions() {
    let raw = "@article{a,author={Smith, Jane},title={The Quantum Paper},year=2024,doi={https://doi.org/10.1234/ABC}}\n@article{b,author={Smith, Jane},title={The Quantum Paper},year=2024,doi={10.1234/abc}}\n@article{c,author={Smith, Jane},title={The Quantum Paper},year=2024,doi={10.1234/other}}";
    let bib = Library::parse(raw).unwrap();
    assert_eq!(bib.duplicates(), vec![(0, 1)]);
    assert_eq!(
        bib.generate_key(0, "{author}_{title}_{year}").unwrap(),
        "smith_quantum_2024"
    );
    assert!(bib.generate_key(0, "{unknown}").is_err());
    assert_eq!(
        doi("https://doi.org/10.1234/ABC%28x%29").unwrap(),
        "10.1234/abc(x)"
    );
    assert!(doi("https://example.org/10.1234/a").is_none());
}

#[test]
fn arxiv_identifiers_map_to_arxivs_own_pdf() {
    let pdf = |id: &str| Some(format!("https://arxiv.org/pdf/{id}"));
    for (input, id) in [
        ("https://doi.org/10.48550/arXiv.2611.11858", "2611.11858"),
        ("10.48550/ARXIV.2611.11858", "2611.11858"),
        ("10.48550/arXiv.hep-th/9901001", "hep-th/9901001"),
        ("arXiv:2611.11858v2", "2611.11858v2"),
        ("0704.0001", "0704.0001"),
        ("math.GT/0309136", "math.gt/0309136"),
        ("solv-int/9901001v1", "solv-int/9901001v1"),
        ("https://arxiv.org/abs/2611.11858", "2611.11858"),
        (
            "http://www.arxiv.org/pdf/hep-th/9901001v1.pdf",
            "hep-th/9901001v1",
        ),
    ] {
        assert_eq!(arxiv_pdf(input), pdf(id), "{input}");
    }
    for input in [
        "10.1234/2611.11858",
        "https://example.org/abs/2611.11858",
        "2611.118",
        "hep-th/990100",
        "",
    ] {
        assert_eq!(arxiv_pdf(input), None, "{input}");
    }
    // `c`'s arXiv copy is found beside its publisher DOI; a download tries it after the DOI's.
    let bib = Library::parse(
        "@misc{a,eprint={2611.11858},archivePrefix={arXiv}}\n@misc{b,eprint={hep-th/9901001},eprinttype={arxiv}}\n@misc{c,doi={10.1103/x},url={https://arxiv.org/abs/2611.11858}}\n@misc{d,eprint={2611.11858},archivePrefix={HAL}}\n@misc{e,doi={10.1103/x}}",
    )
    .unwrap();
    let found: Vec<_> = bib.entries.iter().map(|e| bib.arxiv_pdf(e)).collect();
    assert_eq!(
        found,
        [
            pdf("2611.11858"),
            pdf("hep-th/9901001"),
            pdf("2611.11858"),
            None,
            None
        ]
    );
}

#[test]
fn attachments_rebase_and_escape_jabref_separators() {
    let files = Attachment::parse("Paper:papers/a\\;b.pdf:PDF;:papers/other.pdf:PDF").unwrap();
    assert_eq!(files[0].path, "papers/a;b.pdf");
    let rebased = rebase_files("library/main.bib", "projects/paper/refs.bib", &files).unwrap();
    assert_eq!(rebased[0].path, "../../library/papers/a;b.pdf");
    assert_eq!(
        Attachment::parse(&Attachment::format(&rebased)).unwrap(),
        rebased
    );
    assert!(attachment_path("main.bib", "../../escape.pdf").is_err());
}

#[test]
fn comments_preambles_and_quoted_parentheses_survive_edits() {
    let raw = "@comment{a, b {nested}}\n@preamble{\"a\" # {b}}\n@article(key,title=\"a ) {b}\",year=2020)";
    let bib = Library::parse(raw).unwrap();
    assert_eq!(bib.entries.len(), 1);
    assert_eq!(
        bib.set_field(0, "year", "2021").unwrap(),
        raw.replace("year=2020", "year={2021}")
    );
    assert!(bib.set_field(0, "title", "x}, injected={bad").is_err());
}

#[test]
fn field_insertion_respects_comments_and_rejects_invalid_expressions() {
    let raw = "@article{x,title={A} % comment\n}";
    let bib = Library::parse(raw).unwrap();
    let after = bib.set_field(0, "year", "2024").unwrap();
    assert_eq!(
        Library::parse(&after)
            .unwrap()
            .value(&Library::parse(&after).unwrap().entries[0], "year"),
        "2024"
    );
    assert!(Library::parse("@article{x, title={A} nonsense}").is_err());
}

#[test]
fn transfers_preserve_preambles_and_protect_remaining_children() {
    let bib = Library::parse("@string{prefix={A}}\n@preamble{prefix # { command}}\n@book{parent,title={Book}}\n@inbook{child,crossref={parent}}").unwrap();
    let copied = bib.copy_entries(&[1], "").unwrap();
    assert!(copied.contains("@preamble{prefix # { command}}"));
    assert!(copied.contains("@string{prefix={A}}"));
    assert_eq!(bib.copy_entries(&[1], &copied).unwrap(), copied);
    assert!(bib.move_remainder(&[0]).is_err());
    assert!(bib.move_remainder(&[1]).unwrap().contains("@book{parent"));
    assert!(
        !bib.move_remainder(&[0, 1])
            .unwrap()
            .contains("@book{parent")
    );
}

#[test]
fn attachment_rebasing_normalizes_destination() {
    let files = Attachment::parse(":paper.pdf:PDF").unwrap();
    let rebased = rebase_files("library/main.bib", "project/../refs.bib", &files).unwrap();
    assert_eq!(rebased[0].path, "library/paper.pdf");
    assert!(rebase_files("library/main.bib", "../refs.bib", &files).is_err());
}

#[test]
fn a_citation_is_written_as_the_document_reads_one() {
    assert_eq!(citation(&["a", "b:2"], false), "\\cite{a,b:2}");
    assert_eq!(
        citation(&["knuth84", "doe:2020"], true),
        "@knuth84 @doe:2020"
    );
    // A key `@` cannot spell is cited by its label, quoted as a Typst string.
    assert_eq!(
        citation(&["10.1000/x", "end.", "a\"b"], true),
        "#cite(label(\"10.1000/x\")) #cite(label(\"end.\")) #cite(label(\"a\\\"b\"))"
    );
}
