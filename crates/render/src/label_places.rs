//! Deferred curved placements: transformations are retained until collision placement needs them.
use super::*;
use std::sync::Arc;

type Projection = Arc<dyn Fn(f32, f32) -> Option<(f32, f32)> + Send + Sync>;

#[derive(Clone)]
enum Change {
    Project(Projection),
    PlaceShifts(Vec<(f32, f32)>),
}

#[derive(Debug)]
pub(super) struct CurveSpec {
    pub line: Arc<line_labels::Line>,
    pub chars: Vec<String>,
    pub advances: Vec<f32>,
    pub gap: f32,
    pub overrun: f32,
}

#[derive(Clone)]
pub(super) struct Curves {
    pub spec: Arc<CurveSpec>,
    pub positions: Vec<f32>,
    pub locked: Option<usize>,
    changes: Vec<Change>,
}

impl std::fmt::Debug for Curves {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Curves").field("positions", &self.positions).field("locked", &self.locked).finish_non_exhaustive()
    }
}

impl Curves {
    pub fn new(spec: Arc<CurveSpec>, positions: Vec<f32>, locked: Option<usize>) -> Self {
        Self { spec, positions, locked, changes: Vec::new() }
    }

    pub fn project(&mut self, at: impl Fn(f32, f32) -> Option<(f32, f32)> + Send + Sync + 'static) {
        self.changes.push(Change::Project(Arc::new(at)));
    }

    /// Compute each placement's relief shift at its own transformed anchor, without laying out text.
    pub fn shift_each(&mut self, offset: &impl Fn(f32, f32) -> Option<(f32, f32)>) {
        let shifts = self
            .positions
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let Some((mut x, mut y)) = self.spec.line.anchor(*d) else { return (0.0, 0.0) };
                for change in &self.changes {
                    match change {
                        Change::Project(at) => {
                            let Some(p) = at(x, y) else { return (0.0, 0.0) };
                            (x, y) = p;
                        }
                        Change::PlaceShifts(shifts) => {
                            if let Some((dx, dy)) = shifts.get(i) {
                                x += dx;
                                y += dy;
                            }
                        }
                    }
                }
                offset(x, y).unwrap_or((0.0, 0.0))
            })
            .collect();
        self.changes.push(Change::PlaceShifts(shifts));
    }

    /// The first in-sight preview, with its own line direction projected at its source anchor;
    /// no character layout. Even a too-short stretch has a tangent for View Unplaced.
    pub fn preview_in(&self, template: &mut PlacedLabel, at: &impl Fn(f32, f32) -> Option<(f32, f32)>) -> Option<LabelPlace> {
        let width = line_labels::curved_len(&self.spec.advances, self.spec.gap);
        for (i, d) in self.positions.iter().enumerate() {
            if self.locked.is_some_and(|won| won != i) {
                continue;
            }
            let Some((x, y)) = self.spec.line.anchor(*d) else { continue };
            let angle =
                self.spec.line.parallel(width, self.spec.overrun, Some(*d)).map(|(_, _, a)| a).or_else(|| self.spec.line.direction(*d));
            let mut preview = LabelPlace { x, y, angle, glyphs: Vec::new() };
            if preview.transform(template, at) {
                return Some(preview);
            }
        }
        None
    }

    pub fn place(&self, i: usize, template: &mut PlacedLabel) -> Option<LabelPlace> {
        if self.locked.is_some_and(|won| won != i) {
            return None;
        }
        let at = *self.positions.get(i)?;
        #[cfg(test)]
        LAYOUTS.with(|n| n.set(n.get() + 1));
        let ((x, y), turns) = self.spec.line.curved(&self.spec.advances, self.spec.gap, self.spec.overrun, Some(at))?;
        // Reuse the preceding rejected place's vector and String capacities. The accepted
        // middle preview retains its own glyphs; later failed attempts can share this pool.
        let mut glyphs = std::mem::take(&mut template.glyphs);
        let mut count = 0;
        for (i, (text, (dx, dy, angle))) in self.spec.chars.iter().zip(turns).enumerate() {
            if let Some(glyph) = glyphs.get_mut(i) {
                glyph.text.clone_from(text);
                (glyph.dx, glyph.dy, glyph.angle) = (dx, dy, angle);
            } else {
                glyphs.push(LabelGlyph { text: text.clone(), dx, dy, angle });
            }
            count += 1;
        }
        glyphs.truncate(count);
        let mut p = LabelPlace { x, y, angle: None, glyphs };
        for change in &self.changes {
            match change {
                Change::Project(at) => {
                    if !p.transform(template, &|x, y| at(x, y)) {
                        return None;
                    }
                }
                Change::PlaceShifts(shifts) => {
                    if let Some((dx, dy)) = shifts.get(i) {
                        (p.x, p.y) = (p.x + dx, p.y + dy);
                    }
                }
            }
        }
        Some(p)
    }
}

#[cfg(test)]
thread_local! {
    pub(super) static LAYOUTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
