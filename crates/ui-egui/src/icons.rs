//! Icons, all original and drawn in code on a 24-unit grid: neutral ink strokes with accent fills.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2};

struct P<'a> {
    p: &'a Painter,
    r: Rect,
    ink: Color32,
    acc: Color32,
}

impl P<'_> {
    fn s(&self) -> f32 {
        self.r.width() / 24.0
    }
    fn pt(&self, x: f32, y: f32) -> Pos2 {
        pos2(self.r.min.x + x * self.s(), self.r.min.y + y * self.s())
    }
    fn w(&self, w: f32) -> f32 {
        (w * self.s()).max(1.0)
    }
    fn line(&self, pts: &[(f32, f32)], w: f32, c: Color32) {
        let v: Vec<Pos2> = pts.iter().map(|(x, y)| self.pt(*x, *y)).collect();
        self.p.add(Shape::line(v, Stroke::new(self.w(w), c)));
    }
    fn closed(&self, pts: &[(f32, f32)], w: f32, c: Color32) {
        let v: Vec<Pos2> = pts.iter().map(|(x, y)| self.pt(*x, *y)).collect();
        self.p.add(Shape::closed_line(v, Stroke::new(self.w(w), c)));
    }
    /// Convex fill.
    fn fill(&self, pts: &[(f32, f32)], c: Color32) {
        let v: Vec<Pos2> = pts.iter().map(|(x, y)| self.pt(*x, *y)).collect();
        self.p.add(Shape::convex_polygon(v, c, Stroke::NONE));
    }
    fn rect(&self, x: f32, y: f32, w: f32, h: f32, fill: Option<Color32>, stroke: Option<(f32, Color32)>, round: f32) {
        let r = Rect::from_min_max(self.pt(x, y), self.pt(x + w, y + h));
        if let Some(f) = fill {
            self.p.rect_filled(r, round * self.s(), f);
        }
        if let Some((sw, c)) = stroke {
            self.p.rect_stroke(r, round * self.s(), Stroke::new(self.w(sw), c), egui::StrokeKind::Middle);
        }
    }
    fn circle(&self, x: f32, y: f32, rad: f32, fill: Option<Color32>, stroke: Option<(f32, Color32)>) {
        let c = self.pt(x, y);
        if let Some(f) = fill {
            self.p.circle_filled(c, rad * self.s(), f);
        }
        if let Some((sw, col)) = stroke {
            self.p.circle_stroke(c, rad * self.s(), Stroke::new(self.w(sw), col));
        }
    }
    fn arc(&self, cx: f32, cy: f32, rad: f32, a0: f32, a1: f32, w: f32, c: Color32) {
        let n = 24;
        let pts: Vec<(f32, f32)> = (0..=n)
            .map(|i| {
                let a = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
                (cx + rad * a.cos(), cy + rad * a.sin())
            })
            .collect();
        self.line(&pts, w, c);
    }
    fn dashed_rect(&self, x: f32, y: f32, w: f32, h: f32, c: Color32) {
        let pts = [self.pt(x, y), self.pt(x + w, y), self.pt(x + w, y + h), self.pt(x, y + h), self.pt(x, y)];
        self.p.extend(Shape::dashed_line(&pts, Stroke::new(self.w(1.3), c), 2.2 * self.s(), 1.6 * self.s()));
    }
    fn text(&self, x: f32, y: f32, s: &str, size: f32, c: Color32) {
        self.p.text(self.pt(x, y), egui::Align2::CENTER_CENTER, s, crate::theme::semibold(size * self.s()), c);
    }
    fn tint(&self, a: u8) -> Color32 {
        self.acc.gamma_multiply(a as f32 / 255.0)
    }

    // Composite pieces.
    fn globe(&self, cx: f32, cy: f32, r: f32) {
        self.circle(cx, cy, r, Some(self.tint(60)), Some((1.4, self.ink)));
        self.line(&[(cx - r, cy), (cx + r, cy)], 1.1, self.ink);
        let n = 16;
        let mer: Vec<(f32, f32)> = (0..=n)
            .map(|i| {
                let a = (-90.0 + 180.0 * i as f32 / n as f32).to_radians();
                (cx + r * 0.45 * a.cos(), cy + r * a.sin())
            })
            .collect();
        self.line(&mer, 1.1, self.ink);
        let mer2: Vec<(f32, f32)> = mer.iter().map(|(x, y)| (2.0 * cx - x, *y)).collect();
        self.line(&mer2, 1.1, self.ink);
    }
    fn magnifier(&self, cx: f32, cy: f32, r: f32) {
        self.circle(cx, cy, r, Some(self.tint(45)), Some((1.6, self.ink)));
        let d = r * 0.72;
        self.line(&[(cx + d, cy + d), (cx + d + 5.0, cy + d + 5.0)], 2.4, self.ink);
    }
    fn cursor(&self, x: f32, y: f32, k: f32) {
        let pts = [
            (x, y),
            (x, y + 11.0 * k),
            (x + 3.0 * k, y + 8.2 * k),
            (x + 5.2 * k, y + 12.5 * k),
            (x + 7.0 * k, y + 11.6 * k),
            (x + 4.9 * k, y + 7.4 * k),
            (x + 8.5 * k, y + 7.2 * k),
        ];
        let v: Vec<Pos2> = pts.iter().map(|(a, b)| self.pt(*a, *b)).collect();
        self.p.add(Shape::Path(egui::epaint::PathShape {
            points: v,
            closed: true,
            fill: Color32::WHITE,
            stroke: Stroke::new(self.w(1.2), self.ink).into(),
        }));
    }
    /// Paper: a page or a table's white, dark grey in the dark theme. Asked of the theme, not
    /// guessed from the ink, which may be a mid grey (the light theme's disabled text).
    fn paper(&self) -> Color32 {
        if self.p.ctx().global_style().visuals.dark_mode { Color32::from_gray(40) } else { Color32::WHITE }
    }
    fn page(&self, x: f32, y: f32, w: f32, h: f32) {
        let f = 4.0;
        self.fill(&[(x, y), (x + w - f, y), (x + w, y + f), (x + w, y + h), (x, y + h)], self.paper());
        self.closed(&[(x, y), (x + w - f, y), (x + w, y + f), (x + w, y + h), (x, y + h)], 1.3, self.ink);
        self.line(&[(x + w - f, y), (x + w - f, y + f), (x + w, y + f)], 1.1, self.ink);
    }
    fn table(&self, x: f32, y: f32, w: f32, h: f32) {
        self.rect(x, y, w, h, Some(self.paper()), None, 1.0);
        self.rect(x, y, w, 3.5, Some(self.acc), None, 1.0);
        self.rect(x, y, w, h, None, Some((1.3, self.ink)), 1.0);
        let rows = 3;
        for i in 1..rows {
            let yy = y + 3.5 + (h - 3.5) * i as f32 / rows as f32;
            self.line(&[(x, yy), (x + w, yy)], 0.9, self.ink);
        }
        self.line(&[(x + w * 0.4, y + 3.5), (x + w * 0.4, y + h)], 0.9, self.ink);
    }
    /// A bookmark ribbon in the box `x, y, w, h` (the tab's notch and accent band scale with it).
    fn bookmark(&self, x: f32, y: f32, w: f32, h: f32) {
        let k = h / 18.0;
        let outline = [(x, y), (x + w, y), (x + w, y + h), (x + w / 2.0, y + h - 5.0 * k), (x, y + h)];
        self.fill(&outline, self.tint(60));
        self.fill(&[(x, y), (x + w, y), (x + w, y + 5.0 * k), (x, y + 5.0 * k)], self.acc);
        self.closed(&outline, 1.5, self.ink);
    }
    fn plus_badge(&self, x: f32, y: f32) {
        self.circle(x, y, 4.2, Some(Color32::from_rgb(0x16, 0x9A, 0x5A)), None);
        self.line(&[(x - 2.2, y), (x + 2.2, y)], 1.5, Color32::WHITE);
        self.line(&[(x, y - 2.2), (x, y + 2.2)], 1.5, Color32::WHITE);
    }
    fn layers(&self, x: f32, y: f32, k: f32) {
        for (i, a) in [(2.0, 90u8), (0.0, 170u8)] {
            let yy = y + i * k * 2.0;
            let pts = [(x + 10.0 * k, yy), (x + 20.0 * k, yy + 5.0 * k), (x + 10.0 * k, yy + 10.0 * k), (x, yy + 5.0 * k)];
            self.fill(&pts, self.tint(a));
            self.closed(&pts, 1.3, self.ink);
        }
    }
    fn polygon_shape(&self, pts: &[(f32, f32)], a: u8) {
        // Fan triangulation from the first vertex (fine for these small, near-convex shapes).
        let Some(first) = pts.first() else { return };
        for w in pts.windows(2).skip(1) {
            if let [b, c] = w {
                self.fill(&[*first, *b, *c], self.tint(a));
            }
        }
        self.closed(pts, 1.4, self.ink);
    }
}

/// The arrow for a route's maneuver (ink only): `straight`, `slight_right`, `right`,
/// `sharp_right`, `uturn` and their left twins, `depart`, `arrive`, `roundabout`, `merge`.
fn maneuver(p: &P, kind: &str) {
    let ink = p.ink;
    // An arrow head at `(x, y)` pointing along `a` (degrees clockwise from up).
    let head = |x: f32, y: f32, a: f32| {
        let (s, c) = (a.to_radians().sin(), a.to_radians().cos());
        let at = |f: f32, side: f32| (x + s * f + c * side, y - c * f + s * side);
        p.fill(&[at(2.2, 0.0), at(-3.0, 3.6), at(-3.0, -3.6)], ink);
    };
    match kind {
        "depart" => {
            p.fill(&[(12.0, 3.0), (19.0, 20.0), (12.0, 16.0)], ink);
            p.fill(&[(12.0, 3.0), (12.0, 16.0), (5.0, 20.0)], ink);
        }
        "arrive" => {
            p.line(&[(7.0, 21.0), (7.0, 3.0)], 2.0, ink);
            p.fill(&[(7.0, 3.5), (18.0, 6.5), (7.0, 10.5)], ink);
        }
        "roundabout" => {
            p.arc(12.0, 11.0, 5.0, 30.0, 300.0, 2.2, ink);
            p.line(&[(12.0, 21.0), (12.0, 16.0)], 2.2, ink);
            head(16.6, 6.6, 50.0);
        }
        "merge" => {
            p.line(&[(7.0, 21.0), (7.0, 16.0), (12.0, 10.0), (12.0, 5.0)], 2.2, ink);
            p.line(&[(17.0, 21.0), (17.0, 16.0), (12.0, 10.0)], 2.2, ink);
            head(12.0, 4.5, 0.0);
        }
        "uturn_left" | "uturn" | "uturn_right" => {
            // Up, around, and back down on the left (or the right).
            let d = if kind == "uturn_right" { 1.0 } else { -1.0 };
            p.line(&[(12.0 - 3.5 * d, 21.0), (12.0 - 3.5 * d, 10.0)], 2.2, ink);
            let (a0, a1) = if d < 0.0 { (0.0, -180.0) } else { (180.0, 360.0) };
            p.arc(12.0, 10.0, 3.5, a0, a1, 2.2, ink);
            p.line(&[(12.0 + 3.5 * d, 10.0), (12.0 + 3.5 * d, 15.0)], 2.2, ink);
            head(12.0 + 3.5 * d, 16.5, 180.0);
        }
        _ => {
            let a: f32 = match kind {
                "slight_right" => 40.0,
                "right" => 90.0,
                "sharp_right" => 135.0,
                "slight_left" => -40.0,
                "left" => -90.0,
                "sharp_left" => -135.0,
                _ => 0.0,
            };
            // A stem up from the bottom, then off in the new direction.
            // Off-centre away from the turn (`signum` of 0 is 1: straight stays centred).
            let side = if a == 0.0 { 0.0 } else { a.signum() * 2.5 };
            let (x0, y0) = (12.0 - side, 12.5);
            let len = if a.abs() > 100.0 { 6.0 } else { 7.5 };
            let (x1, y1) = (x0 + a.to_radians().sin() * len, y0 - a.to_radians().cos() * len);
            p.line(&[(x0, 21.0), (x0, y0), (x1, y1)], 2.2, ink);
            head(x1, y1, a);
        }
    }
}

/// Paints icon `name` into `r`. `ink` draws outlines, `accent` the colour parts.
pub fn paint(painter: &Painter, r: Rect, name: &str, ink: Color32, accent: Color32) {
    let p = P { p: painter, r, ink, acc: accent };
    let green = Color32::from_rgb(0x16, 0x9A, 0x5A);
    let red = Color32::from_rgb(0xD1, 0x3C, 0x3C);
    let amber = Color32::from_rgb(0xE5, 0x9E, 0x1A);
    let teal = Color32::from_rgb(0x0E, 0x93, 0x84);
    match name {
        "directions" => {
            // A road sign: a diamond with a turning arrow.
            p.fill(&[(12.0, 2.0), (22.0, 12.0), (12.0, 22.0), (2.0, 12.0)], accent);
            p.closed(&[(12.0, 2.0), (22.0, 12.0), (12.0, 22.0), (2.0, 12.0)], 1.4, ink);
            let w = Color32::WHITE;
            p.line(&[(9.0, 17.0), (9.0, 12.5), (10.0, 11.0), (15.0, 11.0)], 1.8, w);
            p.fill(&[(14.0, 8.0), (17.5, 11.0), (14.0, 14.0)], w);
        }
        n if n.starts_with("maneuver_") => maneuver(&p, &n["maneuver_".len()..]),
        "explore" => {
            p.globe(10.0, 10.0, 7.5);
            p.cursor(12.5, 10.5, 0.85);
        }
        "zoom_full" => {
            p.globe(12.0, 12.0, 6.0);
            for (x, y, dx, dy) in [(3.0, 3.0, 1.0, 1.0), (21.0, 3.0, -1.0, 1.0), (3.0, 21.0, 1.0, -1.0), (21.0, 21.0, -1.0, -1.0)] {
                p.line(&[(x, y + 4.0 * dy), (x, y), (x + 4.0 * dx, y)], 1.6, ink);
            }
        }
        "zoom_world" => p.globe(12.0, 12.0, 9.0),
        "zoom_in" | "zoom_out" => {
            p.magnifier(10.0, 10.0, 6.5);
            p.line(&[(7.0, 10.0), (13.0, 10.0)], 1.7, ink);
            if name == "zoom_in" {
                p.line(&[(10.0, 7.0), (10.0, 13.0)], 1.7, ink);
            }
        }
        "zoom_box" => {
            p.dashed_rect(3.0, 3.0, 13.0, 11.0, ink);
            p.magnifier(15.0, 14.0, 4.5);
        }
        "prev_extent" | "next_extent" => {
            p.circle(12.0, 12.0, 9.0, Some(p.tint(45)), Some((1.4, ink)));
            let d = if name == "prev_extent" { -1.0 } else { 1.0 };
            p.line(&[(12.0 - 4.5 * d, 12.0), (12.0 + 4.5 * d, 12.0)], 1.8, ink);
            p.line(&[(12.0 + 1.0 * d, 8.5), (12.0 + 4.5 * d, 12.0), (12.0 + 1.0 * d, 15.5)], 1.8, ink);
        }
        "bookmark" => p.bookmark(6.0, 3.0, 12.0, 18.0),
        // Export and Import Bookmarks (#351): the export arrow of Export Map and Export Layer,
        // leaving the bookmark over its corner, or turned round, coming into it. A halo of paper
        // keeps the arrow off the bookmark's outline.
        "bookmark_export" | "bookmark_import" => {
            p.bookmark(3.0, 2.0, 13.0, 18.0);
            let (shaft, head) = if name == "bookmark_export" {
                ([(14.0, 20.0), (21.5, 12.5)], [(17.0, 12.5), (21.5, 12.5), (21.5, 17.0)])
            } else {
                ([(21.5, 12.5), (14.0, 20.0)], [(14.0, 15.5), (14.0, 20.0), (18.5, 20.0)])
            };
            for (w, c) in [(3.6, p.paper()), (2.0, green)] {
                p.line(&shaft, w, c);
                p.line(&head, w, c);
            }
        }
        // New and Update Bookmark (#351): the bookmark with a green badge on its corner, as New
        // wears its plus: a plus, or an arrow turning.
        "bookmark_new" | "bookmark_update" => {
            p.bookmark(5.0, 2.0, 12.0, 18.0);
            let (cx, cy, white) = (18.0, 18.0, Color32::WHITE);
            p.circle(cx, cy, 5.0, Some(green), None);
            if name == "bookmark_new" {
                p.line(&[(cx - 2.8, cy), (cx + 2.8, cy)], 1.9, white);
                p.line(&[(cx, cy - 2.8), (cx, cy + 2.8)], 1.9, white);
            } else {
                let r = 2.9;
                p.arc(cx, cy, r, -150.0, 120.0, 1.6, white);
                // The head at the arc's end, pointing on round the circle.
                let a = 120.0f32.to_radians();
                let (ex, ey) = (cx + r * a.cos(), cy + r * a.sin());
                let (tx, ty, nx, ny) = (-a.sin(), a.cos(), a.cos(), a.sin());
                p.fill(&[(ex + tx * 2.1, ey + ty * 2.1), (ex + nx * 1.8, ey + ny * 1.8), (ex - nx * 1.8, ey - ny * 1.8)], white);
            }
        }
        "goto_xy" | "locate" => {
            p.circle(12.0, 9.0, 6.0, Some(if name == "locate" { red } else { accent }), Some((1.4, ink)));
            p.fill(&[(7.0, 11.5), (17.0, 11.5), (12.0, 21.0)], if name == "locate" { red } else { accent });
            p.line(&[(7.0, 11.5), (12.0, 21.0), (17.0, 11.5)], 1.4, ink);
            p.circle(12.0, 9.0, 2.3, Some(Color32::WHITE), None);
            if name == "goto_xy" {
                p.text(20.0, 19.5, "XY", 6.5, ink);
            }
        }
        "basemap" => {
            p.fill(&[(2.0, 6.0), (8.0, 3.5), (8.0, 18.0), (2.0, 20.5)], p.tint(80));
            p.fill(&[(8.0, 3.5), (16.0, 6.0), (16.0, 20.5), (8.0, 18.0)], p.tint(150));
            p.fill(&[(16.0, 6.0), (22.0, 3.5), (22.0, 18.0), (16.0, 20.5)], p.tint(80));
            p.closed(&[(2.0, 6.0), (8.0, 3.5), (16.0, 6.0), (22.0, 3.5), (22.0, 18.0), (16.0, 20.5), (8.0, 18.0), (2.0, 20.5)], 1.4, ink);
            p.line(&[(8.0, 3.5), (8.0, 18.0)], 1.1, ink);
            p.line(&[(16.0, 6.0), (16.0, 20.5)], 1.1, ink);
            p.line(&[(4.0, 13.0), (7.0, 11.0), (11.0, 13.5), (14.0, 10.0), (19.0, 12.0)], 1.3, green);
        }
        "add_data" => {
            p.layers(2.0, 5.0, 0.9);
            p.plus_badge(18.0, 17.5);
        }
        "web_data" => {
            p.globe(10.0, 10.0, 7.5);
            p.plus_badge(18.0, 17.5);
        }
        "add_folder" | "open" => {
            p.fill(&[(2.0, 6.0), (9.0, 6.0), (11.0, 8.0), (22.0, 8.0), (22.0, 19.0), (2.0, 19.0)], amber);
            p.closed(&[(2.0, 5.0), (9.0, 5.0), (11.0, 7.5), (22.0, 7.5), (22.0, 19.0), (2.0, 19.0)], 1.3, ink);
            p.line(&[(2.0, 10.0), (22.0, 10.0)], 1.0, ink);
        }
        "select" => {
            p.dashed_rect(3.0, 3.0, 14.0, 12.0, accent);
            p.rect(3.0, 3.0, 14.0, 12.0, Some(p.tint(40)), None, 0.0);
            p.cursor(12.0, 9.0, 1.0);
        }
        "select_attr" => {
            p.table(2.0, 3.0, 15.0, 13.0);
            p.fill(&[(12.0, 12.0), (22.0, 12.0), (18.5, 16.5), (15.5, 16.5)], accent);
            p.rect(15.5, 16.0, 3.0, 5.0, Some(accent), None, 0.0);
            p.closed(&[(12.0, 12.0), (22.0, 12.0), (18.5, 16.5), (18.5, 21.0), (15.5, 21.0), (15.5, 16.5)], 1.2, ink);
        }
        "select_loc" => {
            p.polygon_shape(&[(2.0, 8.0), (9.0, 3.0), (15.0, 7.0), (13.0, 15.0), (4.0, 15.0)], 90);
            p.circle(16.0, 15.0, 5.5, Some(p.tint(170)), Some((1.4, ink)));
        }
        "clear_sel" => {
            p.dashed_rect(3.0, 3.0, 14.0, 12.0, ink);
            p.circle(17.5, 17.5, 5.0, Some(red), None);
            p.line(&[(15.5, 15.5), (19.5, 19.5)], 1.6, Color32::WHITE);
            p.line(&[(19.5, 15.5), (15.5, 19.5)], 1.6, Color32::WHITE);
        }
        "zoom_sel" => {
            p.rect(
                3.0,
                3.0,
                11.0,
                9.0,
                Some(Color32::from_rgb(0x00, 0xC8, 0xE0).gamma_multiply(0.35)),
                Some((1.4, Color32::from_rgb(0x00, 0xA8, 0xC0))),
                0.0,
            );
            p.magnifier(12.5, 12.5, 5.0);
        }
        "measure" => {
            p.fill(&[(2.0, 15.0), (15.0, 2.0), (22.0, 9.0), (9.0, 22.0)], amber);
            p.closed(&[(2.0, 15.0), (15.0, 2.0), (22.0, 9.0), (9.0, 22.0)], 1.3, ink);
            for i in 0..5 {
                let t = 3.5 + i as f32 * 2.6;
                let l = if i % 2 == 0 { 3.2 } else { 2.0 };
                p.line(&[(2.0 + t, 15.0 - t), (2.0 + t + l * 0.7, 15.0 - t + l * 0.7)], 1.1, ink);
            }
        }
        "measure_area" => {
            p.polygon_shape(&[(3.0, 18.0), (6.0, 5.0), (17.0, 3.0), (21.0, 14.0), (13.0, 21.0)], 70);
            for (x, y) in [(3.0, 18.0), (6.0, 5.0), (17.0, 3.0), (21.0, 14.0), (13.0, 21.0)] {
                p.rect(x - 1.5, y - 1.5, 3.0, 3.0, Some(Color32::WHITE), Some((1.0, ink)), 0.0);
            }
        }
        "identify" | "about" => {
            p.circle(12.0, 12.0, 9.0, Some(accent), None);
            p.circle(12.0, 7.5, 1.4, Some(Color32::WHITE), None);
            p.line(&[(12.0, 10.5), (12.0, 17.0)], 2.2, Color32::WHITE);
        }
        "message_log" => {
            // A message bubble containing a log's severity dots and text lines.
            p.closed(&[(3.0, 3.0), (21.0, 3.0), (21.0, 17.0), (11.0, 17.0), (6.0, 21.0), (6.0, 17.0), (3.0, 17.0)], 1.4, ink);
            for (y, color) in [(7.0, red), (10.5, amber), (14.0, accent)] {
                p.circle(6.5, y, 1.0, Some(color), None);
                p.line(&[(10.0, y), (18.0, y)], 1.3, ink);
            }
        }
        "help" => {
            p.circle(12.0, 12.0, 9.0, Some(accent), None);
            p.text(12.0, 12.3, "?", 12.0, Color32::WHITE);
        }
        "label" => {
            p.fill(&[(2.0, 7.0), (17.0, 7.0), (22.0, 12.0), (17.0, 17.0), (2.0, 17.0)], p.tint(70));
            p.closed(&[(2.0, 7.0), (17.0, 7.0), (22.0, 12.0), (17.0, 17.0), (2.0, 17.0)], 1.3, ink);
            p.text(10.0, 12.0, "Abc", 6.8, ink);
        }
        "label_off" => {
            paint(painter, r, "label", ink, accent);
            p.line(&[(3.0, 21.0), (21.0, 3.0)], 1.8, red);
        }
        "new_map" | "map" => {
            p.fill(&[(2.0, 5.0), (8.0, 3.0), (16.0, 5.0), (22.0, 3.0), (22.0, 19.0), (16.0, 21.0), (8.0, 19.0), (2.0, 21.0)], p.tint(50));
            p.closed(&[(2.0, 5.0), (8.0, 3.0), (16.0, 5.0), (22.0, 3.0), (22.0, 19.0), (16.0, 21.0), (8.0, 19.0), (2.0, 21.0)], 1.3, ink);
            p.line(&[(8.0, 3.0), (8.0, 19.0)], 1.0, ink);
            p.line(&[(16.0, 5.0), (16.0, 21.0)], 1.0, ink);
            p.polygon_shape(&[(10.0, 8.0), (14.0, 9.0), (13.0, 14.0), (9.5, 13.0)], 200);
            if name == "new_map" {
                p.plus_badge(19.0, 18.5);
            }
        }
        "new_layout" => {
            p.page(4.0, 2.0, 15.0, 20.0);
            p.rect(6.5, 5.0, 10.0, 8.0, Some(p.tint(90)), Some((1.0, ink)), 0.0);
            p.line(&[(6.5, 16.0), (16.5, 16.0)], 1.0, ink);
            p.line(&[(6.5, 18.5), (13.0, 18.5)], 1.0, ink);
            p.plus_badge(19.0, 18.5);
        }
        "toolbox" => {
            p.rect(2.0, 8.0, 20.0, 13.0, Some(red), Some((1.3, ink)), 1.5);
            p.line(&[(8.0, 8.0), (8.0, 5.0), (16.0, 5.0), (16.0, 8.0)], 1.5, ink);
            p.rect(2.0, 12.0, 20.0, 2.0, Some(Color32::from_rgb(0x9C, 0x2A, 0x2A)), None, 0.0);
            p.rect(10.5, 11.0, 3.0, 4.0, Some(Color32::from_gray(230)), Some((1.0, ink)), 0.5);
        }
        "tools" | "geoprocessing" => {
            p.line(&[(4.0, 20.0), (13.0, 11.0)], 3.0, ink);
            p.circle(15.5, 8.5, 5.0, Some(accent), Some((1.3, ink)));
            p.circle(15.5, 8.5, 1.8, Some(Color32::WHITE), None);
            p.line(&[(15.0, 15.0), (20.0, 20.0)], 3.0, amber);
        }
        "modelbuilder" => {
            p.circle(5.5, 6.0, 3.5, Some(accent), Some((1.2, ink)));
            p.rect(9.5, 9.5, 6.0, 5.0, Some(amber), Some((1.2, ink)), 1.0);
            p.circle(18.5, 18.0, 3.5, Some(green), Some((1.2, ink)));
            p.line(&[(8.0, 8.0), (10.0, 10.0)], 1.3, ink);
            p.line(&[(15.0, 14.5), (16.5, 16.0)], 1.3, ink);
        }
        "python" => {
            p.rect(3.0, 4.0, 18.0, 16.0, Some(Color32::from_rgb(0x2B, 0x32, 0x3C)), Some((1.2, ink)), 2.0);
            p.line(&[(6.0, 9.0), (9.0, 12.0), (6.0, 15.0)], 1.6, Color32::from_rgb(0x8F, 0xD1, 0x8F));
            p.line(&[(11.0, 15.0), (17.0, 15.0)], 1.6, Color32::WHITE);
        }
        "assistant" => {
            // A speech bubble with a sparkle.
            p.rect(2.0, 5.0, 15.5, 11.5, Some(p.tint(40)), Some((1.4, ink)), 3.0);
            p.fill(&[(5.5, 16.0), (10.0, 16.0), (5.5, 20.5)], ink);
            for x in [6.0, 9.75, 13.5] {
                p.circle(x, 10.75, 1.1, Some(ink), None);
            }
            p.fill(&[(19.0, 1.0), (20.2, 5.3), (19.0, 9.6), (17.8, 5.3)], p.acc);
            p.fill(&[(14.7, 5.3), (19.0, 4.1), (23.3, 5.3), (19.0, 6.5)], p.acc);
        }
        "history" => {
            p.circle(12.5, 12.0, 8.5, Some(p.tint(40)), Some((1.4, ink)));
            p.line(&[(12.5, 7.0), (12.5, 12.0), (16.0, 14.0)], 1.6, ink);
            p.fill(&[(1.5, 9.5), (6.0, 9.5), (3.5, 13.5)], ink);
        }
        "time_slider" => {
            // A clock over a slider track with its thumb.
            p.circle(12.0, 9.0, 7.0, Some(p.tint(40)), Some((1.4, ink)));
            p.line(&[(12.0, 5.0), (12.0, 9.0), (15.0, 10.5)], 1.5, ink);
            p.line(&[(2.0, 20.0), (22.0, 20.0)], 1.6, ink);
            p.rect(14.0, 17.5, 3.5, 5.0, Some(p.acc), None, 1.0);
        }
        "time_off" => {
            p.circle(12.0, 12.0, 8.0, Some(p.tint(30)), Some((1.4, ink)));
            p.line(&[(12.0, 7.0), (12.0, 12.0), (15.5, 14.0)], 1.5, ink);
            p.line(&[(4.0, 4.0), (20.0, 20.0)], 1.8, red);
        }
        "time_first" | "time_last" => {
            let flip = |x: f32| if name == "time_last" { 24.0 - x } else { x };
            p.line(&[(flip(6.0), 6.0), (flip(6.0), 18.0)], 2.0, ink);
            p.fill(&[(flip(18.0), 6.0), (flip(8.0), 12.0), (flip(18.0), 18.0)], ink);
        }
        "time_prev" => p.fill(&[(17.0, 6.0), (7.0, 12.0), (17.0, 18.0)], ink),
        "time_next" => p.fill(&[(7.0, 6.0), (17.0, 12.0), (7.0, 18.0)], ink),
        "time_pause" => {
            p.rect(6.5, 5.0, 4.0, 14.0, Some(ink), None, 1.0);
            p.rect(13.5, 5.0, 4.0, 14.0, Some(ink), None, 1.0);
        }
        "environments" | "settings" => {
            for i in 0..8 {
                let a = (i as f32 * 45.0).to_radians();
                p.line(&[(12.0 + 6.5 * a.cos(), 12.0 + 6.5 * a.sin()), (12.0 + 9.5 * a.cos(), 12.0 + 9.5 * a.sin())], 2.8, ink);
            }
            p.circle(12.0, 12.0, 7.0, Some(p.tint(110)), Some((1.4, ink)));
            p.circle(12.0, 12.0, 2.6, Some(Color32::WHITE), Some((1.2, ink)));
        }
        "buffer" => {
            p.circle(12.0, 12.0, 9.5, Some(p.tint(40)), Some((1.0, ink)));
            p.circle(12.0, 12.0, 6.0, Some(p.tint(90)), Some((1.0, ink)));
            p.circle(12.0, 12.0, 2.2, Some(ink), None);
        }
        "clip" => {
            p.polygon_shape(&[(2.0, 4.0), (16.0, 3.0), (20.0, 14.0), (9.0, 20.0)], 60);
            p.dashed_rect(8.0, 8.0, 13.0, 13.0, red);
            p.rect(8.0, 8.0, 13.0, 13.0, Some(p.tint(110)), None, 0.0);
        }
        "erase" => {
            p.polygon_shape(&[(2.0, 4.0), (16.0, 3.0), (20.0, 14.0), (9.0, 20.0)], 110);
            p.circle(16.0, 16.0, 6.0, Some(Color32::WHITE), Some((1.3, red)));
        }
        "intersect" => {
            // Two overlapping polygons, only their shared part filled.
            p.rect(9.0, 9.0, 6.0, 6.0, Some(accent), None, 0.0);
            p.rect(3.0, 3.0, 12.0, 12.0, None, Some((1.3, ink)), 0.0);
            p.rect(9.0, 9.0, 12.0, 12.0, None, Some((1.3, ink)), 0.0);
        }
        "identity" => {
            // Intersect's two squares, with all of the input's square kept: the overlap marked, the
            // rest of the input lighter.
            p.rect(3.0, 3.0, 12.0, 12.0, Some(p.tint(60)), None, 0.0);
            p.rect(9.0, 9.0, 6.0, 6.0, Some(accent), None, 0.0);
            p.rect(3.0, 3.0, 12.0, 12.0, None, Some((1.3, ink)), 0.0);
            p.rect(9.0, 9.0, 12.0, 12.0, None, Some((1.3, ink)), 0.0);
        }
        "union" => {
            // Both squares kept, their shared part marked.
            p.rect(3.0, 3.0, 12.0, 12.0, Some(p.tint(60)), None, 0.0);
            p.rect(9.0, 9.0, 12.0, 12.0, Some(p.tint(60)), None, 0.0);
            p.rect(9.0, 9.0, 6.0, 6.0, Some(accent), None, 0.0);
            p.rect(3.0, 3.0, 12.0, 12.0, None, Some((1.3, ink)), 0.0);
            p.rect(9.0, 9.0, 12.0, 12.0, None, Some((1.3, ink)), 0.0);
        }
        "symDiff" => {
            // Both squares kept but their shared part, left empty.
            p.rect(3.0, 3.0, 12.0, 12.0, Some(accent), None, 0.0);
            p.rect(9.0, 9.0, 12.0, 12.0, Some(accent), None, 0.0);
            p.rect(9.0, 9.0, 6.0, 6.0, Some(p.paper()), None, 0.0);
            p.rect(3.0, 3.0, 12.0, 12.0, None, Some((1.3, ink)), 0.0);
            p.rect(9.0, 9.0, 12.0, 12.0, None, Some((1.3, ink)), 0.0);
        }
        "updatePolygons" => {
            // The input's square, the update's square laid over it whole.
            p.rect(3.0, 3.0, 12.0, 12.0, Some(p.tint(60)), Some((1.3, ink)), 0.0);
            p.rect(9.0, 9.0, 12.0, 12.0, Some(accent), Some((1.3, ink)), 0.0);
        }
        "split" => {
            // One shape cut in two along a line, its halves apart.
            p.polygon_shape(&[(2.0, 5.0), (10.5, 3.0), (10.5, 20.0), (4.0, 18.0)], 110);
            p.polygon_shape(&[(13.5, 3.0), (22.0, 6.0), (20.0, 19.0), (13.5, 20.0)], 60);
            p.line(&[(12.0, 1.0), (12.0, 23.0)], 1.3, red);
        }
        "near" => {
            // A point, and the shortest line from it to a polygon.
            p.polygon_shape(&[(12.0, 3.0), (21.0, 4.0), (20.0, 12.0), (13.0, 11.0)], 60);
            p.line(&[(6.0, 18.0), (13.0, 11.0)], 1.2, ink);
            p.circle(13.0, 11.0, 1.6, Some(ink), None);
            p.circle(6.0, 18.0, 2.8, Some(red), Some((1.0, Color32::WHITE)));
        }
        "dissolve" => {
            p.polygon_shape(&[(2.0, 6.0), (12.0, 3.0), (22.0, 6.0), (20.0, 19.0), (4.0, 19.0)], 110);
            p.line(&[(12.0, 3.5), (12.0, 18.5)], 1.0, ink.gamma_multiply(0.35));
            p.line(&[(3.0, 12.0), (21.0, 12.0)], 1.0, ink.gamma_multiply(0.35));
        }
        "hull" => {
            p.polygon_shape(&[(3.0, 14.0), (8.0, 3.0), (19.0, 5.0), (22.0, 16.0), (12.0, 21.0)], 50);
            for (x, y) in [(7.0, 9.0), (12.0, 7.0), (16.0, 12.0), (10.0, 15.0), (18.0, 8.0)] {
                p.circle(x, y, 1.7, Some(accent), None);
            }
        }
        "centroid" => {
            p.polygon_shape(&[(3.0, 8.0), (11.0, 3.0), (21.0, 7.0), (19.0, 19.0), (6.0, 20.0)], 60);
            p.circle(12.0, 12.0, 2.8, Some(red), Some((1.0, Color32::WHITE)));
        }
        "simplify" => {
            p.line(
                &[
                    (2.0, 16.0),
                    (4.0, 10.0),
                    (6.0, 13.0),
                    (8.0, 6.0),
                    (10.0, 11.0),
                    (12.0, 8.0),
                    (14.0, 14.0),
                    (16.0, 9.0),
                    (18.0, 12.0),
                    (22.0, 5.0),
                ],
                1.0,
                ink.gamma_multiply(0.4),
            );
            p.line(&[(2.0, 16.0), (8.0, 6.0), (14.0, 14.0), (22.0, 5.0)], 2.0, accent);
        }
        "stats" | "chart" => {
            p.line(&[(3.0, 3.0), (3.0, 21.0), (21.0, 21.0)], 1.4, ink);
            for (i, h) in [8.0, 13.0, 6.0, 16.0].iter().enumerate() {
                let x = 5.5 + i as f32 * 4.0;
                p.rect(x, 20.0 - h, 3.0, *h, Some(if i == 3 { accent } else { p.tint(120) }), None, 0.5);
            }
        }
        "count" => {
            p.polygon_shape(&[(3.0, 6.0), (14.0, 3.0), (20.0, 12.0), (11.0, 20.0), (3.0, 16.0)], 50);
            for (x, y) in [(7.0, 8.0), (11.0, 12.0), (8.0, 15.0), (14.0, 8.0)] {
                p.circle(x, y, 1.6, Some(ink), None);
            }
            p.circle(19.0, 19.0, 4.5, Some(accent), None);
            p.text(19.0, 19.2, "4", 6.5, Color32::WHITE);
        }
        "calc_geom" => {
            p.polygon_shape(&[(3.0, 4.0), (16.0, 3.0), (17.0, 15.0), (4.0, 16.0)], 60);
            p.rect(11.0, 13.0, 11.0, 8.0, Some(Color32::from_gray(250)), Some((1.2, ink)), 1.0);
            p.text(16.5, 17.2, "km²", 4.6, ink);
        }
        "table" => p.table(2.0, 4.0, 20.0, 16.0),
        "field" => {
            p.table(2.0, 4.0, 15.0, 16.0);
            p.plus_badge(19.0, 18.5);
        }
        "calculate" => {
            p.rect(4.0, 2.0, 16.0, 20.0, Some(p.tint(60)), Some((1.3, ink)), 2.0);
            p.rect(6.5, 4.5, 11.0, 5.0, Some(Color32::WHITE), Some((1.0, ink)), 0.5);
            for (i, j) in [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)] {
                p.rect(6.5 + i as f32 * 4.0, 12.0 + j as f32 * 4.5, 3.0, 3.0, Some(ink), None, 0.5);
            }
        }
        "def_query" | "filter" => {
            p.fill(&[(3.0, 4.0), (21.0, 4.0), (14.0, 12.0), (10.0, 12.0)], accent);
            p.rect(10.0, 11.5, 4.0, 8.5, Some(accent), None, 0.0);
            p.closed(&[(3.0, 4.0), (21.0, 4.0), (14.0, 12.0), (14.0, 20.0), (10.0, 20.0), (10.0, 12.0)], 1.3, ink);
        }
        "export_map" | "export" => {
            p.rect(2.0, 4.0, 16.0, 14.0, Some(p.tint(60)), Some((1.3, ink)), 1.0);
            p.polygon_shape(&[(4.0, 15.0), (8.0, 9.0), (11.0, 13.0), (13.0, 11.0), (16.0, 15.0)], 160);
            p.line(&[(15.0, 20.0), (22.0, 13.0)], 1.8, green);
            p.line(&[(17.5, 13.0), (22.0, 13.0), (22.0, 17.5)], 1.8, green);
        }
        "export_layer" => {
            p.layers(1.0, 4.0, 0.85);
            p.line(&[(14.0, 20.0), (21.5, 12.5)], 1.8, green);
            p.line(&[(17.0, 12.5), (21.5, 12.5), (21.5, 17.0)], 1.8, green);
        }
        "web_map" | "share" => {
            p.circle(8.0, 13.0, 5.0, Some(p.tint(120)), None);
            p.circle(14.0, 9.5, 6.5, Some(p.tint(120)), None);
            p.circle(18.0, 14.0, 4.5, Some(p.tint(120)), None);
            p.rect(5.0, 13.0, 16.0, 5.5, Some(p.tint(120)), None, 2.0);
            p.arc(14.0, 9.5, 6.5, 190.0, 340.0, 1.3, ink);
            p.line(&[(4.5, 18.5), (20.5, 18.5)], 1.3, ink);
        }
        "print" => {
            p.rect(6.0, 2.5, 12.0, 6.0, Some(Color32::WHITE), Some((1.2, ink)), 0.5);
            p.rect(2.0, 8.0, 20.0, 9.0, Some(p.tint(110)), Some((1.3, ink)), 2.0);
            p.rect(6.0, 14.0, 12.0, 7.5, Some(Color32::WHITE), Some((1.2, ink)), 0.5);
        }
        "package" => {
            p.fill(&[(3.0, 7.0), (12.0, 3.0), (21.0, 7.0), (12.0, 11.0)], p.tint(150));
            p.fill(&[(3.0, 7.0), (12.0, 11.0), (12.0, 21.0), (3.0, 17.0)], amber);
            p.fill(&[(12.0, 11.0), (21.0, 7.0), (21.0, 17.0), (12.0, 21.0)], Color32::from_rgb(0xC9, 0x86, 0x10));
            p.closed(&[(3.0, 7.0), (12.0, 3.0), (21.0, 7.0), (21.0, 17.0), (12.0, 21.0), (3.0, 17.0)], 1.3, ink);
            p.line(&[(3.0, 7.0), (12.0, 11.0), (21.0, 7.0)], 1.1, ink);
            p.line(&[(12.0, 11.0), (12.0, 21.0)], 1.1, ink);
        }
        "contents" => {
            for (i, c) in [accent, green, amber].iter().enumerate() {
                let y = 4.0 + i as f32 * 6.0;
                p.rect(3.0, y, 4.0, 4.0, Some(*c), Some((1.0, ink)), 0.5);
                p.line(&[(9.5, y + 2.0), (21.0, y + 2.0)], 1.5, ink);
            }
        }
        "catalog" => {
            p.rect(3.0, 3.0, 7.5, 7.5, Some(p.tint(150)), Some((1.2, ink)), 1.0);
            p.rect(13.5, 3.0, 7.5, 7.5, Some(amber), Some((1.2, ink)), 1.0);
            p.rect(3.0, 13.5, 7.5, 7.5, Some(green), Some((1.2, ink)), 1.0);
            p.rect(13.5, 13.5, 7.5, 7.5, Some(red), Some((1.2, ink)), 1.0);
        }
        "symbology" => {
            p.circle(12.0, 12.0, 9.5, Some(Color32::from_gray(245)), Some((1.3, ink)));
            for (x, y, c) in [(8.0, 8.0, red), (13.0, 6.5, amber), (17.0, 10.0, green), (16.0, 15.5, accent)] {
                p.circle(x, y, 2.2, Some(c), None);
            }
            p.circle(9.0, 15.0, 2.6, Some(Color32::WHITE), Some((1.0, ink)));
        }
        "transparency" => {
            for i in 0..4 {
                for j in 0..4 {
                    if (i + j) % 2 == 0 {
                        p.rect(3.0 + i as f32 * 4.5, 3.0 + j as f32 * 4.5, 4.5, 4.5, Some(ink.gamma_multiply(0.35)), None, 0.0);
                    }
                }
            }
            p.rect(3.0, 3.0, 18.0, 18.0, Some(p.tint(110)), Some((1.3, ink)), 1.0);
        }
        "scale_range" => {
            p.globe(12.0, 9.0, 5.5);
            p.line(&[(3.0, 19.0), (21.0, 19.0)], 1.6, ink);
            p.rect(8.0, 17.0, 3.0, 4.0, Some(accent), None, 0.5);
            p.rect(14.0, 17.0, 3.0, 4.0, Some(accent), None, 0.5);
        }
        "save" => {
            p.rect(3.0, 3.0, 18.0, 18.0, Some(accent), Some((1.3, ink)), 2.0);
            p.rect(7.0, 3.0, 10.0, 6.0, Some(Color32::WHITE), None, 0.0);
            p.rect(6.0, 13.0, 12.0, 8.0, Some(Color32::WHITE), None, 0.5);
        }
        "save_as" => {
            paint(painter, Rect::from_min_size(r.min, r.size() * 0.85), "save", ink, accent);
            p.line(&[(13.0, 21.0), (21.0, 13.0)], 2.2, amber);
        }
        "new" => {
            p.page(5.0, 2.0, 14.0, 20.0);
            p.plus_badge(18.0, 18.0);
        }
        "undo" | "redo" => {
            let d = if name == "undo" { 1.0 } else { -1.0 };
            let cx = 12.0;
            let pts: Vec<(f32, f32)> = (0..=16)
                .map(|i| {
                    let a = (-90.0 + 180.0 * i as f32 / 16.0f32).to_radians();
                    (cx + d * 6.5 * a.cos(), 13.0 + 6.0 * a.sin())
                })
                .collect();
            p.line(&pts, 1.8, ink);
            p.line(&[(cx, 19.0), (cx - d * 6.0, 19.0)], 1.8, ink);
            p.fill(&[(cx - d * 1.0, 3.5), (cx - d * 1.0, 10.5), (cx - d * 5.5, 7.0)], ink);
        }
        "delete" => {
            p.rect(5.0, 6.0, 14.0, 16.0, Some(p.tint(50)), Some((1.3, ink)), 1.5);
            p.line(&[(3.0, 6.0), (21.0, 6.0)], 1.6, ink);
            p.line(&[(9.0, 6.0), (9.0, 3.0), (15.0, 3.0), (15.0, 6.0)], 1.3, ink);
            for x in [9.5, 12.0, 14.5] {
                p.line(&[(x, 9.5), (x, 18.5)], 1.1, ink);
            }
        }
        "create" | "edit" => {
            p.fill(&[(4.0, 16.5), (15.5, 5.0), (19.0, 8.5), (7.5, 20.0)], amber);
            p.fill(&[(15.5, 5.0), (17.5, 3.0), (21.0, 6.5), (19.0, 8.5)], red);
            p.closed(&[(4.0, 16.5), (17.5, 3.0), (21.0, 6.5), (7.5, 20.0), (3.0, 21.0)], 1.3, ink);
            if name == "create" {
                p.plus_badge(6.0, 6.0);
            }
        }
        "point_tool" => {
            p.circle(12.0, 12.0, 4.5, Some(accent), Some((1.5, Color32::WHITE)));
            p.circle(12.0, 12.0, 6.0, None, Some((1.0, ink)));
        }
        "line_tool" => {
            p.line(&[(3.0, 18.0), (9.0, 8.0), (15.0, 15.0), (21.0, 5.0)], 2.2, accent);
            for (x, y) in [(3.0, 18.0), (9.0, 8.0), (15.0, 15.0), (21.0, 5.0)] {
                p.rect(x - 1.6, y - 1.6, 3.2, 3.2, Some(Color32::WHITE), Some((1.0, ink)), 0.0);
            }
        }
        "polygon_tool" => {
            p.polygon_shape(&[(3.0, 17.0), (6.0, 5.0), (18.0, 3.0), (21.0, 15.0), (12.0, 21.0)], 120);
            for (x, y) in [(3.0, 17.0), (6.0, 5.0), (18.0, 3.0), (21.0, 15.0), (12.0, 21.0)] {
                p.rect(x - 1.6, y - 1.6, 3.2, 3.2, Some(Color32::WHITE), Some((1.0, ink)), 0.0);
            }
        }
        "vertices" => {
            // A shape with one vertex dragged out of place.
            p.line(&[(3.0, 19.0), (6.0, 6.0), (12.0, 11.0)], 1.4, ink);
            p.line(&[(12.0, 11.0), (21.0, 19.0), (3.0, 19.0)], 1.4, ink);
            p.line(&[(12.0, 11.0), (17.0, 3.0)], 1.0, accent);
            for (x, y) in [(3.0, 19.0), (6.0, 6.0), (21.0, 19.0), (12.0, 11.0)] {
                p.rect(x - 1.6, y - 1.6, 3.2, 3.2, Some(Color32::WHITE), Some((1.0, ink)), 0.0);
            }
            p.rect(15.0, 1.0, 4.0, 4.0, Some(accent), Some((1.0, Color32::WHITE)), 0.0);
        }
        "dark" => {
            p.circle(12.0, 12.0, 8.0, Some(Color32::from_rgb(0x3B, 0x44, 0x60)), None);
            p.circle(15.5, 9.0, 6.5, Some(if ink.r() > 128 { Color32::from_rgb(0x24, 0x29, 0x31) } else { Color32::WHITE }), None);
        }
        "graticule" => {
            p.circle(12.0, 12.0, 9.0, Some(p.tint(40)), Some((1.3, ink)));
            for y in [7.0, 12.0, 17.0] {
                p.line(&[(4.0, y), (20.0, y)], 0.9, ink);
            }
            for x in [7.0, 12.0, 17.0] {
                p.line(&[(x, 3.5), (x, 20.5)], 0.9, ink);
            }
        }
        "search" => p.magnifier(10.0, 10.0, 6.0),
        "command" => {
            p.rect(2.0, 5.0, 20.0, 14.0, Some(p.tint(40)), Some((1.3, ink)), 3.0);
            p.magnifier(9.0, 11.0, 3.3);
            p.line(&[(15.0, 12.0), (19.0, 12.0)], 1.4, ink);
        }
        "pane" => {
            p.rect(2.0, 4.0, 20.0, 16.0, None, Some((1.3, ink)), 1.5);
            p.rect(2.0, 4.0, 7.0, 16.0, Some(p.tint(110)), Some((1.3, ink)), 1.5);
        }
        "pane_right" => {
            p.rect(2.0, 4.0, 20.0, 16.0, None, Some((1.3, ink)), 1.5);
            p.rect(15.0, 4.0, 7.0, 16.0, Some(p.tint(110)), Some((1.3, ink)), 1.5);
        }
        "unique" => {
            for (i, c) in [accent, amber, green, red].iter().enumerate() {
                let (x, y) = (3.0 + (i % 2) as f32 * 10.0, 3.0 + (i / 2) as f32 * 10.0);
                p.rect(x, y, 8.0, 8.0, Some(*c), Some((1.0, ink)), 1.5);
            }
        }
        "graduated" => {
            for i in 0..5 {
                p.rect(2.5 + i as f32 * 4.0, 5.0, 3.6, 14.0, Some(teal.gamma_multiply(0.2 + i as f32 * 0.2)), None, 0.5);
            }
            p.rect(2.5, 5.0, 19.6, 14.0, None, Some((1.2, ink)), 0.5);
        }
        "proportional" => {
            for (x, y, rr) in [(5.0, 18.0, 2.5), (11.0, 15.0, 4.0), (17.5, 10.5, 5.5)] {
                p.circle(x, y, rr, Some(accent.gamma_multiply(0.7)), Some((1.0, ink)));
            }
        }
        "single" => {
            p.rect(4.0, 4.0, 16.0, 16.0, Some(accent), Some((1.3, ink)), 2.0);
        }
        "eye" => {
            p.arc(12.0, 18.0, 10.0, 210.0, 330.0, 1.5, ink);
            p.arc(12.0, 6.0, 10.0, 30.0, 150.0, 1.5, ink);
            p.circle(12.0, 12.0, 3.0, Some(accent), None);
        }
        "close" => {
            p.line(&[(6.0, 6.0), (18.0, 18.0)], 1.8, ink);
            p.line(&[(18.0, 6.0), (6.0, 18.0)], 1.8, ink);
        }
        "dropdown" => p.fill(&[(7.0, 9.5), (17.0, 9.5), (12.0, 15.0)], ink),
        "chevron_right" => p.line(&[(9.0, 6.0), (15.0, 12.0), (9.0, 18.0)], 1.8, ink),
        "chevron_left" => p.line(&[(15.0, 6.0), (9.0, 12.0), (15.0, 18.0)], 1.8, ink),
        "chevron_down" => p.line(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)], 1.8, ink),
        "globe_lang" => {
            p.globe(9.5, 9.5, 7.0);
            p.rect(11.5, 11.5, 11.0, 11.0, Some(accent), None, 2.5);
            p.text(17.0, 17.2, "A", 7.5, Color32::WHITE);
        }
        "back" => {
            p.line(&[(20.0, 12.0), (4.5, 12.0)], 2.0, ink);
            p.line(&[(10.5, 6.0), (4.5, 12.0), (10.5, 18.0)], 2.0, ink);
        }
        "run" => p.fill(&[(7.0, 4.0), (20.0, 12.0), (7.0, 20.0)], green),
        "update" => {
            // A circular arrow around a downward arrow: fetch the newer version.
            p.arc(12.0, 12.0, 8.0, 40.0, 320.0, 1.7, ink);
            p.line(&[(15.6, 5.2), (18.4, 6.9), (18.0, 3.7)], 1.7, ink);
            p.line(&[(12.0, 8.0), (12.0, 15.0)], 1.8, p.acc);
            p.line(&[(9.2, 12.4), (12.0, 15.2), (14.8, 12.4)], 1.8, p.acc);
        }
        "up" => p.line(&[(6.0, 15.0), (12.0, 9.0), (18.0, 15.0)], 1.8, ink),
        "down" => p.line(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)], 1.8, ink),
        "logo" => {
            // A small version of the RustGIS logo: a rust cog around a four-colour globe.
            let rust = crate::theme::BRAND;
            for k in 0..12 {
                let a = (k as f32 * 30.0).to_radians();
                let (c, s2) = (a.cos(), a.sin());
                let q = |r: f32, w: f32| (12.0 + r * c - w * s2, 12.0 + r * s2 + w * c);
                p.fill(&[q(9.0, -1.6), q(11.6, -1.1), q(11.6, 1.1), q(9.0, 1.6)], rust);
            }
            p.circle(12.0, 12.0, 9.8, Some(rust), None);
            p.circle(12.0, 12.0, 8.0, Some(Color32::WHITE), None);
            let wedge = |a0: f32, a1: f32, col: Color32| {
                let n = 8;
                for k in 0..n {
                    let (b0, b1) = (a0 + (a1 - a0) * k as f32 / n as f32, a0 + (a1 - a0) * (k + 1) as f32 / n as f32);
                    p.fill(
                        &[
                            (12.0, 12.0),
                            (12.0 + 7.0 * b0.to_radians().cos(), 12.0 + 7.0 * b0.to_radians().sin()),
                            (12.0 + 7.0 * b1.to_radians().cos(), 12.0 + 7.0 * b1.to_radians().sin()),
                        ],
                        col,
                    );
                }
            };
            wedge(180.0, 275.0, Color32::from_rgb(0x2D, 0x76, 0xC2));
            wedge(275.0, 365.0, Color32::from_rgb(0x18, 0x9D, 0x8C));
            wedge(5.0, 95.0, Color32::from_rgb(0xEE, 0xAE, 0x3A));
            wedge(95.0, 180.0, Color32::from_rgb(0x6C, 0xB3, 0x3F));
            p.line(&[(11.5, 4.5), (11.0, 12.0), (12.5, 19.5)], 1.2, Color32::WHITE);
            p.line(&[(4.5, 12.6), (11.0, 12.0), (19.5, 11.0)], 1.2, Color32::WHITE);
        }
        "point_layer" => p.circle(12.0, 12.0, 5.0, Some(accent), Some((1.2, ink))),
        "line_layer" => p.line(&[(3.0, 17.0), (9.0, 8.0), (15.0, 15.0), (21.0, 6.0)], 2.2, accent),
        "polygon_layer" => p.polygon_shape(&[(3.0, 8.0), (11.0, 3.0), (21.0, 7.0), (19.0, 20.0), (5.0, 19.0)], 180),
        // A lit globe with an orbit: the 3D view.
        "globe_view" => {
            p.circle(11.0, 12.0, 8.0, Some(teal.gamma_multiply(0.85)), None);
            p.circle(9.0, 9.5, 4.0, Some(Color32::from_white_alpha(70)), None);
            p.globe(11.0, 12.0, 8.0);
            let orbit: Vec<(f32, f32)> = (0..=24)
                .map(|i| {
                    let a = (i as f32 * 15.0).to_radians();
                    (11.0 + 11.5 * a.cos(), 12.0 + 3.6 * a.sin() - 2.0 * a.cos())
                })
                .collect();
            p.line(&orbit, 1.3, amber);
        }
        // A ringed planet: the celestial body switcher.
        "planet" => {
            let ring = |a0: f32, a1: f32| -> Vec<(f32, f32)> {
                (0..=16)
                    .map(|i| {
                        let a = (a0 + (a1 - a0) * i as f32 / 16.0).to_radians();
                        (12.0 + 10.5 * a.cos(), 12.5 + 3.4 * a.sin() - 2.6 * a.cos())
                    })
                    .collect()
            };
            // The ring's far half behind the planet, its near half in front.
            p.line(&ring(180.0, 360.0), 1.3, ink);
            p.circle(12.0, 12.0, 6.5, Some(amber.gamma_multiply(0.9)), Some((1.3, ink)));
            p.circle(10.0, 9.8, 2.2, Some(Color32::from_white_alpha(80)), None);
            p.line(&ring(0.0, 180.0), 1.3, ink);
        }
        // A grid of cells shaded low to high.
        "raster_layer" => {
            for (i, a) in [40u8, 90, 150, 70, 130, 200, 110, 180, 240].into_iter().enumerate() {
                let (x, y) = (4.0 + (i % 3) as f32 * 5.4, 4.0 + (i / 3) as f32 * 5.4);
                p.rect(x, y, 5.4, 5.4, Some(teal.gamma_multiply(a as f32 / 255.0)), None, 0.0);
            }
            p.rect(4.0, 4.0, 16.2, 16.2, None, Some((1.2, ink)), 1.0);
        }
        // A smooth ramp.
        "stretch" => {
            for i in 0..8 {
                p.rect(3.0 + i as f32 * 2.25, 6.0, 2.4, 12.0, Some(Color32::from_gray((20 + i * 32) as u8)), None, 0.0);
            }
            p.rect(3.0, 6.0, 18.0, 12.0, None, Some((1.2, ink)), 1.0);
        }
        "rgb" => {
            p.circle(9.0, 9.5, 5.0, Some(red.gamma_multiply(0.75)), None);
            p.circle(15.0, 9.5, 5.0, Some(green.gamma_multiply(0.75)), None);
            p.circle(12.0, 14.5, 5.0, Some(Color32::from_rgb(0x2E, 0x6F, 0xD8).gamma_multiply(0.75)), None);
        }
        "classify" => {
            for (i, a) in [0.25f32, 0.5, 0.75, 1.0].into_iter().enumerate() {
                p.rect(3.0 + i as f32 * 4.6, 6.0, 4.6, 12.0, Some(amber.gamma_multiply(a)), None, 0.0);
            }
            p.rect(3.0, 6.0, 18.4, 12.0, None, Some((1.2, ink)), 1.0);
        }
        // A lit and a shaded mountain face.
        "relief" => {
            p.fill(&[(2.0, 20.0), (10.0, 6.0), (10.0, 20.0)], Color32::from_gray(225));
            p.fill(&[(10.0, 6.0), (18.0, 20.0), (10.0, 20.0)], Color32::from_gray(110));
            p.fill(&[(12.0, 20.0), (17.0, 11.0), (22.0, 20.0)], Color32::from_gray(160));
            p.line(&[(2.0, 20.0), (10.0, 6.0), (14.0, 13.0), (17.0, 11.0), (22.0, 20.0)], 1.3, ink);
            p.circle(19.0, 5.0, 2.2, Some(amber), None);
        }
        // A histogram with a view window.
        "dra" => {
            for (i, hgt) in [3.0f32, 7.0, 12.0, 9.0, 5.0, 2.0].into_iter().enumerate() {
                p.rect(3.0 + i as f32 * 3.0, 19.0 - hgt, 2.4, hgt, Some(p.tint(150)), None, 0.0);
            }
            p.rect(6.0, 4.0, 9.0, 16.0, None, Some((1.4, ink)), 1.5);
        }
        "bands" => {
            for (i, c) in [red, green, Color32::from_rgb(0x2E, 0x6F, 0xD8)].into_iter().enumerate() {
                let o = i as f32 * 3.5;
                p.rect(3.0 + o, 3.0 + o, 11.0, 11.0, Some(c.gamma_multiply(0.85)), Some((1.0, ink)), 1.5);
            }
        }
        "resample" => {
            for i in 0..4 {
                p.line(&[(4.0 + i as f32 * 5.3, 4.0), (4.0 + i as f32 * 5.3, 20.0)], 1.0, ink.gamma_multiply(0.5));
                p.line(&[(4.0, 4.0 + i as f32 * 5.3), (20.0, 4.0 + i as f32 * 5.3)], 1.0, ink.gamma_multiply(0.5));
            }
            p.circle(12.0, 12.0, 3.5, Some(accent), Some((1.2, ink)));
        }
        "brightness" => {
            p.circle(12.0, 12.0, 4.0, Some(amber), Some((1.2, ink)));
            for i in 0..8 {
                let a = (i as f32 * 45.0).to_radians();
                p.line(&[(12.0 + 6.5 * a.cos(), 12.0 + 6.5 * a.sin()), (12.0 + 9.0 * a.cos(), 12.0 + 9.0 * a.sin())], 1.5, ink);
            }
        }
        "contrast" => {
            p.circle(12.0, 12.0, 8.5, Some(Color32::WHITE), Some((1.3, ink)));
            p.fill(
                &(0..=12)
                    .map(|i| {
                        let a = (-90.0 + i as f32 * 15.0).to_radians();
                        (12.0 + 8.5 * a.cos(), 12.0 + 8.5 * a.sin())
                    })
                    .collect::<Vec<_>>(),
                ink,
            );
        }
        "gamma" => {
            p.line(
                &(0..=12)
                    .map(|i| {
                        let t = i as f32 / 12.0;
                        (4.0 + 16.0 * t, 20.0 - 16.0 * t.powf(0.5))
                    })
                    .collect::<Vec<_>>(),
                2.0,
                accent,
            );
            p.line(&[(4.0, 4.0), (4.0, 20.0), (20.0, 20.0)], 1.2, ink);
        }
        "paste" => {
            p.rect(4.0, 4.0, 13.0, 17.0, Some(p.tint(60)), Some((1.3, ink)), 1.5);
            p.rect(7.5, 2.5, 6.0, 3.5, Some(accent), Some((1.1, ink)), 1.0);
            p.page(11.0, 9.0, 10.0, 13.0);
        }
        "copy" => {
            p.page(3.0, 3.0, 11.0, 14.0);
            p.page(9.0, 7.0, 11.0, 14.0);
            p.rect(11.5, 12.0, 6.0, 1.2, Some(accent), None, 0.5);
            p.rect(11.5, 15.0, 6.0, 1.2, Some(accent), None, 0.5);
        }
        "cut" => {
            p.circle(7.0, 17.0, 3.2, Some(p.tint(60)), Some((1.4, ink)));
            p.circle(17.0, 17.0, 3.2, Some(p.tint(60)), Some((1.4, ink)));
            p.line(&[(9.0, 14.5), (16.0, 3.0)], 1.6, ink);
            p.line(&[(15.0, 14.5), (8.0, 3.0)], 1.6, ink);
        }
        "copy_path" => {
            p.page(3.0, 3.0, 12.0, 15.0);
            p.line(&[(6.0, 9.0), (12.0, 9.0)], 1.1, ink);
            p.line(&[(6.0, 12.0), (10.0, 12.0)], 1.1, ink);
            p.rect(10.0, 14.0, 12.0, 7.0, Some(accent), None, 2.0);
            p.text(16.0, 17.5, "C:\\", 5.0, Color32::WHITE);
        }
        "popup" => {
            p.rect(3.0, 3.0, 18.0, 12.0, Some(p.tint(50)), Some((1.3, ink)), 2.0);
            p.fill(&[(8.0, 15.0), (13.0, 15.0), (9.0, 20.0)], ink);
            p.line(&[(6.0, 7.0), (17.0, 7.0)], 1.1, ink);
            p.line(&[(6.0, 10.5), (13.0, 10.5)], 1.1, ink);
        }
        "label_lock" => {
            paint(painter, r, "label", ink, accent);
            p.rect(14.0, 14.5, 8.0, 7.0, Some(amber), Some((1.0, ink)), 1.0);
            p.arc(18.0, 14.5, 2.6, 180.0, 360.0, 1.3, ink);
        }
        "lock" => {
            p.rect(5.0, 11.0, 14.0, 10.0, Some(p.tint(60)), Some((1.4, ink)), 1.5);
            p.arc(12.0, 11.0, 4.5, 180.0, 360.0, 1.8, ink);
            p.circle(12.0, 16.0, 1.4, Some(ink), None);
        }
        "label_properties" => {
            // The label with a gear at its lower right: its settings (the Labeling pane).
            paint(painter, r, "label", ink, accent);
            // At the corner, clear of the text: the teeth are turned so that a gap, not a tooth,
            // faces the "c", and only the body reaches towards it.
            let (cx, cy) = (18.8, 18.8);
            for i in 0..8 {
                let a = (i as f32 * 45.0 + 22.5).to_radians();
                p.line(&[(cx + 3.0 * a.cos(), cy + 3.0 * a.sin()), (cx + 5.0 * a.cos(), cy + 5.0 * a.sin())], 1.9, ink);
            }
            p.circle(cx, cy, 3.6, Some(accent), Some((1.0, ink)));
            p.circle(cx, cy, 1.25, Some(Color32::WHITE), None);
        }
        "label_pause" => {
            paint(painter, r, "label", ink, accent);
            p.circle(18.0, 18.0, 4.5, Some(amber), None);
            p.line(&[(16.8, 16.0), (16.8, 20.0)], 1.4, Color32::WHITE);
            p.line(&[(19.2, 16.0), (19.2, 20.0)], 1.4, Color32::WHITE);
        }
        "label_unplaced" => {
            p.fill(&[(2.0, 7.0), (17.0, 7.0), (22.0, 12.0), (17.0, 17.0), (2.0, 17.0)], red.gamma_multiply(0.25));
            p.closed(&[(2.0, 7.0), (17.0, 7.0), (22.0, 12.0), (17.0, 17.0), (2.0, 17.0)], 1.3, red);
            p.text(10.0, 12.0, "Abc", 6.8, red);
        }
        "label_convert" => {
            paint(painter, r, "label", ink, accent);
            p.line(&[(13.0, 21.0), (21.0, 21.0)], 1.5, green);
            p.fill(&[(19.0, 18.5), (23.0, 21.0), (19.0, 23.5)], green);
        }
        "placement" => {
            p.circle(8.0, 15.0, 3.0, Some(accent), Some((1.2, ink)));
            p.rect(11.0, 4.0, 11.0, 7.0, Some(p.tint(60)), Some((1.2, ink)), 1.5);
            p.line(&[(10.0, 12.5), (12.0, 10.5)], 1.2, ink);
        }
        "scale_label" => {
            paint(painter, r, "scale_range", ink, accent);
            p.text(17.0, 6.0, "A", 7.0, ink);
        }
        "g_point" => {
            p.circle(12.0, 12.0, 5.0, Some(accent), Some((1.4, ink)));
        }
        "g_line" => {
            p.line(&[(3.0, 19.0), (10.0, 8.0), (15.0, 14.0), (21.0, 5.0)], 1.8, ink);
        }
        "g_arrow" => {
            p.line(&[(4.0, 20.0), (16.0, 8.0)], 1.8, ink);
            p.fill(&[(21.0, 3.0), (11.5, 6.5), (17.5, 12.5)], ink);
        }
        "g_curve" => {
            let pts: Vec<(f32, f32)> = (0..=20)
                .map(|i| {
                    let t = i as f32 / 20.0;
                    (3.0 + 18.0 * t, 12.0 - 7.0 * (t * std::f32::consts::TAU).sin())
                })
                .collect();
            p.line(&pts, 1.8, ink);
        }
        "g_freehand" => {
            p.line(&[(3.0, 15.0), (5.0, 10.0), (8.0, 12.0), (10.0, 7.0), (13.0, 13.0), (15.0, 9.0), (18.0, 14.0), (21.0, 8.0)], 1.6, ink);
        }
        "g_polygon" => p.polygon_shape(&[(3.0, 9.0), (11.0, 3.0), (21.0, 7.0), (18.0, 20.0), (6.0, 19.0)], 70),
        "g_lasso" => {
            p.polygon_shape(&[(4.0, 10.0), (7.0, 5.0), (13.0, 4.0), (19.0, 7.0), (20.0, 13.0), (15.0, 17.0), (8.0, 16.0)], 50);
            p.line(&[(8.0, 16.0), (7.0, 20.0), (9.0, 22.0)], 1.3, ink);
        }
        "no_fill" => p.rect(3.0, 4.0, 18.0, 16.0, None, Some((1.6, ink)), 0.0),
        "g_rect" => {
            p.rect(3.0, 6.0, 18.0, 12.0, Some(p.tint(70)), Some((1.4, ink)), 0.5);
        }
        "g_circle" => {
            p.circle(12.0, 12.0, 8.5, Some(p.tint(70)), Some((1.4, ink)));
        }
        "g_ellipse" => {
            let pts: Vec<(f32, f32)> = (0..48)
                .map(|i| {
                    let t = i as f32 / 48.0 * std::f32::consts::TAU;
                    (12.0 + 9.5 * t.cos(), 12.0 + 6.0 * t.sin())
                })
                .collect();
            p.polygon_shape(&pts, 70);
        }
        "g_text" => {
            p.text(12.0, 12.5, "A", 17.0, ink);
            p.line(&[(5.0, 21.0), (19.0, 21.0)], 1.6, accent);
        }
        "dynamic_text" => {
            p.text(9.0, 11.0, "A", 13.0, ink);
            p.rect(13.0, 13.0, 9.0, 8.0, Some(amber), None, 1.5);
            p.text(17.5, 17.0, "{}", 6.0, Color32::WHITE);
        }
        "picture" => {
            p.rect(3.0, 4.0, 18.0, 16.0, Some(p.tint(40)), Some((1.4, ink)), 1.5);
            p.fill(&[(4.0, 19.0), (10.0, 11.0), (14.0, 16.0), (16.0, 13.0), (20.0, 19.0)], green);
            p.circle(16.0, 8.5, 2.0, Some(amber), None);
        }
        "keep_tool" => {
            p.cursor(5.0, 4.0, 1.0);
            p.arc(15.0, 15.0, 5.0, 200.0, 520.0, 1.5, accent);
            p.fill(&[(19.5, 9.5), (21.5, 13.5), (17.0, 12.5)], accent);
        }
        "graphics_layer" => {
            p.layers(2.0, 4.0, 0.9);
            p.circle(17.0, 16.0, 4.0, Some(accent), Some((1.2, ink)));
            p.rect(4.0, 15.0, 6.0, 5.0, Some(amber), Some((1.0, ink)), 0.5);
        }
        "north_arrow" => {
            p.fill(&[(12.0, 4.0), (6.5, 21.0), (12.0, 17.0)], ink);
            p.closed(&[(12.0, 4.0), (17.5, 21.0), (12.0, 17.0)], 1.2, ink);
            p.text(19.5, 6.0, "N", 6.5, accent);
        }
        "scale_bar" => {
            p.rect(2.0, 11.0, 5.0, 4.0, Some(ink), None, 0.0);
            p.rect(7.0, 11.0, 5.0, 4.0, None, Some((1.0, ink)), 0.0);
            p.rect(12.0, 11.0, 5.0, 4.0, Some(ink), None, 0.0);
            p.rect(17.0, 11.0, 5.0, 4.0, None, Some((1.0, ink)), 0.0);
            p.text(12.0, 7.0, "km", 6.0, accent);
        }
        "legend" => {
            p.rect(3.0, 3.0, 18.0, 18.0, Some(p.tint(30)), Some((1.2, ink)), 2.0);
            p.rect(6.0, 6.5, 4.0, 3.0, Some(accent), None, 0.5);
            p.circle(8.0, 12.0, 1.8, Some(green), None);
            p.line(&[(6.0, 17.0), (10.0, 15.5)], 1.4, amber);
            for y in [8.0, 12.0, 16.0] {
                p.line(&[(12.0, y), (18.0, y)], 1.1, ink);
            }
        }
        // A gradient bar over its tick marks.
        "colorbar" => {
            for i in 0..9 {
                let t = i as f32 / 8.0;
                p.rect(3.0 + i as f32 * 2.0, 5.0, 2.2, 7.0, Some(accent.lerp_to_gamma(amber, t)), None, 0.0);
            }
            p.rect(3.0, 5.0, 18.0, 7.0, None, Some((1.2, ink)), 0.5);
            for x in [3.6, 12.0, 20.4] {
                p.line(&[(x, 12.0), (x, 15.0)], 1.1, ink);
                p.line(&[(x - 1.6, 18.5), (x + 1.6, 18.5)], 1.3, ink);
            }
        }
        "extension" => {
            // A puzzle piece with an on/off switch.
            p.rect(2.5, 4.5, 12.0, 11.0, Some(p.tint(40)), Some((1.3, ink)), 2.0);
            p.circle(8.5, 4.0, 2.4, Some(accent), Some((1.2, ink)));
            p.rect(10.0, 15.0, 12.0, 6.5, Some(green), Some((1.1, ink)), 3.25);
            p.circle(18.75, 18.25, 2.2, Some(Color32::WHITE), None);
        }
        "plugin" | "plugin_add" => {
            // A puzzle piece: a body with two knobs.
            p.rect(3.5, 7.5, 13.0, 13.0, Some(p.tint(40)), Some((1.3, ink)), 2.0);
            p.circle(10.0, 6.5, 2.6, Some(accent), Some((1.2, ink)));
            p.circle(17.5, 14.0, 2.6, Some(accent), Some((1.2, ink)));
            if name == "plugin_add" {
                p.plus_badge(15.0, 2.0);
            }
        }
        _ => {
            p.rect(4.0, 4.0, 16.0, 16.0, Some(p.tint(60)), Some((1.2, ink)), 3.0);
        }
    }
}

/// Every icon name, for the gallery test.
pub const ALL: &[&str] = &[
    "directions",
    "maneuver_straight",
    "maneuver_slight_right",
    "maneuver_right",
    "maneuver_sharp_right",
    "maneuver_uturn",
    "maneuver_slight_left",
    "maneuver_left",
    "maneuver_sharp_left",
    "maneuver_depart",
    "maneuver_arrive",
    "maneuver_roundabout",
    "maneuver_merge",
    "plugin",
    "plugin_add",
    "extension",
    "explore",
    "zoom_full",
    "zoom_world",
    "zoom_in",
    "zoom_out",
    "zoom_box",
    "prev_extent",
    "next_extent",
    "bookmark",
    "bookmark_new",
    "bookmark_export",
    "bookmark_import",
    "bookmark_update",
    "goto_xy",
    "locate",
    "basemap",
    "add_data",
    "web_data",
    "add_folder",
    "open",
    "select",
    "select_attr",
    "select_loc",
    "clear_sel",
    "zoom_sel",
    "measure",
    "measure_area",
    "identify",
    "message_log",
    "about",
    "help",
    "label",
    "label_off",
    "label_properties",
    "new_map",
    "map",
    "new_layout",
    "toolbox",
    "tools",
    "geoprocessing",
    "modelbuilder",
    "python",
    "history",
    "time_slider",
    "time_off",
    "time_first",
    "time_last",
    "time_prev",
    "time_next",
    "time_pause",
    "environments",
    "settings",
    "buffer",
    "clip",
    "erase",
    "intersect",
    "identity",
    "union",
    "symDiff",
    "updatePolygons",
    "split",
    "near",
    "dissolve",
    "hull",
    "centroid",
    "simplify",
    "stats",
    "chart",
    "colorbar",
    "count",
    "calc_geom",
    "table",
    "field",
    "calculate",
    "def_query",
    "filter",
    "export_map",
    "export",
    "export_layer",
    "web_map",
    "share",
    "print",
    "package",
    "contents",
    "catalog",
    "symbology",
    "transparency",
    "scale_range",
    "save",
    "save_as",
    "new",
    "undo",
    "redo",
    "delete",
    "create",
    "edit",
    "point_tool",
    "line_tool",
    "polygon_tool",
    "vertices",
    "dark",
    "graticule",
    "search",
    "command",
    "pane",
    "pane_right",
    "unique",
    "graduated",
    "proportional",
    "single",
    "eye",
    "close",
    "back",
    "globe_lang",
    "dropdown",
    "chevron_right",
    "chevron_left",
    "chevron_down",
    "run",
    "up",
    "down",
    "logo",
    "point_layer",
    "line_layer",
    "polygon_layer",
    "globe_view",
    "planet",
    "raster_layer",
    "stretch",
    "rgb",
    "classify",
    "relief",
    "dra",
    "bands",
    "resample",
    "brightness",
    "contrast",
    "gamma",
];
