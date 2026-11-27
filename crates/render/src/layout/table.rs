//! The attribute table element.

use super::*;

/// The first `rows` rows of a table, with as many columns as fit: on white cells in dark grey, or
/// with an `ink` (#2257) in it on no paper of its own, its header and stripes a tint of the ink, so
/// it reads on whatever is behind it (a dark page, or the element's background).
pub(super) fn table(pm: &mut Canvas, r: Rect, ds: &rustgis_data::Dataset, rows: usize, ink: Option<Rgba>, ppp: f64) {
    let ppp = ppp as f32;
    let px = 8.0 * 1.33 * ppp;
    let row_h = px * 1.6;
    let col_w = (90.0 * ppp).min(r.width());
    let cols = ((r.width() / col_w).floor() as usize).clamp(1, ds.fields.len().max(1));
    let n = rows.min(ds.features.len()).min(((r.height() / row_h) as usize).saturating_sub(1));
    let dark = ink.unwrap_or(INK);
    // Without an ink, the white paper, header and stripes it always had, so a table drawn before
    // draws to the same pixels. With one, white cells could hide a light ink.
    let ((header, header_alpha), (stripe, stripe_alpha)) = match ink {
        None => ((Rgba::rgb(0xE4, 0xE8, 0xEE), 1.0), (Rgba::rgb(0xF5, 0xF6, 0xF8), 1.0)),
        Some(c) => ((c, 0.18), (c, 0.08)),
    };
    if ink.is_none()
        && let Some(bg) = Rect::from_xywh(r.x(), r.y(), col_w * cols as f32, row_h * (n + 1) as f32)
    {
        pm.fill_rect(bg, &paint(Rgba::rgb(0xFF, 0xFF, 0xFF), 1.0), Transform::identity(), None);
    }
    if let Some(hdr) = Rect::from_xywh(r.x(), r.y(), col_w * cols as f32, row_h) {
        pm.fill_rect(hdr, &paint(header, header_alpha), Transform::identity(), None);
    }
    let fit = |s: &str| -> String {
        let mut t: String = s.chars().take(40).collect();
        while !t.is_empty() && text::measure(&t, 8.0, false) * ppp > col_w - 6.0 * ppp {
            t.pop();
        }
        t
    };
    for (c, f) in ds.fields.iter().take(cols).enumerate() {
        text::text_at(
            pm,
            &fit(f.display()),
            r.x() + c as f32 * col_w + 3.0 * ppp,
            r.y() + row_h / 2.0,
            px,
            dark,
            true,
            text::Align::Left,
            false,
        );
    }
    for row in 0..n {
        let y = r.y() + row_h * (row + 1) as f32;
        if row % 2 == 1
            && let Some(b) = Rect::from_xywh(r.x(), y, col_w * cols as f32, row_h)
        {
            pm.fill_rect(b, &paint(stripe, stripe_alpha), Transform::identity(), None);
        }
        for c in 0..cols {
            text::text_at(
                pm,
                &fit(&ds.value(row, c).to_string()),
                r.x() + c as f32 * col_w + 3.0 * ppp,
                y + row_h / 2.0,
                px,
                dark,
                false,
                text::Align::Left,
                false,
            );
        }
    }
}
