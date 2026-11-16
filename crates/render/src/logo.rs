//! The RustGIS logo, drawn in code: a rust-coloured cog framing a globe split into four
//! segments by white gaps, with a tilted orbit. The same design is published as SVG in
//! rustgis-assets; this keeps the app free of image files.

use tiny_skia::{
    BlendMode, Color, FillRule, GradientStop, LineCap, LinearGradient, Mask, Paint, PathBuilder, Pixmap, Point, SpreadMode, Stroke,
    Transform,
};

const C: f32 = 256.0;

fn gradient(a: (u8, u8, u8), b: (u8, u8, u8), x0: f32, y0: f32, x1: f32, y1: f32) -> Paint<'static> {
    let mut p = Paint { anti_alias: true, ..Paint::default() };
    let stops =
        vec![GradientStop::new(0.0, Color::from_rgba8(a.0, a.1, a.2, 255)), GradientStop::new(1.0, Color::from_rgba8(b.0, b.1, b.2, 255))];
    match LinearGradient::new(Point::from_xy(x0, y0), Point::from_xy(x1, y1), stops, SpreadMode::Pad, Transform::identity()) {
        Some(sh) => p.shader = sh,
        None => p.set_color_rgba8(a.0, a.1, a.2, 255),
    }
    p
}

fn solid(r: u8, g: u8, b: u8, a: u8) -> Paint<'static> {
    let mut p = Paint { anti_alias: true, ..Paint::default() };
    p.set_color_rgba8(r, g, b, a);
    p
}

const RUST: ((u8, u8, u8), (u8, u8, u8)) = ((0xE4, 0x78, 0x3D), (0xA8, 0x3F, 0x24));

/// The logo as a premultiplied RGBA pixmap of `size`×`size` pixels (16..=1024).
pub fn pixmap(size: u32) -> Option<Pixmap> {
    draw(size, false)
}

/// The logo as one opaque white shape on a transparent background, `size`×`size` pixels
/// (16..=1024): its white parts (the globe's gaps, the ring around it, the orbit's outline) are
/// cut out, so it still reads as the logo in one colour. For the icons a phone tints itself:
/// Android's themed icons (the adaptive icon's monochrome layer) and iOS's tinted app icons.
pub fn monochrome(size: u32) -> Option<Pixmap> {
    draw(size, true)
}

fn draw(size: u32, mono: bool) -> Option<Pixmap> {
    let size = size.clamp(16, 1024);
    let mut pm = Pixmap::new(size, size)?;
    let t = Transform::from_scale(size as f32 / 512.0, size as f32 / 512.0);
    // In one colour, every colour is the shape and white cuts holes in it (as much as its alpha).
    // These closures shadow the module's `gradient` and `solid`, which they call by path
    // (`self::`) so that what they call doesn't depend on the order they are declared in.
    let gradient = |a, b, x0, y0, x1, y1| if mono { self::solid(255, 255, 255, 255) } else { self::gradient(a, b, x0, y0, x1, y1) };
    let solid = |r, g, b, a| {
        let mut p = self::solid(r, g, b, a);
        if mono {
            p.blend_mode = BlendMode::DestinationOut;
        }
        p
    };
    let rust = gradient(RUST.0, RUST.1, 0.0, 0.0, 512.0, 512.0);

    // Cog.
    let mut pb = PathBuilder::new();
    let teeth = 12;
    let step = std::f32::consts::TAU / teeth as f32;
    let mut first = true;
    for i in 0..teeth {
        let a = i as f32 * step - std::f32::consts::FRAC_PI_2;
        for (frac, r) in [(-0.5, 212.0), (-0.31, 212.0), (-0.21, 246.0), (0.21, 246.0), (0.31, 212.0)] {
            let ang = a + frac * step;
            let (x, y) = (C + r * ang.cos(), C + r * ang.sin());
            if first {
                pb.move_to(x, y);
                first = false;
            } else {
                pb.line_to(x, y);
            }
        }
    }
    pb.close();
    pm.fill_path(&pb.finish()?, &rust, FillRule::Winding, t, None);
    let white = solid(255, 255, 255, 255);
    pm.fill_path(&PathBuilder::from_circle(C, C, 184.0)?, &white, FillRule::Winding, t, None);

    // Globe segments, clipped to a circle.
    let mut mask = Mask::new(size, size)?;
    mask.fill_path(&PathBuilder::from_circle(C, C, 166.0)?, FillRule::Winding, true, t);
    let seg = |cmds: &dyn Fn(&mut PathBuilder)| -> Option<tiny_skia::Path> {
        let mut pb = PathBuilder::new();
        cmds(&mut pb);
        pb.close();
        pb.finish()
    };
    let blue = seg(&|pb| {
        pb.move_to(60.0, 60.0);
        pb.line_to(250.0, 60.0);
        pb.cubic_to(215.0, 150.0, 205.0, 230.0, 214.0, 262.0);
        pb.cubic_to(150.0, 250.0, 100.0, 262.0, 60.0, 282.0);
    })?;
    let teal = seg(&|pb| {
        pb.move_to(250.0, 60.0);
        pb.line_to(452.0, 60.0);
        pb.line_to(452.0, 226.0);
        pb.cubic_to(380.0, 236.0, 300.0, 250.0, 214.0, 262.0);
        pb.cubic_to(205.0, 230.0, 215.0, 150.0, 250.0, 60.0);
    })?;
    let green = seg(&|pb| {
        pb.move_to(60.0, 282.0);
        pb.cubic_to(100.0, 262.0, 150.0, 250.0, 214.0, 262.0);
        pb.cubic_to(222.0, 330.0, 250.0, 400.0, 280.0, 452.0);
        pb.line_to(60.0, 452.0);
    })?;
    let gold = seg(&|pb| {
        pb.move_to(214.0, 262.0);
        pb.cubic_to(300.0, 250.0, 380.0, 236.0, 452.0, 226.0);
        pb.line_to(452.0, 452.0);
        pb.line_to(280.0, 452.0);
        pb.cubic_to(250.0, 400.0, 222.0, 330.0, 214.0, 262.0);
    })?;
    for (path, (a, b), (x0, y0, x1, y1)) in [
        (&blue, ((0x3B, 0x8E, 0xDB), (0x1F, 0x5F, 0xA8)), (60.0, 60.0, 250.0, 282.0)),
        (&teal, ((0x22, 0xB5, 0xA2), (0x0E, 0x85, 0x76)), (214.0, 60.0, 452.0, 262.0)),
        (&green, ((0x8C, 0xC8, 0x4B), (0x4E, 0x9A, 0x2E)), (60.0, 250.0, 280.0, 452.0)),
        (&gold, ((0xF8, 0xC1, 0x50), (0xE3, 0x9A, 0x22)), (214.0, 226.0, 452.0, 452.0)),
    ] {
        pm.fill_path(path, &gradient(a, b, x0, y0, x1, y1), FillRule::Winding, t, Some(&mask));
    }
    let gap = Stroke { width: 14.0, line_cap: LineCap::Round, ..Stroke::default() };
    let mut pb = PathBuilder::new();
    pb.move_to(250.0, 60.0);
    pb.cubic_to(215.0, 150.0, 205.0, 230.0, 214.0, 262.0);
    pb.cubic_to(222.0, 330.0, 250.0, 400.0, 280.0, 452.0);
    pb.move_to(60.0, 282.0);
    pb.cubic_to(100.0, 262.0, 150.0, 250.0, 214.0, 262.0);
    pb.cubic_to(300.0, 250.0, 380.0, 236.0, 452.0, 226.0);
    pm.stroke_path(&pb.finish()?, &white, &gap, t, Some(&mask));
    let mut pb = PathBuilder::new();
    pb.move_to(70.0, 170.0);
    pb.cubic_to(170.0, 135.0, 330.0, 125.0, 440.0, 150.0);
    pb.move_to(75.0, 360.0);
    pb.cubic_to(180.0, 345.0, 330.0, 335.0, 440.0, 345.0);
    pm.stroke_path(&pb.finish()?, &solid(255, 255, 255, 140), &Stroke { width: 5.0, ..Stroke::default() }, t, Some(&mask));

    // Orbit: the front half of an ellipse tilted by -28°, ending in a node.
    let (rx, ry, tilt) = (226.0f32, 70.0f32, (-28.0f32).to_radians());
    let mut pb = PathBuilder::new();
    for i in 0..=64 {
        let th = std::f32::consts::PI * (1.0 - i as f32 / 64.0);
        let (ex, ey) = (rx * th.cos(), ry * th.sin());
        let (x, y) = (C + ex * tilt.cos() - ey * tilt.sin(), C + ex * tilt.sin() + ey * tilt.cos());
        if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
    }
    let orbit = pb.finish()?;
    pm.stroke_path(&orbit, &white, &Stroke { width: 24.0, line_cap: LineCap::Round, ..Stroke::default() }, t, None);
    pm.stroke_path(&orbit, &rust, &Stroke { width: 11.0, line_cap: LineCap::Round, ..Stroke::default() }, t, None);
    let (nx, ny) = (C + rx * tilt.cos(), C + rx * tilt.sin());
    pm.fill_path(&PathBuilder::from_circle(nx, ny, 22.0)?, &white, FillRule::Winding, t, None);
    pm.fill_path(&PathBuilder::from_circle(nx, ny, 14.0)?, &rust, FillRule::Winding, t, None);
    Some(pm)
}

/// The logo as straight (unpremultiplied) RGBA bytes, for window icons.
pub fn rgba(size: u32) -> Option<(u32, Vec<u8>)> {
    let pm = pixmap(size)?;
    let mut out = Vec::with_capacity(pm.data().len());
    for p in pm.pixels() {
        let c = p.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some((pm.width(), out))
}

/// The logo as an ICO file holding PNG images from 16 to 256 px (Windows Vista and later read
/// these), for the exe icon and the Windows installers.
pub fn ico() -> Option<Vec<u8>> {
    const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
    let mut images = Vec::new();
    for size in SIZES {
        images.push((size, pixmap(size)?.encode_png().ok()?));
    }
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&u16::try_from(images.len()).ok()?.to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (size, png) in &images {
        // A dimension of 0 means 256.
        let dim = u8::try_from(*size).unwrap_or(0);
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&u32::try_from(png.len()).ok()?.to_le_bytes());
        ico.extend_from_slice(&u32::try_from(offset).ok()?.to_le_bytes());
        offset += png.len();
    }
    for (_, png) in &images {
        ico.extend_from_slice(png);
    }
    Some(ico)
}

/// A 24-bit BMP of `width`×`height` white pixels with the logo (`size` px) drawn at `x`, `y`,
/// for installer wizard images. Sizes are clamped to 1..=2048.
pub fn bmp(size: u32, width: u32, height: u32, x: i32, y: i32) -> Option<Vec<u8>> {
    let (w, h) = (width.clamp(1, 2048), height.clamp(1, 2048));
    let (side, px) = rgba(size)?;
    let row = (w as usize * 3).div_ceil(4) * 4;
    let len = 54 + row * h as usize;
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&u32::try_from(len).ok()?.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&u32::try_from(row * h as usize).ok()?.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    // Rows run bottom to top, pixels are BGR, each row padded to four bytes.
    for r in (0..h as i64).rev() {
        let start = out.len();
        for c in 0..w as i64 {
            let (lx, ly) = (c - i64::from(x), r - i64::from(y));
            let mut bgr = [255u8; 3];
            if (0..i64::from(side)).contains(&lx) && (0..i64::from(side)).contains(&ly) {
                let i = usize::try_from((ly * i64::from(side) + lx) * 4).ok()?;
                if let Some(p) = px.get(i..i + 4) {
                    let a = u32::from(p[3]);
                    let over = |v: u8| ((u32::from(v) * a + 255 * (255 - a)) / 255) as u8;
                    bgr = [over(p[2]), over(p[1]), over(p[0])];
                }
            }
            out.extend_from_slice(&bgr);
        }
        out.resize(start + row, 0);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn draws_at_every_size() {
        for s in [0, 16, 32, 256, 5000] {
            let (w, px) = super::rgba(s).unwrap();
            assert_eq!(px.len() as u32, w * w * 4);
            assert!(px.chunks(4).any(|c| c[3] == 255), "logo has opaque pixels");
        }
    }

    #[test]
    fn monochrome_is_white_with_the_white_parts_cut_out() {
        let pm = super::monochrome(512).unwrap();
        let at = |x: u32, y: u32| pm.pixel(x, y).unwrap();
        // Every pixel is white or clear (premultiplied: each channel equals the alpha).
        assert!(pm.pixels().iter().all(|p| p.red() == p.alpha() && p.green() == p.alpha() && p.blue() == p.alpha()));
        // A cog tooth is solid, the corner and the ring between cog and globe are clear.
        assert_eq!(at(256, 14).alpha(), 255);
        assert_eq!(at(2, 2).alpha(), 0);
        assert_eq!(at(256, 256 + 175).alpha(), 0);
        // A globe segment is solid, and the gap between two segments is clear.
        assert_eq!(at(180, 180).alpha(), 255);
        assert_eq!(at(214, 262).alpha(), 0);
    }

    #[test]
    fn bmp_has_padded_rows_and_the_logo() {
        let bmp = super::bmp(32, 50, 40, 9, 4).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(bmp.len(), 54 + 152 * 40);
        assert!(bmp[54..].chunks(3).any(|c| c != [255, 255, 255]), "logo pixels");
        // A logo placed off the canvas leaves it white.
        let blank = super::bmp(32, 10, 10, 500, -500).unwrap();
        assert!(blank[54..].chunks(32).all(|r| r[..30].iter().all(|&b| b == 255)));
    }

    #[test]
    fn ico_lists_seven_png_images() {
        let ico = super::ico().unwrap();
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 7, 0]);
        // The first image starts right after the directory, with the PNG signature.
        let offset = u32::from_le_bytes(ico[18..22].try_into().unwrap()) as usize;
        assert_eq!(offset, 6 + 16 * 7);
        assert_eq!(&ico[offset..offset + 4], b"\x89PNG");
    }
}
