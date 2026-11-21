// Derived from draw.io js/grapheditor/Sidebar.js (addGeneralPalette) and js/diagramly/sidebar/Sidebar-Basic.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! draw.io's General and Basic palettes, the shapes the diagram ring's Shape tool draws: each
//! entry's title, style and size as draw.io's sidebar inserts it
//! (`Sidebar.createVertexTemplateEntry`), so a new cell is the one draw.io's would be. The edges,
//! containers, lists and text boxes are left out, and so are the shapes the ring's own tools
//! draw: the rectangle, the ellipse, their square and circle, and the Note.

use crate::{Style, shapes};

/// A palette entry: what draw.io's sidebar calls it, the style a new cell takes and its size.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub title: &'static str,
    pub style: &'static str,
    pub width: u16,
    pub height: u16,
}

impl Entry {
    /// Whether accent draws it rather than its dashed stand-in: a shape ported
    /// ([`shapes::KNOWN`]), or a library stencil whose pack is installed (`make stencils`).
    pub fn drawn(&self) -> bool {
        let style = Style::parse(self.style).resolve(false);
        let shape = style.shape();
        shapes::is_known(shape) || shapes::library::source(shape).is_some()
    }
}

/// The palettes under their titles, in draw.io's order.
pub const PALETTES: [(&str, &[Entry]); 2] = [("General", GENERAL), ("Basic", BASIC)];

/// What the Shape tool draws until another is picked: the Diamond, a flowchart's decision.
pub const DEFAULT: &Entry = &GENERAL[1];

/// The entry whose style is `style`, which names one alone, as titles do not.
pub fn find(style: &str) -> Option<&'static Entry> {
    let entries = PALETTES.iter().flat_map(|(_, entries)| entries.iter());
    entries.into_iter().find(|entry| entry.style == style)
}

const fn entry(title: &'static str, style: &'static str, width: u16, height: u16) -> Entry {
    Entry {
        title,
        style,
        width,
        height,
    }
}

#[rustfmt::skip]
const GENERAL: &[Entry] = &[
    entry("Process", "shape=process;whiteSpace=wrap;html=1;backgroundOutline=1;", 120, 60),
    entry("Diamond", "rhombus;whiteSpace=wrap;html=1;shapeInside=1;", 80, 80),
    entry("Parallelogram", "shape=parallelogram;perimeter=parallelogramPerimeter;whiteSpace=wrap;html=1;shapeInside=1;fixedSize=1;", 120, 60),
    entry("Hexagon", "shape=hexagon;perimeter=hexagonPerimeter2;whiteSpace=wrap;html=1;shapeInside=1;fixedSize=1;", 120, 80),
    entry("Triangle", "triangle;whiteSpace=wrap;html=1;shapeInside=1;", 60, 80),
    entry("Cylinder", "shape=cylinder3;whiteSpace=wrap;html=1;boundedLbl=1;backgroundOutline=1;size=15;", 60, 80),
    entry("Cloud", "ellipse;shape=cloud;whiteSpace=wrap;html=1;", 120, 80),
    entry("Document", "shape=document;whiteSpace=wrap;html=1;boundedLbl=1;", 120, 80),
    entry("Internal Storage", "shape=internalStorage;whiteSpace=wrap;html=1;backgroundOutline=1;", 80, 80),
    entry("Cube", "shape=cube;whiteSpace=wrap;html=1;boundedLbl=1;backgroundOutline=1;darkOpacity=0.05;darkOpacity2=0.1;", 120, 80),
    entry("Step", "shape=step;perimeter=stepPerimeter;whiteSpace=wrap;html=1;shapeInside=1;fixedSize=1;", 120, 80),
    entry("Trapezoid", "shape=trapezoid;perimeter=trapezoidPerimeter;whiteSpace=wrap;html=1;shapeInside=1;fixedSize=1;", 120, 60),
    entry("Tape", "shape=tape;whiteSpace=wrap;html=1;", 120, 100),
    entry("Card", "shape=card;whiteSpace=wrap;html=1;", 80, 100),
    entry("Callout", "shape=callout;whiteSpace=wrap;html=1;perimeter=calloutPerimeter;", 120, 80),
    entry("Wedge Callout", "shape=wedgeCallout;whiteSpace=wrap;html=1;", 120, 80),
    entry("Actor", "shape=umlActor;verticalLabelPosition=bottom;verticalAlign=top;html=1;outlineConnect=0;", 30, 60),
    entry("Or", "shape=xor;whiteSpace=wrap;html=1;shapeInside=1;", 60, 80),
    entry("And", "shape=or;whiteSpace=wrap;html=1;shapeInside=1;", 60, 80),
    entry("Data Storage", "shape=dataStorage;whiteSpace=wrap;html=1;shapeInside=1;fixedSize=1;", 100, 80),
];

#[rustfmt::skip]
const BASIC: &[Entry] = &[
    entry("Partial Rectangle", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.rect;fillColor2=none;strokeWidth=1;size=20;indent=5;", 120, 60),
    entry("Partial Rectangle", "shape=partialRectangle;whiteSpace=wrap;html=1;top=0;bottom=0;fillColor=none;", 120, 60),
    entry("Partial Rectangle", "shape=partialRectangle;whiteSpace=wrap;html=1;right=0;top=0;bottom=0;fillColor=none;routingCenterX=-0.5;", 120, 60),
    entry("Partial Rectangle", "shape=partialRectangle;whiteSpace=wrap;html=1;bottom=0;right=0;fillColor=none;", 120, 60),
    entry("Partial Rectangle", "shape=partialRectangle;whiteSpace=wrap;html=1;top=0;left=0;fillColor=none;", 120, 60),
    entry("Polygon", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.polygon;polyCoords=[[0.25,0],[0.75,0],[1,0.25],[1,0.75],[0.75,1],[0.25,1],[0,0.75],[0,0.25]];polyline=0;", 100, 100),
    entry("Polyline", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.polygon;polyCoords=[[0.25,0],[0.75,0],[1,0.25],[1,0.75],[0.75,1],[0.25,1],[0,0.75],[0,0.25]];polyline=1;fillColor=none;", 100, 100),
    entry("Rectangle with diagonal fill", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.patternFillRect;fillStyle=diag;step=5;fillStrokeWidth=0.2;fillStrokeColor=#dddddd;", 120, 60),
    entry("Rectangle with reverse diagonal fill", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.patternFillRect;fillStyle=diagRev;step=5;fillStrokeWidth=0.2;fillStrokeColor=#dddddd;", 120, 60),
    entry("Rectangle with vertical fill", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.patternFillRect;fillStyle=vert;step=5;fillStrokeWidth=0.2;fillStrokeColor=#dddddd;", 120, 60),
    entry("Rectangle with horizontal fill", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.patternFillRect;fillStyle=hor;step=5;fillStrokeWidth=0.2;fillStrokeColor=#dddddd;", 120, 60),
    entry("Rectangle with grid fill", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.patternFillRect;fillStyle=grid;step=5;fillStrokeWidth=0.2;fillStrokeColor=#dddddd;", 120, 60),
    entry("Rectangle with diagonal grid fill", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.patternFillRect;fillStyle=diagGrid;step=5;fillStrokeWidth=0.2;fillStrokeColor=#dddddd;", 120, 60),
    entry("4 Point Star", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.4_point_star_2;dx=0.8;", 100, 100),
    entry("6 Point Star", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.6_point_star", 100, 90),
    entry("8 Point Star", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.8_point_star", 100, 100),
    entry("Banner", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.banner", 100, 50),
    entry("Cloud Callout", "whiteSpace=wrap;html=1;shape=mxgraph.basic.cloud_callout", 90, 60),
    entry("Cloud Rectangle", "whiteSpace=wrap;html=1;shape=mxgraph.basic.cloud_rect", 120, 90),
    entry("Cone", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.cone", 100, 100),
    entry("Cone (adjustable)", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.cone2;dx=0.5;dy=0.9;", 100, 100),
    entry("Document", "whiteSpace=wrap;html=1;shape=mxgraph.basic.document", 100, 100),
    entry("Donut", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.donut;dx=25;", 100, 100),
    entry("Drop", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.drop", 70, 100),
    entry("Flash", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.flash", 60, 100),
    entry("Half Circle", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.half_circle", 100, 50),
    entry("Heart", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.heart", 100, 100),
    entry("Isometric Cube", "html=1;shape=mxgraph.basic.isocube;isoAngle=15;", 100, 100),
    entry("Loud Callout", "whiteSpace=wrap;html=1;shape=mxgraph.basic.loud_callout", 100, 60),
    entry("Moon", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.moon", 75, 100),
    entry("No Symbol", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.no_symbol", 100, 100),
    entry("Octagon", "whiteSpace=wrap;html=1;shape=mxgraph.basic.octagon2;align=center;verticalAlign=middle;dx=15;", 100, 100),
    entry("Orthogonal Triangle", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.orthogonal_triangle", 100, 70),
    entry("Acute Triangle", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.acute_triangle;dx=0.5;", 100, 70),
    entry("Obtuse Triangle", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.obtuse_triangle;dx=0.25;", 100, 70),
    entry("Oval Callout", "whiteSpace=wrap;html=1;shape=mxgraph.basic.oval_callout", 100, 60),
    entry("Pentagon", "whiteSpace=wrap;html=1;shape=mxgraph.basic.pentagon", 100, 90),
    entry("Pointed Oval", "whiteSpace=wrap;html=1;shape=mxgraph.basic.pointed_oval", 50, 100),
    entry("Pyramid", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.pyramid;dx1=0.4;dx2=0.6;dy1=0.9;dy2=0.8;", 100, 100),
    entry("Diagonal Snip Rectangle", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.diag_snip_rect;dx=6;whiteSpace=wrap;", 100, 60),
    entry("Diagonal Rounded Rectangle", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.diag_round_rect;dx=6;whiteSpace=wrap;", 100, 60),
    entry("Corner Rounded Rectangle", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.corner_round_rect;dx=6;whiteSpace=wrap;", 100, 60),
    entry("Rounded Rectangle (three corners)", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.three_corner_round_rect;dx=6;whiteSpace=wrap;", 100, 60),
    entry("Plaque", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.plaque;dx=6;whiteSpace=wrap;", 100, 60),
    entry("Frame", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.frame;dx=10;whiteSpace=wrap;", 100, 60),
    entry("Rounded Frame", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.rounded_frame;dx=10;whiteSpace=wrap;", 100, 60),
    entry("Plaque Frame", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.plaque_frame;dx=10;whiteSpace=wrap;", 100, 60),
    entry("Frame Corner", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.frame_corner;dx=10;whiteSpace=wrap;", 100, 60),
    entry("Diagonal Stripe", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.diag_stripe;dx=10;", 100, 60),
    entry("Rectangular Callout", "whiteSpace=wrap;html=1;shape=mxgraph.basic.rectCallout;dx=30;dy=15;boundedLbl=1;", 100, 60),
    entry("Rounded Rectangular Callout", "whiteSpace=wrap;html=1;shape=mxgraph.basic.roundRectCallout;dx=30;dy=15;size=5;boundedLbl=1;", 100, 60),
    entry("Layered Rectangle", "whiteSpace=wrap;html=1;shape=mxgraph.basic.layered_rect;dx=10;outlineConnect=0;boundedLbl=1;", 100, 60),
    entry("Smiley", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.smiley", 100, 100),
    entry("Star", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.star", 100, 95),
    entry("Sun", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.sun", 100, 100),
    entry("Tick", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.tick", 85, 100),
    entry("Wave", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.wave2;dy=0.3;", 100, 60),
    entry("Button", "labelPosition=center;verticalLabelPosition=middle;align=center;html=1;shape=mxgraph.basic.button;dx=10;whiteSpace=wrap;", 100, 60),
    entry("Button (shaded)", "labelPosition=center;verticalLabelPosition=middle;align=center;html=1;shape=mxgraph.basic.shaded_button;dx=10;fillColor=#E6E6E6;strokeColor=none;whiteSpace=wrap;", 100, 60),
    entry("X", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.x", 100, 100),
    entry("Pie", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.pie;startAngle=0.2;endAngle=0.9;", 100, 100),
    entry("Arc", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.arc;startAngle=0.3;endAngle=0.1;", 100, 100),
    entry("Partial Concentric Ellipse", "verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.partConcEllipse;startAngle=0.25;endAngle=0.1;arcWidth=0.5;", 100, 100),
    entry("Message", "shape=message;html=1;html=1;outlineConnect=0;labelPosition=center;verticalLabelPosition=bottom;align=center;verticalAlign=top;", 60, 40),
    entry("Cylinder Stack", "shape=cylinder3;whiteSpace=wrap;html=1;boundedLbl=1;backgroundOutline=1;size=15;lid=0;", 60, 80),
    entry("Bang", "shape=mindmapBang;whiteSpace=wrap;html=1;perimeter=ellipsePerimeter;", 140, 80),
    entry("Tag", "shape=gitTag;tabSize=8;tabInset=4;holeSize=1;whiteSpace=wrap;html=1;", 80, 20),
    entry("Smiley", "shape=smileyFace;aspect=fixed;smileyType=happy;perimeter=ellipsePerimeter;fillColor=#FFF8DC;strokeColor=#999999;strokeWidth=2;whiteSpace=wrap;html=1;", 40, 40),
    entry("Neutral Smiley", "shape=smileyFace;aspect=fixed;smileyType=neutral;perimeter=ellipsePerimeter;fillColor=#FFF8DC;strokeColor=#999999;strokeWidth=2;whiteSpace=wrap;html=1;", 40, 40),
    entry("Sad Smiley", "shape=smileyFace;aspect=fixed;smileyType=sad;perimeter=ellipsePerimeter;fillColor=#FFF8DC;strokeColor=#999999;strokeWidth=2;whiteSpace=wrap;html=1;", 40, 40),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_entry_is_found_by_its_style_alone() {
        for (_, entries) in PALETTES {
            for entry in entries {
                assert_eq!(find(entry.style), Some(entry), "{}", entry.title);
            }
        }
        assert_eq!(DEFAULT.title, "Diamond");
        assert_eq!(find("rhombus;"), None);
    }

    /// Every General entry is a shape accent has ported, and of the Basic ones those ported so far.
    #[test]
    fn the_ported_shapes_are_drawn() {
        assert!(GENERAL.iter().all(Entry::drawn));
        let drawn = |title: &str| BASIC.iter().any(|e| e.title == title && e.drawn());
        assert!(drawn("Diagonal Rounded Rectangle") && drawn("Message"));
        assert!(!drawn("Bang"));
    }
}
