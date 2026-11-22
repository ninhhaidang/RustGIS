//! Picture markers' images in the interface's previews (#2266): the Layers pane's patches and
//! legend, the symbol editor's swatch, and the stroke and fill editors' previews. Each image is
//! decoded once into a small texture kept with the egui context, so the painters that draw them
//! need nothing but their `Painter`. An image the map draws as a circle gives none, and the
//! preview draws the circle.

use std::collections::HashMap;

use egui::{Color32, Pos2, Rect, TextureHandle, TextureOptions, pos2, vec2};
use rustgis_engine::map::{Marker, Picture};

/// Thumbnails' most pixels per side: sharp in the panes' patches (at most 14 points across, at two
/// pixels a point) and the editors' usual sizes; softer where an editor draws a marker larger.
const SIDE: u32 = 96;
/// Where they're kept in the context's data.
const KEPT: &str = "picture-marker-images";

/// The thumbnails kept, by picture (where its image is kept, so a hit compares no bytes); `None`
/// for one that doesn't decode, so it isn't tried again each frame. Never emptied: a run keeps at
/// most `MAX_PICTURES` (1024) pictures, so this holds no more than that many textures of at most
/// 96 × 96 pixels (37 MB at worst), and none is decoded twice; clearing at a cap would decode a
/// picture in sight again every frame.
#[derive(Clone, Default)]
struct Kept(HashMap<Picture, Option<Image>>);

/// A picture marker's thumbnail, ready to paint.
#[derive(Clone)]
pub(crate) struct Image {
    texture: TextureHandle,
    /// The image's height for its width.
    aspect: f32,
}

impl Image {
    /// Paints the image whole within the `side` × `side` square centred on `at`, in its
    /// proportions: its longer side is `side` (the map draws it `side` wide, but a tall image
    /// would leave a legend's row). Not turned: previews show no rotation.
    pub(crate) fn paint(&self, p: &egui::Painter, at: Pos2, side: f32) {
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        p.image(self.texture.id(), fitted(at, side, self.aspect), uv, Color32::WHITE);
    }
}

/// The rectangle of an image of height `aspect` for its width, whole within the `side` square
/// centred on `at`.
fn fitted(at: Pos2, side: f32, aspect: f32) -> Rect {
    let size = if aspect <= 1.0 { vec2(side, side * aspect) } else { vec2(side / aspect, side) };
    Rect::from_center_size(at, size)
}

/// Marker `m`'s thumbnail when it is a picture the map draws, decoded the first time it's asked
/// for; `None` for any other marker, which its preview draws as it always has, and for an image
/// the map draws as a circle (none, one a file gave that wasn't kept, or one that doesn't decode).
pub(crate) fn image(ctx: &egui::Context, m: Marker) -> Option<Image> {
    let Marker::Picture(pic) = m else { return None };
    if pic.is_default() {
        return None;
    }
    let id = egui::Id::unique(KEPT);
    if let Some(kept) = ctx.data_mut(|d| d.get_temp_mut_or_default::<Kept>(id).0.get(&pic).cloned()) {
        return kept;
    }
    // Scaled by averaging from the copy render decodes once for the run (so it doesn't alias, and
    // the image isn't decoded again); once a run for an icon. Loaded outside `data_mut`: the
    // context's data is locked inside it.
    let image = rustgis_engine::render::picture_marker_image(pic, SIDE).map(|(pm, aspect)| {
        let pixels = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
        Image { texture: ctx.load_texture("picture-marker", pixels, TextureOptions::LINEAR), aspect }
    });
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Kept>(id).0.insert(pic, image.clone());
    });
    image
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn png(w: u32, h: u32) -> Picture {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(w, h, image::Rgba([10, 120, 230, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        Picture::new(&base64::engine::general_purpose::STANDARD.encode(&png))
    }

    /// An image fits the preview's square in its proportions, wide or tall.
    #[test]
    fn an_image_fits_the_square_in_its_proportions() {
        let at = pos2(50.0, 20.0);
        assert_eq!(fitted(at, 14.0, 0.5), Rect::from_center_size(at, vec2(14.0, 7.0)));
        assert_eq!(fitted(at, 14.0, 4.0), Rect::from_center_size(at, vec2(3.5, 14.0)));
        assert_eq!(fitted(at, 14.0, 1.0), Rect::from_center_size(at, vec2(14.0, 14.0)));
    }

    /// An image is decoded once into a texture its every preview shares; one the map draws as a
    /// circle gives none, and is remembered as such; another marker gives none.
    #[test]
    fn images_are_kept_once_and_circles_give_none() {
        let ctx = egui::Context::default();
        let wide = png(40, 10);
        let first = image(&ctx, Marker::Picture(wide)).unwrap();
        assert_eq!(first.aspect, 0.25);
        assert_eq!(first.texture.size(), [40, 10]);
        assert_eq!(image(&ctx, Marker::Picture(wide)).unwrap().texture.id(), first.texture.id());
        let dropped = Picture::read(&"A".repeat(rustgis_engine::map::symbology::MAX_PICTURE_BYTES + 4));
        for circle in [Picture::default(), dropped, Picture::new("bm90IGFuIGltYWdl")] {
            assert!(image(&ctx, Marker::Picture(circle)).is_none());
        }
        let kept = |ctx: &egui::Context| ctx.data_mut(|d| d.get_temp_mut_or_default::<Kept>(egui::Id::unique(KEPT)).0.len());
        // The decoded image and the broken one are kept; no image and the dropped one never enter.
        assert_eq!(kept(&ctx), 2);
        assert!(image(&ctx, Marker::Square).is_none());
    }
}
