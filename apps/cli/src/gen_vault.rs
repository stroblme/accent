//! `gen-vault`: a synthetic Obsidian-shaped vault for tests and benchmarks.
//!
//! The user's real vault is never read, not even to sample it — this only mimics its *shape*:
//! folder depth, note-size distribution, wikilinks/embeds, a ~300-tag Zipf pool, Syncthing
//! conflicts and temp files, `.obsidian/`, a symlinked external code repo, an in-vault `.venv`.
//!
//! Deterministic: the same `--seed --notes --files` produces a byte-identical tree, because
//! every random draw comes from one splitmix64 stream consumed in a fixed order (hence: no
//! parallel writing).
//!
//! ponytail: std only, no `rand`/`chrono`/image crates. Images carry the right magic bytes and
//! random payload but are not decodable; upgrade to real IHDR/IDAT chunks the day a thumbnailer
//! test needs one. PDFs *are* valid (hand-written objects + xref), so the `pdf` module can open
//! them.

use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Instant;

// ------------------------------------------------------------------ rng

/// splitmix64: 4 lines, deterministic, plenty for test data.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
    /// Inclusive on both ends.
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
    fn word(&mut self) -> &'static str {
        WORDS[self.below(WORDS.len())]
    }
    /// Log-uniform index: 0 is the most popular, the tail is long — close enough to Zipf.
    fn zipf(&mut self, n: usize) -> usize {
        let u = self.unit();
        (((n as f64).powf(u)) as usize).min(n - 1)
    }
}

// ------------------------------------------------------------------ vocabulary

/// Deliberately excludes "missing": unresolved link targets are built from it.
const WORDS: &[&str] = &[
    "quantum",
    "circuit",
    "lattice",
    "ansatz",
    "qubit",
    "gate",
    "noise",
    "error",
    "correction",
    "decoherence",
    "entanglement",
    "fidelity",
    "measurement",
    "hamiltonian",
    "spectrum",
    "eigenvalue",
    "variational",
    "optimizer",
    "gradient",
    "sampling",
    "benchmark",
    "latency",
    "throughput",
    "scheduler",
    "compiler",
    "transpiler",
    "backend",
    "simulator",
    "kernel",
    "tensor",
    "network",
    "encoding",
    "decoder",
    "syndrome",
    "stabilizer",
    "surface",
    "code",
    "threshold",
    "overhead",
    "pipeline",
    "dataset",
    "baseline",
    "ablation",
    "protocol",
    "calibration",
    "pulse",
    "waveform",
    "resonator",
    "transmon",
    "coupler",
    "cryostat",
    "readout",
    "amplifier",
    "attenuation",
    "crosstalk",
    "dephasing",
    "relaxation",
    "coherence",
    "sequence",
    "tomography",
    "estimator",
    "shots",
    "expectation",
    "observable",
    "commutator",
    "unitary",
    "channel",
    "density",
    "matrix",
    "trace",
    "purity",
    "entropy",
    "mutual",
    "information",
    "capacity",
    "bound",
    "theorem",
    "lemma",
    "proof",
    "sketch",
    "draft",
    "outline",
    "review",
    "revision",
    "deadline",
    "submission",
    "rebuttal",
    "poster",
    "talk",
    "slides",
    "meeting",
    "agenda",
    "minutes",
    "action",
    "item",
    "followup",
    "reading",
    "summary",
    "question",
    "answer",
    "idea",
    "hypothesis",
    "experiment",
    "result",
    "figure",
    "table",
    "appendix",
    "reference",
    "citation",
    "library",
    "archive",
    "inbox",
    "journal",
    "weekly",
    "planning",
    "thesis",
    "chapter",
    "section",
    "paragraph",
    "footnote",
    "cluster",
    "runtime",
    "budget",
    "sweep",
];

const AREAS: &[&str] = &[
    "phd",
    "qc",
    "reading",
    "meeting",
    "idea",
    "project",
    "paper",
    "teaching",
    "admin",
    "tool",
    "theory",
    "experiment",
    "hardware",
    "software",
    "review",
    "travel",
    "conference",
    "thesis",
    "grant",
    "personal",
];

const PROJECTS: &[&str] = &[
    "proj-a",
    "proj-b",
    "qec-scaling",
    "pulse-shaping",
    "noise-atlas",
    "compiler-bench",
    "thesis-defense",
    "grant-2026",
];

const CONFS: &[&str] = &["QIP", "IEEE-QCE", "Qiskit-Camp", "APS-March"];

// ------------------------------------------------------------------ summary

#[derive(Debug, Default, Clone)]
pub struct Summary {
    /// Regular files written inside the vault (symlinks and directories not counted).
    pub files: usize,
    pub dirs: usize,
    /// Everything the walker will treat as markdown: notes + excalidraw + external repo notes.
    pub md_files: usize,
    pub symlinks: usize,
    pub bytes: u64,
    /// Files written in the `<out>-external` tree (most are `.gitignore`d away at scan time).
    pub external_files: usize,
    /// Files in the real in-vault `.venv` (indexable: only `.accentignore` could hide them).
    pub venv_files: usize,
    pub ms: u128,
}

// ------------------------------------------------------------------ generator

struct Gen {
    root: PathBuf,
    ext_root: PathBuf,
    rng: Rng,
    /// 1 MiB of noise, sliced for file payloads — cheaper than per-byte rng, just as incompressible.
    blob: Vec<u8>,
    tags: Vec<String>,
    dirs: HashSet<String>,
    files: usize,
    md_files: usize,
    bytes: u64,
    symlinks: usize,
    external_files: usize,
}

const BLOB: usize = 1 << 20;

impl Gen {
    fn new(root: PathBuf, ext_root: PathBuf, seed: u64) -> Self {
        let mut rng = Rng(seed);
        let mut blob = Vec::with_capacity(BLOB);
        while blob.len() < BLOB {
            blob.extend_from_slice(&rng.next_u64().to_le_bytes());
        }
        // ponytail: O(n^2) dedup over 300 strings — 45k comparisons, once.
        let mut tags: Vec<String> = AREAS.iter().map(|a| (*a).to_string()).collect();
        while tags.len() < 300 {
            let t = format!("{}/{}", AREAS[rng.below(AREAS.len())], WORDS[rng.below(60)]);
            if !tags.contains(&t) {
                tags.push(t);
            }
        }
        Gen {
            root,
            ext_root,
            rng,
            blob,
            tags,
            dirs: HashSet::new(),
            files: 0,
            md_files: 0,
            bytes: 0,
            symlinks: 0,
            external_files: 0,
        }
    }

    fn mkdir(&mut self, rel: &str) -> Result<()> {
        if self.dirs.contains(rel) {
            return Ok(());
        }
        let p = self.root.join(rel);
        fs::create_dir_all(&p).with_context(|| format!("mkdir {}", p.display()))?;
        let mut acc = String::new();
        for c in rel.split('/') {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(c);
            self.dirs.insert(acc.clone());
        }
        Ok(())
    }

    fn write(&mut self, rel: &str, data: &[u8]) -> Result<()> {
        let p = self.root.join(rel);
        fs::write(&p, data).with_context(|| format!("write {}", p.display()))?;
        self.files += 1;
        self.bytes += data.len() as u64;
        if rel.ends_with(".md") && !rel.contains(".sync-conflict-") {
            self.md_files += 1;
        }
        Ok(())
    }

    fn write_ext(&mut self, rel: &str, data: &[u8]) -> Result<()> {
        let p = self.ext_root.join(rel);
        if let Some(d) = p.parent() {
            fs::create_dir_all(d)?;
        }
        fs::write(&p, data)?;
        self.external_files += 1;
        self.bytes += data.len() as u64;
        Ok(())
    }

    fn payload(&mut self, n: usize, out: &mut Vec<u8>) {
        let mut left = n;
        while left > 0 {
            let take = left.min(BLOB / 2);
            let off = self.rng.below(BLOB - take);
            out.extend_from_slice(&self.blob[off..off + take]);
            left -= take;
        }
    }

    fn tag(&mut self) -> String {
        let i = self.rng.zipf(self.tags.len());
        self.tags[i].clone()
    }

    fn words(&mut self, n: usize) -> String {
        (0..n)
            .map(|_| self.rng.word())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn title_case(&mut self, n: usize) -> String {
        let mut s = String::new();
        for i in 0..n {
            if i > 0 {
                s.push(' ');
            }
            let w = self.rng.word();
            let mut c = w.chars();
            s.push(c.next().unwrap().to_ascii_uppercase());
            s.push_str(c.as_str());
        }
        s
    }

    /// Always a real calendar date (day <= 28), so no month table is needed.
    fn date(&mut self) -> String {
        format!(
            "{}-{:02}-{:02}",
            self.rng.range(2019, 2026),
            self.rng.range(1, 12),
            self.rng.range(1, 28)
        )
    }
}

// ------------------------------------------------------------------ note bodies

struct Corpus {
    /// Every note's link key (file stem), for resolvable wikilinks.
    stems: Vec<String>,
    imgs: usize,
    pdfs: usize,
}

impl Gen {
    /// ~90% resolvable, in all three wikilink shapes.
    fn wikilink(&mut self, c: &Corpus) -> String {
        let t = if self.rng.chance(0.9) {
            c.stems[self.rng.below(c.stems.len())].clone()
        } else {
            format!("Missing {}", self.title_case(2))
        };
        match self.rng.below(3) {
            0 => format!("[[{t}]]"),
            1 => format!("[[{t}#{}]]", self.title_case(2)),
            _ => format!("[[{t}|{}]]", self.words(2)),
        }
    }

    fn embed(&mut self, c: &Corpus) -> String {
        if self.rng.chance(0.6) {
            format!("![[Attachments/img-{}.png]]", self.rng.below(c.imgs))
        } else {
            format!(
                "![[Attachments/paper-{}.pdf#page={}&selection={},0,{},12]]",
                self.rng.below(c.pdfs),
                self.rng.range(1, 12),
                self.rng.range(1, 40),
                self.rng.range(41, 80)
            )
        }
    }

    fn paragraph(&mut self, out: &mut String, c: &Corpus) {
        for _ in 0..self.rng.range(3, 8) {
            let n = self.rng.range(6, 16);
            for i in 0..n {
                if i > 0 {
                    out.push(' ');
                }
                let w = self.rng.word();
                if i == 0 {
                    let mut ch = w.chars();
                    out.push(ch.next().unwrap().to_ascii_uppercase());
                    out.push_str(ch.as_str());
                } else {
                    out.push_str(w);
                }
            }
            match self.rng.below(12) {
                0 => {
                    let l = self.wikilink(c);
                    out.push_str(&format!(" {l}"));
                }
                1 => {
                    let t = self.tag();
                    out.push_str(&format!(" #{t}"));
                }
                2 => {
                    let w = self.words(2);
                    out.push_str(&format!(" **{w}**"));
                }
                3 => out.push_str(" $\\langle \\psi | H | \\psi \\rangle$"),
                4 => {
                    let w = self.words(2);
                    out.push_str(&format!(" *{w}*"));
                }
                _ => {}
            }
            out.push_str(". ");
        }
        out.push_str("\n\n");
    }

    fn note_body(&mut self, title: &str, target: usize, c: &Corpus) -> String {
        let mut s = String::with_capacity(target + 1024);
        s.push_str("---\n");
        s.push_str(&format!("title: {title}\n"));
        if self.rng.chance(0.6) {
            let n = self.rng.range(1, 4);
            let tags: Vec<String> = (0..n).map(|_| self.tag()).collect();
            if self.rng.chance(0.5) {
                s.push_str(&format!("tags: [{}]\n", tags.join(", ")));
            } else {
                s.push_str("tags:\n");
                for t in tags {
                    s.push_str(&format!("  - {t}\n"));
                }
            }
        }
        s.push_str(&format!("created: {}\n", self.date()));
        s.push_str("---\n\n");
        s.push_str(&format!("# {title}\n\n"));

        while s.len() < target {
            match self.rng.below(16) {
                0 | 1 => {
                    let n = self.rng.range(2, 4);
                    let h = self.title_case(n);
                    let lvl = if self.rng.chance(0.6) { "##" } else { "###" };
                    s.push_str(&format!("{lvl} {h}\n\n"));
                }
                2 => {
                    for _ in 0..self.rng.range(2, 6) {
                        let done = self.rng.chance(0.4);
                        let n = self.rng.range(4, 9);
                        let w = self.words(n);
                        s.push_str(&format!("- [{}] {w}\n", if done { "x" } else { " " }));
                    }
                    s.push('\n');
                }
                3 => {
                    // Fenced code: the walker must NOT see a link or a tag in here.
                    let f = self.words(1);
                    s.push_str("```python\n");
                    s.push_str(&format!(
                        "def {f}(shots=1024):  # [[not a link]] #notatag\n"
                    ));
                    s.push_str("    return sum(range(shots)) / shots\n");
                    s.push_str("```\n\n");
                }
                4 => {
                    let (a, b) = (self.words(1), self.words(1));
                    s.push_str(&format!("| {a} | {b} | value |\n| --- | --- | --- |\n"));
                    for _ in 0..self.rng.range(2, 5) {
                        let (x, y) = (self.words(1), self.words(1));
                        let v = self.rng.range(1, 999);
                        s.push_str(&format!("| {x} | {y} | {v} |\n"));
                    }
                    s.push('\n');
                }
                5 => {
                    let e = self.embed(c);
                    s.push_str(&format!("{e}\n\n"));
                }
                6 => {
                    let n = self.rng.range(6, 14);
                    let w = self.words(n);
                    s.push_str(&format!("> {w}\n\n"));
                }
                _ => self.paragraph(&mut s, c),
            }
        }
        s
    }
}

// ------------------------------------------------------------------ binary payloads

impl Gen {
    fn png(&mut self, n: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(n);
        v.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        self.payload(n.saturating_sub(8), &mut v);
        v
    }

    fn jpg(&mut self, n: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(n);
        v.extend_from_slice(
            b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00",
        );
        self.payload(n.saturating_sub(20), &mut v);
        v.extend_from_slice(b"\xff\xd9");
        v
    }

    /// A real single-page PDF: catalog, pages, page, Helvetica font, one text stream, xref.
    fn pdf(&mut self, name: &str) -> Vec<u8> {
        let line = self.words(6);
        let content = format!(
            "BT /F1 18 Tf 60 720 Td ({name}) Tj ET\nBT /F1 11 Tf 60 690 Td ({line}) Tj ET\n"
        );
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        ];
        let mut out = Vec::with_capacity(1024);
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::with_capacity(objs.len());
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn json(&mut self, n: usize) -> Vec<u8> {
        let mut s = String::with_capacity(n + 64);
        s.push_str("{\n  \"version\": 3,\n  \"items\": [\n");
        while s.len() < n {
            let (k, t) = (self.words(2), self.tag());
            let v = self.rng.range(1, 100_000);
            s.push_str(&format!(
                "    {{\"key\": \"{k}\", \"tag\": \"{t}\", \"n\": {v}}},\n"
            ));
        }
        s.push_str("    {\"key\": \"end\", \"tag\": \"admin\", \"n\": 0}\n  ]\n}\n");
        s.into_bytes()
    }

    fn csv(&mut self, n: usize) -> Vec<u8> {
        let mut s = String::with_capacity(n + 64);
        s.push_str("shots,backend,fidelity,note\n");
        while s.len() < n {
            let b = self.words(1);
            let shots = self.rng.range(128, 100_000);
            let f = self.rng.unit();
            let note = self.words(3);
            s.push_str(&format!("{shots},{b},{f:.6},{note}\n"));
        }
        s.into_bytes()
    }

    /// Obsidian's Excalidraw plugin stores drawings as markdown with a JSON block.
    fn excalidraw(&mut self, n: usize) -> Vec<u8> {
        let mut s = String::from(
            "---\n\nexcalidraw-plugin: parsed\ntags: [excalidraw]\n\n---\n\n# Excalidraw Data\n\n## Text Elements\n",
        );
        for _ in 0..self.rng.range(3, 9) {
            let w = self.words(3);
            let id = self.rng.next_u64();
            s.push_str(&format!("{w} ^{id:x}\n\n"));
        }
        s.push_str("## Drawing\n```json\n{\n\t\"type\": \"excalidraw\",\n\t\"elements\": [\n");
        while s.len() < n {
            let (x, y, w, h) = (
                self.rng.range(0, 1200),
                self.rng.range(0, 900),
                self.rng.range(20, 400),
                self.rng.range(20, 300),
            );
            let id = self.rng.next_u64();
            s.push_str(&format!(
                "\t\t{{\"id\":\"{id:x}\",\"type\":\"rectangle\",\"x\":{x},\"y\":{y},\"width\":{w},\"height\":{h}}},\n"
            ));
        }
        s.push_str("\t\t{\"id\":\"end\",\"type\":\"text\",\"x\":0,\"y\":0}\n\t]\n}\n```\n%%\n");
        s.into_bytes()
    }

    fn py_module(&mut self, name: &str) -> Vec<u8> {
        let mut s = format!("\"\"\"{name}: {}.\"\"\"\n\n", self.words(5));
        for _ in 0..self.rng.range(3, 12) {
            let f = self.words(1);
            let v = self.rng.range(1, 9999);
            s.push_str(&format!(
                "def {f}_{v}(x=None):\n    return {v} if x is None else x\n\n"
            ));
        }
        s.into_bytes()
    }
}

// ------------------------------------------------------------------ tree

/// ~200 note directories, 2–4 levels deep, in the shape of a research vault.
fn note_dirs(rng: &mut Rng) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut dirs = Vec::new();
    let mut push = |d: String, dirs: &mut Vec<String>| {
        if seen.insert(d.clone()) {
            dirs.push(d);
        }
    };
    for d in [
        "Daily",
        "Templates",
        "Inbox",
        "Attachments",
        "Notes-PHD/thesis",
    ] {
        push(d.to_string(), &mut dirs);
    }
    for top in ["Notes-PHD", "Notes-QC", "Resources"] {
        for _ in 0..10 {
            let t = format!(
                "{}-{}",
                WORDS[rng.below(WORDS.len())],
                WORDS[rng.below(WORDS.len())]
            );
            push(format!("{top}/{t}"), &mut dirs);
            for _ in 0..rng.below(4) {
                let s = WORDS[rng.below(WORDS.len())];
                push(format!("{top}/{t}/{s}"), &mut dirs);
            }
        }
    }
    for year in 2019..=2026 {
        for conf in CONFS {
            push(format!("Events/{year}/{conf}"), &mut dirs);
        }
    }
    for p in PROJECTS {
        push(format!("Submissions/{p}"), &mut dirs);
        push(format!("Submissions/{p}/figures"), &mut dirs);
        push(format!("Submissions/{p}/drafts"), &mut dirs);
    }
    for year in 2016..=2023 {
        push(format!("Archive/{year}"), &mut dirs);
    }
    dirs
}

// ------------------------------------------------------------------ run

pub fn run(out: &Path, notes: usize, files: usize, seed: u64, force: bool) -> Result<Summary> {
    let t0 = Instant::now();
    let name = out
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .context("<out_dir> must be a plain directory path, e.g. testvault/")?;
    let ext_root = out.with_file_name(format!("{name}-external"));

    // Never generate on top of a real notes tree, whatever the flags say.
    let abs = if out.is_absolute() {
        out.to_path_buf()
    } else {
        std::env::current_dir()?.join(out)
    };
    if abs.to_string_lossy().contains("Sync/Notes") {
        bail!(
            "refusing to write inside a Syncthing notes tree: {}",
            abs.display()
        );
    }
    if out.exists() && out.read_dir()?.next().is_some() && !force {
        bail!("{} is not empty (use --force to wipe it)", out.display());
    }
    if force {
        for p in [out, ext_root.as_path()] {
            if p.exists() {
                fs::remove_dir_all(p).with_context(|| format!("wiping {}", p.display()))?;
            }
        }
    }
    fs::create_dir_all(out)?;
    fs::create_dir_all(&ext_root)?;

    let mut g = Gen::new(out.to_path_buf(), ext_root.clone(), seed);

    // Flat, embeddable attachments are named before the notes are written, so `![[…]]` targets
    // exist by the time the walker resolves them.
    let flat_imgs = (files / 100).clamp(1, 300);
    let flat_pdfs = (files / 200).clamp(1, 150);
    let excal = (files / 1000).clamp(1, 40);
    let venv_in = (files / 40).clamp(2, 1000);
    let venv_ext = (files / 20).clamp(4, 2000);

    let floor = notes + 20 + venv_in + flat_imgs + flat_pdfs + excal;
    if files < floor {
        bail!("--files {files} is too small for --notes {notes}: need at least {floor}");
    }

    let dirs = note_dirs(&mut g.rng);
    for d in &dirs {
        g.mkdir(d)?;
    }

    // ---- plan the notes (paths first: bodies link to each other by stem)
    let daily = notes / 7; // ~15% of a research vault is daily notes
    let mut plan: Vec<(String, String)> = Vec::with_capacity(notes); // (dir, stem)
    let mut used: HashSet<String> = HashSet::new();
    for i in 0..notes {
        let (dir, stem) = match i {
            0 => ("Notes-QC".to_string(), "Real".to_string()), // the file symlink's target
            1..=3 => ("Notes-PHD/thesis".to_string(), g.title_case(3)), // under a `*.md` .gitignore
            _ if i < 4 + daily => {
                let d = i - 4;
                (
                    "Daily".to_string(),
                    format!(
                        "{}-{:02}-{:02}",
                        2024 + d / 336,
                        (d % 336) / 28 + 1,
                        d % 28 + 1
                    ),
                )
            }
            _ => {
                let d = dirs[g.rng.below(dirs.len())].clone();
                let n = g.rng.range(2, 5);
                (d, g.title_case(n))
            }
        };
        let stem = if used.insert(stem.to_lowercase()) {
            stem
        } else {
            let s = format!("{stem} {i}");
            used.insert(s.to_lowercase());
            s
        };
        plan.push((dir, stem));
    }
    g.mkdir("Notes-QC")?;

    let corpus = Corpus {
        stems: plan.iter().map(|(_, s)| s.clone()).collect(),
        imgs: flat_imgs,
        pdfs: flat_pdfs,
    };

    // ---- notes: 70% 1–4 KB, 25% 5–30 KB, 5% 100–400 KB, one ~1 MB
    for (i, (dir, stem)) in plan.iter().enumerate() {
        let target = if i == 1 {
            1_000_000
        } else {
            match g.rng.unit() {
                u if u < 0.70 => g.rng.range(1024, 4096),
                u if u < 0.95 => g.rng.range(5 * 1024, 30 * 1024),
                _ => g.rng.range(100 * 1024, 400 * 1024),
            }
        };
        let body = g.note_body(stem, target, &corpus);
        g.write(&format!("{dir}/{stem}.md"), body.as_bytes())?;
    }

    // ---- Syncthing artefacts, next to real notes
    for k in 0..6.min(plan.len()) {
        let (dir, stem) = &plan[(k * 37) % plan.len()];
        let (dir, stem) = (dir.clone(), stem.clone());
        let body = g.note_body(&stem, 2048, &corpus);
        let dev: String = (0..7)
            .map(|_| (b'A' + g.rng.below(26) as u8) as char)
            .collect();
        g.write(
            &format!(
                "{dir}/{stem}.sync-conflict-2026090{}-10150{}-{dev}.md",
                k % 9,
                k % 9
            ),
            body.as_bytes(),
        )?;
    }
    for k in 0..2.min(plan.len()) {
        let (dir, stem) = &plan[(k * 101) % plan.len()];
        let (dir, stem) = (dir.clone(), stem.clone());
        let body = g.note_body(&stem, 1024, &corpus);
        g.write(&format!("{dir}/.syncthing.{stem}.md.tmp"), body.as_bytes())?;
    }
    g.mkdir(".stfolder")?;
    g.write(".stignore", b"(?d).DS_Store\n(?d)Thumbs.db\n.trash\n")?;
    g.write(
        ".accentignore",
        b"# accent honours only this file inside the vault (never .gitignore).\n\
          # Uncommenting the next line drops the real in-vault .venv from the index:\n\
          # .venv/\n",
    )?;
    g.write(
        "Notes-PHD/thesis/.gitignore",
        b"# accent must ignore this file inside the vault: the notes below are real notes.\n*.md\nreferences\n",
    )?;

    // ---- .obsidian
    g.mkdir(".obsidian/plugins/foo")?;
    g.write(
        ".obsidian/app.json",
        b"{\n  \"attachmentFolderPath\": \"Attachments\",\n  \"alwaysUpdateLinks\": true,\n  \"useMarkdownLinks\": false\n}\n",
    )?;
    g.write(
        ".obsidian/workspace.json",
        b"{\n  \"main\": {\"id\": \"root\", \"type\": \"split\", \"children\": []},\n  \"active\": \"editor\",\n  \"lastOpenFiles\": [\"Notes-QC/Real.md\"]\n}\n",
    )?;
    g.write(
        ".obsidian/plugins/foo/data.json",
        b"{\n  \"enabled\": true,\n  \"folder\": \"Templates\",\n  \"dateFormat\": \"YYYY-MM-DD\"\n}\n",
    )?;
    g.write(
        ".obsidian/daily-notes.json",
        b"{\"folder\":\"Daily\",\"format\":\"YYYY-MM-DD\",\"template\":\"Templates/Daily\"}\n",
    )?;

    // ---- real templates: written verbatim, because template expansion is what they are for
    g.write(
        "Templates/Daily.md",
        b"---\ndate: {{date}}\ntags: [daily]\n---\n\n\
          # {{date:%A, %d %B %Y}}\n\n\
          ## Log\n\n\
          - {{time}} {{cursor}}\n",
    )?;
    g.write(
        "Templates/Meeting.md",
        b"# {{title}}\n\n\
          ## Attendees\n\n- Me\n\n\
          ## Agenda\n\n{{cursor}}\n\n\
          ## Actions\n\n- [ ] follow up\n",
    )?;
    g.write(
        "Templates/Paper.md",
        b"---\nadded: {{date:%Y-%m-%dT%H:%M}}\ntags: [paper]\n---\n\n\
          # {{title}}\n\n\
          ![[Attachments/paper-0.pdf]]\n\n\
          ## Notes\n\n{{cursor}}\n",
    )?;

    // ---- a real .venv inside the vault: only .accentignore could hide it, and it is commented out
    for i in 0..venv_in {
        let pkg = format!("pkg{:02}", i % 40);
        let dir = format!("Submissions/proj-b/examples/.venv/lib/python3.13/site-packages/{pkg}");
        g.mkdir(&dir)?;
        let m = format!("mod{:03}", i / 40);
        let body = g.py_module(&m);
        g.write(&format!("{dir}/{m}.py"), &body)?;
    }

    // ---- the external repo the vault symlinks into
    let ext_notes = 8;
    g.write_ext("proj-a/.gitignore", b".venv/\ntarget/\n")?;
    g.write_ext(
        "proj-a/README.md",
        b"# proj-a\n\nExternal code tree, reached through a vault symlink.\n",
    )?;
    for f in ["main.rs", "lib.rs", "util.rs"] {
        let body = format!("// {f}\nfn main() {{ println!(\"{}\"); }}\n", g.words(3));
        g.write_ext(&format!("proj-a/src/{f}"), body.as_bytes())?;
    }
    for _ in 0..ext_notes {
        let title = g.title_case(3);
        let body = g.note_body(&title, 2048, &corpus);
        g.write_ext(&format!("proj-a/src/{title}.md"), body.as_bytes())?;
    }
    for i in 0..venv_ext {
        let m = format!("mod{i:04}");
        let body = g.py_module(&m);
        g.write_ext(
            &format!(
                "proj-a/.venv/lib/python3.13/site-packages/pkg{:02}/{m}.py",
                i % 50
            ),
            &body,
        )?;
    }
    for i in 0..(venv_ext / 4).max(1) {
        let body = g.png(2048);
        g.write_ext(&format!("proj-a/target/debug/blob{i:04}.bin"), &body)?;
    }
    g.md_files += ext_notes; // the external .md notes are indexed through the symlink

    // ---- symlinks
    g.mkdir("Submissions/proj-a")?;
    g.mkdir("Resources")?;
    g.mkdir("Archive")?;
    let abs_ext = ext_root.canonicalize().unwrap_or(ext_root.clone());
    symlink(abs_ext.join("proj-a"), out.join("Submissions/proj-a/code"))?; // followed, .gitignore applies
    symlink("../Notes-QC", out.join("Resources/link-to-notes"))?; // skipped: target inside vault
    symlink("../Archive", out.join("Archive/loop"))?; // skipped: loop
    symlink("Real.md", out.join("Notes-QC/alias.md"))?; // file symlink -> one alias
    g.symlinks = 4;

    // ---- flat, embeddable attachments
    for i in 0..flat_imgs {
        let n = g.rng.range(5 * 1024, 60 * 1024);
        let b = g.png(n);
        g.write(&format!("Attachments/img-{i}.png"), &b)?;
    }
    for i in 0..flat_pdfs {
        let name = format!("paper-{i}.pdf");
        let b = g.pdf(&name);
        g.write(&format!("Attachments/{name}"), &b)?;
    }
    g.mkdir("Attachments/Excalidraw")?;
    for i in 0..excal {
        let n = g.rng.range(8 * 1024, 40 * 1024);
        let b = g.excalidraw(n);
        g.write(
            &format!("Attachments/Excalidraw/Drawing-{i}.excalidraw.md"),
            &b,
        )?;
    }
    if files >= 5000 {
        for i in 0..6 {
            let n = g.rng.range(2 << 20, 5 << 20);
            let b = if i % 2 == 0 { g.png(n) } else { g.jpg(n) };
            let ext = if i % 2 == 0 { "png" } else { "jpg" };
            g.write(&format!("Attachments/scan-{i}.{ext}"), &b)?;
        }
    }

    // ---- bulk fill to exactly --files, spread over month buckets and a Zotero-ish store
    let mut bulk: Vec<String> = Vec::new();
    for y in 2019..=2026 {
        for m in 1..=12 {
            bulk.push(format!("Attachments/{y}-{m:02}"));
        }
    }
    for _ in 0..(files / 16).clamp(1, 2400) {
        bulk.push(format!(
            "Resources/library/storage/{:08X}",
            g.rng.next_u64() as u32
        ));
    }
    let mut i = 0usize;
    while g.files < files {
        let dir = bulk[i % bulk.len()].clone();
        g.mkdir(&dir)?;
        let zotero = dir.starts_with("Resources/library/storage");
        let (data, ext) = match (zotero, i % 16) {
            (true, 0..=5) => {
                let name = format!("doc-{i}.pdf");
                (g.pdf(&name), "pdf")
            }
            (true, 6..=13) => {
                let n = g.rng.range(512, 4096);
                (g.json(n), "json")
            }
            (true, _) => {
                let n = g.rng.range(5 * 1024, 40 * 1024);
                (g.png(n), "png")
            }
            (false, 0) => {
                let n = g.rng.range(5 * 1024, 120 * 1024);
                (g.png(n), "png")
            }
            (false, 1) => {
                let n = g.rng.range(5 * 1024, 120 * 1024);
                (g.jpg(n), "jpg")
            }
            (false, 2..=4) => {
                let name = format!("clip-{i}.pdf");
                (g.pdf(&name), "pdf")
            }
            (false, 5..=9) => {
                let n = g.rng.range(512, 4096);
                (g.json(n), "json")
            }
            (false, _) => {
                let n = g.rng.range(512, 8192);
                (g.csv(n), "csv")
            }
        };
        g.write(&format!("{dir}/file-{i}.{ext}"), &data)?;
        i += 1;
    }

    Ok(Summary {
        files: g.files,
        dirs: g.dirs.len(),
        md_files: g.md_files,
        symlinks: g.symlinks,
        bytes: g.bytes,
        external_files: g.external_files,
        venv_files: venv_in,
        ms: t0.elapsed().as_millis(),
    })
}

pub fn print_summary(out: &Path, s: &Summary) {
    println!("vault         {}", out.display());
    println!(
        "files         {}  (regular files inside the vault)",
        s.files
    );
    println!("dirs          {}", s.dirs);
    println!(
        "markdown      {}  (incl. excalidraw + the external repo's notes)",
        s.md_files
    );
    println!(
        "symlinks      {}  (1 external dir, 1 in-vault dir, 1 loop, 1 file)",
        s.symlinks
    );
    println!(
        "external      {} files in <out>-external (most .gitignore'd at scan time)",
        s.external_files
    );
    println!(
        "bytes         {:.1} MiB",
        s.bytes as f64 / (1024.0 * 1024.0)
    );
    println!("elapsed       {} ms", s.ms);
    println!(
        "note          .accentignore has `# .venv/` commented out, so the {} files under\n\
         \x20             Submissions/proj-b/examples/.venv/ are indexed; uncommenting it drops them.",
        s.venv_files
    );
}

// ------------------------------------------------------------------ test

#[cfg(test)]
mod tests {
    use super::*;
    use accent_core::walk::{self, FileKind, ScanOptions, SkipReason};

    /// Everything on disk under `dir`, including the entries the scanner filters out.
    fn all_names(dir: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            out.push(e.file_name().to_string_lossy().into_owned());
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) && !e.path().is_symlink() {
                all_names(&e.path(), out);
            }
        }
    }

    #[test]
    fn tiny_vault_is_generated_and_scans_as_expected() {
        let tmp = std::env::temp_dir().join(format!("accent-genvault-{}", std::process::id()));
        let vault = tmp.join("testvault");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let s = run(&vault, 20, 100, 7, false).unwrap();
        assert_eq!(s.files, 100, "--files is exact");
        assert!(s.md_files >= 20, "{s:?}");
        assert_eq!(s.symlinks, 4);

        let mut names = Vec::new();
        all_names(&vault, &mut names);
        assert_eq!(
            names
                .iter()
                .filter(|n| n.contains(".sync-conflict-"))
                .count(),
            6
        );
        assert_eq!(
            names
                .iter()
                .filter(|n| n.starts_with(".syncthing."))
                .count(),
            2
        );
        assert!(names.iter().any(|n| n == ".stignore"));
        assert!(names.iter().any(|n| n == ".accentignore"));
        assert!(names.iter().any(|n| n == ".stfolder"));
        for link in [
            "Submissions/proj-a/code",
            "Resources/link-to-notes",
            "Archive/loop",
            "Notes-QC/alias.md",
        ] {
            assert!(vault.join(link).is_symlink(), "{link} must be a symlink");
        }
        assert!(vault.join("Notes-QC/Real.md").is_file());

        for t in ["Daily", "Meeting", "Paper"] {
            assert!(
                vault.join(format!("Templates/{t}.md")).is_file(),
                "Templates/{t}.md must exist"
            );
        }
        let daily = fs::read_to_string(vault.join("Templates/Daily.md")).unwrap();
        assert!(daily.contains("{{cursor}}"), "{daily}");
        assert!(daily.contains("{{date:"), "{daily}");
        assert!(vault.join(".obsidian/daily-notes.json").is_file());

        let r = walk::scan(&vault, &ScanOptions::default());
        let md = r
            .files
            .iter()
            .filter(|f| f.kind == FileKind::Markdown)
            .count();
        assert!(md >= 20, "notes indexed: {md}");
        assert_eq!(
            r.files
                .iter()
                .filter(|f| f.kind == FileKind::Conflict)
                .count(),
            6
        );
        assert!(
            !r.files.iter().any(|f| f.rel_path.contains(".syncthing.")),
            "temp files indexed"
        );
        assert!(
            !r.aliases.is_empty(),
            "the file symlink must yield an alias"
        );

        // Exactly two symlinks are rejected: the in-vault shortcut and the loop.
        let mut reasons: Vec<_> = r.skipped.iter().map(|s| s.reason).collect();
        reasons.sort_by_key(|r| format!("{r:?}"));
        assert_eq!(
            reasons,
            vec![SkipReason::SymlinkLoop, SkipReason::TargetInsideVault],
            "{:?}",
            r.skipped
        );

        // The external repo is followed, but its .venv/target are gitignored away.
        assert!(
            r.files
                .iter()
                .any(|f| f.rel_path.starts_with("Submissions/proj-a/code/"))
        );
        assert!(!r.files.iter().any(|f| f.rel_path.contains("code/.venv")));

        fs::remove_dir_all(&tmp).unwrap();
    }
}
