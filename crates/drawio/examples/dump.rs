//! What accent makes of a draw.io file: its pages, what each paints, anything it could not draw,
//! how long that took, and whether the file survives a round trip. A tool for checking a real
//! diagram against the engine, not a test.
//!
//! `cargo run --release -p accent-drawio --example dump -- diagram.drawio`

use std::collections::BTreeMap;
use std::time::Instant;

use accent_drawio::{File, Prim, Run, shapes};

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump <file.drawio>");
    let bytes = std::fs::read(&path).expect("readable file");
    let started = Instant::now();
    let file = File::from_bytes(&bytes).expect("a draw.io file");
    println!(
        "parsed {} pages in {:?}",
        file.pages().len(),
        started.elapsed()
    );

    let mut unknown: BTreeMap<String, usize> = BTreeMap::new();
    let (mut cells, mut prims, mut unrouted, mut formulas) = (0, 0, 0, 0);
    let started = Instant::now();
    for (i, page) in file.pages().iter().enumerate() {
        let scene = accent_drawio::scene(page);
        let (mut paths, mut texts, mut images) = (0, 0, 0);
        for prim in &scene.prims {
            match prim {
                Prim::Path { .. } => paths += 1,
                Prim::Text { runs, .. } => {
                    texts += 1;
                    formulas += runs
                        .iter()
                        .filter(|r| matches!(r, Run::Math { .. }))
                        .count();
                }
                Prim::Image { .. } => images += 1,
            }
        }
        for cell in &page.cells {
            let style = cell.style.resolve(cell.edge);
            if cell.vertex && !shapes::is_known(style.shape()) {
                *unknown.entry(style.shape().to_string()).or_default() += 1;
            }
            if cell.edge && !scene.prims.iter().any(|p| p.cell() == cell.id) {
                unrouted += 1;
            }
        }
        cells += page.cells.len();
        prims += scene.prims.len();
        println!(
            "page {:>2} {:<40} {:>4} cells {:>4} paths {:>4} labels {:>3} pictures",
            i + 1,
            page.name(),
            page.cells.len(),
            paths,
            texts,
            images
        );
    }
    println!(
        "{cells} cells, {prims} prims, {formulas} formulas; scenes in {:?}",
        started.elapsed()
    );
    println!("edges drawing nothing: {unrouted}");
    println!("shapes drawn as stand-ins: {unknown:?}");

    let started = Instant::now();
    let xml = file.to_xml();
    let back = File::from_bytes(xml.as_bytes()).expect("our own output parses");
    println!(
        "written ({} bytes, was {}) in {:?}; reads back {}",
        xml.len(),
        bytes.len(),
        started.elapsed(),
        if back == file {
            "identical"
        } else {
            "DIFFERENT"
        }
    );
}
