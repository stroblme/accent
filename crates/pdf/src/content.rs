//! Appearance streams: the operators an annotation's drawing and its text need (ISO 32000-1
//! §8.4–8.5, §9.3–9.4), and the form XObject that holds them.

use crate::write::{real, string};
use crate::{Dict, Object, Stream};

/// A content stream, an operator at a time.
#[derive(Debug, Default)]
pub struct Content(Vec<u8>);

impl Content {
    fn op(&mut self, operands: &[f32], op: &str) -> &mut Self {
        for v in operands {
            self.0.extend(real(f64::from(*v)).bytes());
            self.0.push(b' ');
        }
        self.0.extend(op.bytes());
        self.0.push(b'\n');
        self
    }

    /// `q`: keep the graphics state.
    pub fn save(&mut self) -> &mut Self {
        self.op(&[], "q")
    }

    /// `Q`: back to the state kept.
    pub fn restore(&mut self) -> &mut Self {
        self.op(&[], "Q")
    }

    /// `cm`: the matrix `[a b c d e f]` before the current one.
    pub fn transform(&mut self, m: [f32; 6]) -> &mut Self {
        self.op(&m, "cm")
    }

    /// `w`: the line width.
    pub fn width(&mut self, w: f32) -> &mut Self {
        self.op(&[w], "w")
    }

    /// `J`: 0 butt, 1 round, 2 square caps.
    pub fn cap(&mut self, style: u8) -> &mut Self {
        self.op(&[f32::from(style)], "J")
    }

    /// `j`: 0 miter, 1 round, 2 bevel joins.
    pub fn join(&mut self, style: u8) -> &mut Self {
        self.op(&[f32::from(style)], "j")
    }

    /// `RG`: the stroking colour.
    pub fn stroke_rgb(&mut self, [r, g, b]: [f32; 3]) -> &mut Self {
        self.op(&[r, g, b], "RG")
    }

    /// `rg`: the filling colour.
    pub fn fill_rgb(&mut self, [r, g, b]: [f32; 3]) -> &mut Self {
        self.op(&[r, g, b], "rg")
    }

    pub fn move_to(&mut self, (x, y): (f32, f32)) -> &mut Self {
        self.op(&[x, y], "m")
    }

    pub fn line_to(&mut self, (x, y): (f32, f32)) -> &mut Self {
        self.op(&[x, y], "l")
    }

    /// `c`: a cubic through two control points to `end`.
    pub fn curve_to(&mut self, c1: (f32, f32), c2: (f32, f32), end: (f32, f32)) -> &mut Self {
        self.op(&[c1.0, c1.1, c2.0, c2.1, end.0, end.1], "c")
    }

    /// `h`: close the subpath.
    pub fn close(&mut self) -> &mut Self {
        self.op(&[], "h")
    }

    /// `S`: stroke the path.
    pub fn stroke(&mut self) -> &mut Self {
        self.op(&[], "S")
    }

    /// `f`: fill the path.
    pub fn fill(&mut self) -> &mut Self {
        self.op(&[], "f")
    }

    /// `gs`: the graphics state the resources name `name` (opacity, a blend mode).
    pub fn state(&mut self, name: &[u8]) -> &mut Self {
        self.0.push(b'/');
        self.0.extend(name);
        self.op(&[], " gs")
    }

    /// `BT`: a text object begins, its lines set from the origin.
    pub fn begin_text(&mut self) -> &mut Self {
        self.op(&[], "BT")
    }

    /// `ET`: the text object ends.
    pub fn end_text(&mut self) -> &mut Self {
        self.op(&[], "ET")
    }

    /// `Tf`: the font the resources name `name`, at `size`.
    pub fn font(&mut self, name: &[u8], size: f32) -> &mut Self {
        self.0.push(b'/');
        self.0.extend(name);
        self.0.push(b' ');
        self.op(&[size], "Tf")
    }

    /// `Td`: the next line starts this far from where the last one did.
    pub fn next_line(&mut self, (x, y): (f32, f32)) -> &mut Self {
        self.op(&[x, y], "Td")
    }

    /// `Tj`: `text`, in the font's encoding, set from where the line is.
    pub fn show(&mut self, text: &[u8]) -> &mut Self {
        string(&mut self.0, text);
        self.op(&[], " Tj")
    }

    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

/// A form XObject of `content` drawn in `bbox`, as an annotation's `/AP /N` is.
pub fn form(bbox: [f32; 4], content: Vec<u8>, resources: Dict) -> Stream {
    let bbox = bbox.iter().map(|v| Object::Real(f64::from(*v))).collect();
    Stream {
        dict: Dict(vec![
            (b"Type".to_vec(), Object::Name(b"XObject".to_vec())),
            (b"Subtype".to_vec(), Object::Name(b"Form".to_vec())),
            (b"BBox".to_vec(), Object::Array(bbox)),
            (b"Resources".to_vec(), Object::Dict(resources)),
        ]),
        raw: content,
    }
}
