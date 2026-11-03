//! Strokes, texts and pictures taken off a page as plain geometry, which is what a copy carries
//! to the clipboard and a paste or a duplicate draws again as ours ([`PdfDoc::clip`] reads one).

use accent_pdf::Ref;
use serde::{Deserialize, Serialize};

use super::ink::{Matrix, apply, polyline, then};
use super::picture::{Picture, picture_bounds};
use super::typed::Typed;
use super::{InkStyle, PdfDoc, Rect};
use crate::{Error, Result};

/// One stroke, text or picture as a copy holds it, in top-left page points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Clip {
    /// A stroke, drawn straight between its points.
    Ink {
        points: Vec<(f32, f32)>,
        style: InkStyle,
    },
    /// A text of ours.
    Text(Typed),
    /// A picture, and where its unit square goes on the page (`PlacedPicture::place`).
    Image { place: Matrix, picture: Picture },
}

impl Clip {
    /// The box it covers on the page.
    pub fn bounds(&self) -> Rect {
        match self {
            Clip::Ink { points, style } => (points.iter())
                .map(|&p| Rect::from_corners(p, p))
                .reduce(Rect::union)
                .unwrap_or(Rect::ZERO)
                .grow(style.width / 2.0),
            Clip::Text(typed) => typed.bounds(),
            Clip::Image { place, .. } => picture_bounds(*place),
        }
    }

    /// The same taken by `m`: a stroke's every point, a text's corner and a picture whole, as a
    /// move takes them. A picture's bytes are copied with it.
    pub fn under(&self, m: Matrix) -> Clip {
        match self {
            Clip::Ink { points, style } => Clip::Ink {
                points: points.iter().map(|&p| apply(m, p)).collect(),
                style: *style,
            },
            Clip::Text(typed) => Clip::Text(Typed {
                at: apply(m, typed.at),
                ..typed.clone()
            }),
            Clip::Image { place, picture } => Clip::Image {
                place: then(*place, m),
                picture: picture.clone(),
            },
        }
    }
}

impl PdfDoc {
    /// `clips` taken by `m` and drawn onto `page` as strokes, texts and pictures of ours, at the
    /// end of its `/Annots` in their order, the document opened again once: the box they cover,
    /// and their objects in that order. A picture this document already holds shares its image.
    pub fn paste(&mut self, page: usize, clips: &[Clip], m: Matrix) -> Result<(Rect, Vec<Ref>)> {
        if clips.is_empty() {
            return Err(Error::Invalid("nothing to paste".to_string()));
        }
        let (_, height) = self.page_size(page)?;
        let mut area: Option<Rect> = None;
        let mut objects = Vec::with_capacity(clips.len());
        for clip in clips {
            let (covers, at) = match clip {
                Clip::Ink { points, style } => {
                    let points: Vec<_> = points.iter().map(|&p| apply(m, p)).collect();
                    (self.cos).put_ink(page, height, &polyline(&points), *style, None)?
                }
                Clip::Text(typed) => {
                    let moved = Typed {
                        at: apply(m, typed.at),
                        ..typed.clone()
                    };
                    self.cos.put_text(page, height, &moved, None)?
                }
                // Moved by its place alone, its bytes not copied.
                Clip::Image { place, picture } => {
                    let image = self.cos.put_picture(picture);
                    (self.cos).put_stamp(page, height, image, then(*place, m), None)?
                }
            };
            area = Some(area.map_or(covers, |a| a.union(covers)));
            objects.push(at);
        }
        self.reopen()?;
        Ok((area.unwrap_or(Rect::ZERO), objects))
    }
}
