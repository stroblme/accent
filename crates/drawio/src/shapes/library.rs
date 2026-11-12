// Derived from draw.io js/grapheditor/Graph.js and js/diagramly/Editor.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! draw.io's stencil packs, which hold the `<shape>` element a style's `shape=mxgraph.…` draws.
//! accent does not ship them, so they are read from a folder when there is one (`make stencils`
//! fills it), a file at a time as its stencils are asked for (`mxStencilRegistry.getStencil`).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

/// The packs [`source`] reads, once [`set_dir`] has named their folder.
static LIBRARY: Mutex<Option<Library>> = Mutex::new(None);

/// Point the lookup at draw.io's `stencils` folder. Called once at startup by the app.
pub fn set_dir(dir: impl Into<PathBuf>) {
    *LIBRARY.lock().unwrap_or_else(PoisonError::into_inner) = Some(Library::new(dir.into()));
}

/// The XML text of the `<shape>` element draw.io registers under `name`
/// (`mxgraph.aws4.lambda`), from `<shape` to `</shape>` inclusive. `None` without a folder,
/// for a name outside `mxgraph.`, or for a shape the packs do not hold.
pub(crate) fn source(name: &str) -> Option<String> {
    let mut library = LIBRARY.lock().unwrap_or_else(PoisonError::into_inner);
    library.as_mut()?.source(name)
}

/// A folder of stencil packs and what has been read of it.
struct Library {
    dir: PathBuf,
    /// The files asked for so far, as paths in `dir`, whether they were there or not.
    files: Vec<String>,
    /// Each stencil read so far, under its registered name: its file, an index into `files`, and
    /// the bytes of its element there.
    shapes: HashMap<String, (usize, Range<u64>)>,
}

impl Library {
    fn new(dir: PathBuf) -> Library {
        Library {
            dir,
            files: Vec::new(),
            shapes: HashMap::new(),
        }
    }

    fn source(&mut self, name: &str) -> Option<String> {
        if !self.shapes.contains_key(name) {
            for file in files(name) {
                if !self.files.contains(&file) {
                    self.read(file);
                }
            }
        }
        let (at, range) = self.shapes.get(name)?;
        let mut file = File::open(self.dir.join(&self.files[*at])).ok()?;
        file.seek(SeekFrom::Start(range.start)).ok()?;
        let mut text = String::new();
        file.take(range.end - range.start)
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    }

    /// Index the stencils in `file` (`mxStencilRegistry.loadStencilSet`). A file that is missing,
    /// not well-formed, or outside the folder holds none: a style's name may hold a `/` that
    /// makes the path absolute, which `join` would follow out of the folder.
    fn read(&mut self, file: String) {
        let path = Path::new(&file);
        let inside = path.components().all(|c| matches!(c, Component::Normal(_)));
        let text = inside.then(|| std::fs::read_to_string(self.dir.join(path)).ok());
        let found = text
            .flatten()
            .as_deref()
            .and_then(index)
            .unwrap_or_default();
        for (name, range) in found {
            self.shapes.insert(name, (self.files.len(), range));
        }
        self.files.push(file);
    }
}

/// The files draw.io loads to find stencil `name`, as paths in the stencils folder
/// (`mxStencilRegistry.getStencil` and `getBasenameForStencil`): `mxgraph.a.b.c` is in `a/b.xml`,
/// unless package `a/b` is one of [`library`]'s.
fn files(name: &str) -> Vec<String> {
    let mut parts: Vec<&str> = name.split('.').collect();
    // The last part names the stencil, the second the package even when it is the last.
    if parts.len() > 2 {
        parts.pop();
    }
    let package = match parts.split_first() {
        Some((&"mxgraph", package)) if !package.is_empty() => package.join("/"),
        _ => return Vec::new(),
    };
    match library(&package) {
        Some(files) => files.iter().map(|f| f.to_string()).collect(),
        None => vec![format!("{}.xml", package.replacen("_-_", "_", 1))],
    }
}

/// The XML files of the packages whose files do not follow from their name
/// (`mxStencilRegistry.libraries`, Editor.js 12057-12120). Those that list only scripts, which
/// draw their shapes in code, have none.
fn library(package: &str) -> Option<&'static [&'static str]> {
    Some(match package {
        "atlassian" => &["atlassian.xml"],
        "bpmn" | "bpmn2" => &["bpmn.xml"],
        "cisco19" => &["cisco19.xml"],
        "cisco_safe" => &[
            "cisco_safe/architecture.xml",
            "cisco_safe/business_icons.xml",
            "cisco_safe/capability.xml",
            "cisco_safe/design.xml",
            "cisco_safe/iot_things_icons.xml",
            "cisco_safe/people_places_things_icons.xml",
            "cisco_safe/security_icons.xml",
            "cisco_safe/technology_icons.xml",
            "cisco_safe/threat.xml",
        ],
        "kubernetes" => &["kubernetes.xml", "kubernetes2.xml"],
        "flowchart" => &["flowchart.xml"],
        "rackGeneral" => &["rack/general.xml"],
        "rackF5" => &["rack/f5.xml"],
        "lean_mapping" => &["lean_mapping.xml"],
        "basic" => &["basic.xml"],
        "ios7icons" => &["ios7/icons.xml"],
        "ios7ui" => &["ios7/misc.xml"],
        "android" => &["android/android.xml"],
        "electrical/abstract" => &["electrical/abstract.xml"],
        "electrical/logic_gates" => &["electrical/logic_gates.xml"],
        "electrical/miscellaneous" => &["electrical/miscellaneous.xml"],
        "electrical/signal_sources" => &["electrical/signal_sources.xml"],
        "electrical/electro-mechanical" => &["electrical/electro-mechanical.xml"],
        "electrical/transmission" => &["electrical/transmission.xml"],
        "mockup/graphics" | "mockup/misc" | "mockup/navigation" => &["mockup/misc.xml"],
        "floorplan" => &["floorplan.xml"],
        "bootstrap" => &["bootstrap.xml"],
        "gmdl" => &["gmdl.xml"],
        "gcp2" => &["gcp2.xml"],
        "ibm" => &["ibm.xml"],
        "ibmcloud" => &["ibm_cloud.xml"],
        "cabinets" => &["cabinets.xml"],
        "eip" => &["eip.xml"],
        "networks" => &["networks.xml"],
        "networks2" => &["networks2.xml"],
        "atlassian2" => &["atlassian2.xml"],
        "aws3d" => &["aws3d.xml"],
        "aws4" | "aws4b" => &["aws4.xml"],
        "veeam" => &["veeam/2d.xml", "veeam/3d.xml", "veeam/veeam.xml"],
        "veeam2" => &["veeam/2d.xml", "veeam/3d.xml", "veeam/veeam2.xml"],
        "pid2misc" => &["pid/misc.xml"],
        "pidFlowSensors" => &["pid/flow_sensors.xml"],
        "salesforce" => &["salesforce.xml"],
        "sap" => &["sap.xml"],
        "mockup" | "arrows2" | "c4" | "dfd" | "er" | "ios" | "infographic" | "mockup/buttons"
        | "mockup/containers" | "mockup/forms" | "mockup/markup" | "mockup/text" | "archimate"
        | "archimate3" | "archimate4" | "sysml" | "uml25" | "pid2inst" | "pid2valves" | "emoji" => {
            &[]
        }
        _ => return None,
    })
}

/// Each stencil of a stencil file under the name draw.io registers it by, with the bytes of its
/// element (`mxStencilRegistry.parseStencilSet`). The document element is a set of stencils, or a
/// `<stencils>` holding several `<shapes>` sets. `None` for a file that is not well-formed.
fn index(xml: &str) -> Option<Vec<(String, Range<u64>)>> {
    let mut reader = Reader::from_str(xml);
    let mut found = Vec::new();
    // The elements open around the next one: `<stencils>` as `None`, a set as the prefix its
    // stencils are named with.
    let mut open: Vec<Option<String>> = Vec::new();
    loop {
        let start = reader.buffer_position();
        let (e, empty) = match reader.read_event().ok()? {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(_) => {
                open.pop();
                continue;
            }
            Event::Eof => return Some(found),
            _ => continue,
        };
        let name = e
            .try_get_attribute("name")
            .ok()
            .flatten()
            .and_then(|a| a.normalized_value(XmlVersion::Implicit1_0).ok())
            .map(|name| name.into_owned());
        match open.last() {
            // In a set, each named element is a stencil.
            Some(Some(prefix)) => {
                let key = name.map(|n| prefix.clone() + &n.replace(' ', "_").to_lowercase());
                if !empty {
                    reader.read_to_end(e.name()).ok()?;
                }
                found.extend(key.map(|key| (key, start..reader.buffer_position())));
            }
            // In `<stencils>`, only a `<shapes>` is a set.
            Some(None) if e.name().as_ref() != "shapes" => {
                if !empty {
                    reader.read_to_end(e.name()).ok()?;
                }
            }
            None if e.name().as_ref() == "stencils" => {
                if !empty {
                    open.push(None);
                }
            }
            _ => {
                let prefix = name.map(|n| n.to_lowercase() + ".").unwrap_or_default();
                if !empty {
                    open.push(Some(prefix));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_maps_to_the_files_draw_io_loads_for_it() {
        assert_eq!(files("mxgraph.a.b.c"), ["a/b.xml"]);
        assert_eq!(files("mxgraph.aws4.lambda"), ["aws4.xml"]);
        assert_eq!(
            files("mxgraph.a"),
            ["a.xml"],
            "the package even as the last part"
        );
        assert_eq!(files("mxgraph.pid.pumps_-_din.x"), ["pid/pumps_din.xml"]);
        assert_eq!(
            files("mxgraph.rackF5.x"),
            ["rack/f5.xml"],
            "a library of the table"
        );
        assert!(
            files("mxgraph.mockup.x").is_empty(),
            "a library drawn by code alone"
        );
        assert!(files("mxgraph").is_empty() && files("rect").is_empty());
        assert!(
            files("mxGraph.a.b").is_empty(),
            "a style's name is matched as it is"
        );
    }

    #[test]
    fn a_stencil_is_its_element_under_its_registered_name() {
        let dir = std::env::temp_dir().join(format!("accent-stencils-{}", std::process::id()));
        let write = |file: &str, xml: &str| {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, xml).unwrap();
        };
        let big = r#"<shape name="Big Box" w="10" h="20"><foreground><rect x="0" y="0" w="10" h="20"/><fillstroke/></foreground></shape>"#;
        let dot = r#"<shape name="dot" w="1" h="1"/>"#;
        write(
            "a/b.xml",
            &format!(
                "<shapes name=\"mxGraph.a.b\">\n{big}\n<!-- <shape name=\"x\"> -->\n{dot}\n</shapes>\n"
            ),
        );
        write(
            "w.xml",
            r#"<stencils><shapes name="mxgraph.w"><shape name="x"/></shapes></stencils>"#,
        );
        write(
            "bad.xml",
            r#"<shapes name="mxgraph.bad"><shape name="x"></shapes>"#,
        );

        let mut library = Library::new(dir.clone());
        assert_eq!(library.source("mxgraph.a.b.big_box").as_deref(), Some(big));
        assert_eq!(library.source("mxgraph.a.b.dot").as_deref(), Some(dot));
        assert_eq!(
            library.source("mxgraph.a.b.Big Box"),
            None,
            "only as registered"
        );
        assert_eq!(
            library.source("mxgraph.w.x").as_deref(),
            Some(r#"<shape name="x"/>"#),
            "a set in <stencils>"
        );
        assert_eq!(library.source("mxgraph.bad.x"), None, "a malformed file");
        assert_eq!(library.source("mxgraph.none.x"), None, "a missing file");
        assert_eq!(library.source("rect"), None);

        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            Library::new(dir).source("mxgraph.a.b.dot"),
            None,
            "no folder"
        );
    }
}
