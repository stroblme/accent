//! The style keys the renderer and the editor read, and the values they know where those are a
//! few names: what the style editor completes (DESIGN.md, Diagram).

/// A switch, as [`Resolved::flag`](super::Resolved::flag) reads it.
const FLAG: &[&str] = &["0", "1"];
const ALIGN: &[&str] = &["left", "center", "right"];
const VALIGN: &[&str] = &["top", "middle", "bottom"];
const DIRECTION: &[&str] = &["north", "east", "south", "west"];
/// `fontStyle`'s bits, added together ([`about`]).
const FONT_STYLE: &[&str] = &["0", "1", "2", "4", "8"];

/// mxGraph's edge styles (mxStyleRegistry.js 62-68), as `route` reads them.
pub const EDGE_STYLES: &[&str] = &[
    "orthogonalEdgeStyle",
    "elbowEdgeStyle",
    "entityRelationEdgeStyle",
    "segmentEdgeStyle",
    "loopEdgeStyle",
    "sideToSideEdgeStyle",
    "topToBottomEdgeStyle",
    "none",
];

/// The perimeters `perimeter` reads (mxStyleRegistry.js 70-74 and Shapes.js).
pub const PERIMETERS: &[&str] = &[
    "rectanglePerimeter",
    "ellipsePerimeter",
    "rhombusPerimeter",
    "trianglePerimeter",
    "hexagonPerimeter",
    "hexagonPerimeter2",
    "parallelogramPerimeter",
    "trapezoidPerimeter",
    "stepPerimeter",
    "calloutPerimeter",
    "lifelinePerimeter",
    "orthogonalPerimeter",
    "backbonePerimeter",
    "centerPerimeter",
];

/// The heads `marker` draws (mxMarker.js, Shapes.js and mxER.js).
pub const ARROWS: &[&str] = &[
    "none",
    "classic",
    "classicThin",
    "block",
    "blockThin",
    "open",
    "openThin",
    "oval",
    "diamond",
    "diamondThin",
    "halfCircle",
    "doubleBlock",
    "box",
    "circle",
    "circlePlus",
    "async",
    "openAsync",
    "dash",
    "cross",
    "baseDash",
    "mermaidExtension",
    "mermaidDiamond",
    "manyOptional",
    "ERone",
    "ERmandOne",
    "ERmany",
    "ERoneToMany",
    "ERzeroToOne",
    "ERzeroToMany",
];

/// The named styles a bare token brings in (draw.io's `styles/default.xml`).
pub const NAMED: &[&str] = &[
    "text",
    "edgeLabel",
    "label",
    "group",
    "ellipse",
    "image",
    "rhombus",
    "triangle",
    "swimlane",
    "line",
    "arrow",
];

/// Every key, with the values it knows where those are a few names.
pub const KEYS: &[(&str, &[&str])] = &[
    ("shape", crate::shapes::KNOWN),
    ("perimeter", PERIMETERS),
    ("perimeterSpacing", &[]),
    ("fillColor", &["none", "default"]),
    ("gradientColor", &["none", "default"]),
    (
        "gradientDirection",
        &["north", "east", "south", "west", "radial"],
    ),
    ("fillOpacity", &[]),
    ("opacity", &[]),
    ("strokeColor", &["none", "default"]),
    ("strokeWidth", &[]),
    ("strokeOpacity", &[]),
    ("dashed", FLAG),
    ("dashPattern", &[]),
    ("fixDash", FLAG),
    ("rounded", FLAG),
    ("arcSize", &[]),
    ("absoluteArcSize", FLAG),
    ("shadow", FLAG),
    ("sketch", FLAG),
    ("comic", FLAG),
    ("jiggle", &[]),
    ("rotation", &[]),
    ("flipH", FLAG),
    ("flipV", FLAG),
    ("direction", DIRECTION),
    ("aspect", &["fixed"]),
    ("horizontal", FLAG),
    ("image", &[]),
    ("imageAspect", FLAG),
    ("imageBackground", &[]),
    ("imageBorder", &[]),
    ("imageWidth", &[]),
    ("imageHeight", &[]),
    ("clipPath", &[]),
    ("size", &[]),
    ("fixedSize", FLAG),
    ("double", FLAG),
    ("base", &[]),
    ("dx", &[]),
    ("dy", &[]),
    ("position", &[]),
    ("position2", &[]),
    ("tabWidth", &[]),
    ("tabHeight", &[]),
    ("tabPosition", &["left", "right"]),
    ("folderSymbol", &[]),
    ("jettyWidth", &[]),
    ("jettyHeight", &[]),
    ("startSize", &[]),
    ("endSize", &[]),
    ("swimlaneLine", FLAG),
    ("swimlaneFillColor", &["none", "default"]),
    ("swimlaneHead", FLAG),
    ("swimlaneBody", FLAG),
    ("separatorColor", &["none"]),
    ("fixedHeader", FLAG),
    ("footerSize", &[]),
    ("labelInHeader", FLAG),
    ("lid", FLAG),
    ("backboneSize", &[]),
    ("boundedLbl", FLAG),
    ("width", &[]),
    ("startWidth", &[]),
    ("endWidth", &[]),
    ("margin", &[]),
    ("points", &[]),
    ("legacyAnchorPoints", FLAG),
    ("fontColor", &["none", "default"]),
    ("fontSize", &[]),
    (
        "fontFamily",
        &["Helvetica", "Times New Roman", "Courier New", "Verdana"],
    ),
    ("fontStyle", FONT_STYLE),
    ("align", ALIGN),
    ("verticalAlign", VALIGN),
    ("labelPosition", ALIGN),
    ("verticalLabelPosition", VALIGN),
    ("labelBackgroundColor", &["none", "default"]),
    ("labelBorderColor", &["none", "default"]),
    ("textOpacity", &[]),
    ("html", FLAG),
    ("whiteSpace", &["wrap"]),
    ("spacing", &[]),
    ("spacingLeft", &[]),
    ("spacingRight", &[]),
    ("spacingTop", &[]),
    ("spacingBottom", &[]),
    ("noLabel", FLAG),
    ("edgeStyle", EDGE_STYLES),
    ("elbow", &["horizontal", "vertical"]),
    ("curved", FLAG),
    ("bezier", FLAG),
    ("orthogonal", FLAG),
    ("orthogonalLoop", FLAG),
    ("noEdgeStyle", FLAG),
    ("loop", EDGE_STYLES),
    ("startArrow", ARROWS),
    ("endArrow", ARROWS),
    ("startFill", FLAG),
    ("endFill", FLAG),
    ("sourcePort", &[]),
    ("targetPort", &[]),
    ("portConstraint", DIRECTION),
    ("portConstraintRotation", FLAG),
    ("exitX", &[]),
    ("exitY", &[]),
    ("exitDx", &[]),
    ("exitDy", &[]),
    ("exitPerimeter", FLAG),
    ("entryX", &[]),
    ("entryY", &[]),
    ("entryDx", &[]),
    ("entryDy", &[]),
    ("entryPerimeter", FLAG),
    ("routingCenterX", &[]),
    ("routingCenterY", &[]),
    ("jettySize", &["auto"]),
    ("sourcePerimeterSpacing", &[]),
    ("targetPerimeterSpacing", &[]),
    ("segment", &[]),
    ("locked", FLAG),
    ("container", FLAG),
    ("transparentBounds", FLAG),
    ("selectParentFirst", FLAG),
    ("pointerEvents", FLAG),
    ("movable", FLAG),
    ("rotatable", FLAG),
    ("bendable", FLAG),
];

/// What a key's values mean where its value list alone does not say.
pub fn about(key: &str) -> Option<&'static str> {
    match key {
        "fontStyle" => Some("1 bold, 2 italic, 4 underline, 8 strikethrough, added together"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;

    #[test]
    fn every_value_offered_is_one_the_renderer_reads() {
        for name in NAMED {
            assert!(crate::style::named_style(name).is_some(), "{name}");
        }
        for name in PERIMETERS.iter().filter(|p| **p != "rectanglePerimeter") {
            let kind = crate::perimeter::PerimeterKind::named(Some(name));
            assert_ne!(kind, crate::perimeter::PerimeterKind::Rectangle, "{name}");
        }
        // The edge styles are checked where they are routed (`route::tests`).
        for name in ARROWS.iter().filter(|a| **a != "none") {
            let mut end = Point::new(0.0, 0.0);
            let unit = Point::new(1.0, 0.0);
            let head = crate::marker::marker(name, &mut end, unit, 6.0, 1.0, false, None);
            assert!(head.is_some(), "{name}");
        }
        let mut keys: Vec<&str> = KEYS.iter().map(|(k, _)| *k).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), KEYS.len(), "a key listed twice");
    }
}
