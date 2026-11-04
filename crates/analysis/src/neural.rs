//! Deep learning for imagery: a multilayer perceptron (dense layers with ReLU, softmax output)
//! trained on the pixels under training polygons, then applied to every pixel of a raster.
//! Deterministic (a seeded generator), bounded (samples, layers, units and epochs are capped),
//! and pure Rust.

use rustgis_data::raster::Raster;
use rustgis_geom::Coord;
use serde::{Deserialize, Serialize};

/// Most training samples kept per class, hidden layers, units per layer, and epochs.
pub const MAX_SAMPLES_PER_CLASS: usize = 4000;
pub const MAX_LAYERS: usize = 4;
pub const MAX_UNITS: usize = 256;
pub const MAX_EPOCHS: usize = 500;
/// Most classes and input bands.
pub const MAX_CLASSES: usize = 64;
pub const MAX_INPUTS: usize = 64;
/// Most multiply-adds one training may do (seconds, not minutes): epochs are reduced to fit.
pub const MAX_WORK: f64 = 5e9;

/// One dense layer: `out × inp` weights (row per output) and `out` biases.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dense {
    pub inp: usize,
    pub out: usize,
    pub w: Vec<f32>,
    pub b: Vec<f32>,
}

/// A trained classifier: input scaling, layers, and the class names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mlp {
    /// 0-based bands read, in order.
    pub bands: Vec<usize>,
    pub mean: Vec<f32>,
    pub std: Vec<f32>,
    pub layers: Vec<Dense>,
    pub classes: Vec<String>,
}

/// A small, seeded generator (xorshift), so training is repeatable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform in [-1, 1).
    fn sym(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

impl Mlp {
    /// Checks a model read from a file: sizes agree, numbers are finite, within the caps.
    pub fn validate(&self) -> Result<(), String> {
        let n = self.bands.len();
        if n == 0 || n > MAX_INPUTS || self.mean.len() != n || self.std.len() != n {
            return Err("the model's inputs don't agree".into());
        }
        if self.layers.is_empty() || self.layers.len() > MAX_LAYERS + 1 {
            return Err("the model has no layers, or too many".into());
        }
        let mut inp = n;
        for l in &self.layers {
            if l.inp != inp || l.out == 0 || l.out > MAX_UNITS.max(MAX_CLASSES) || l.w.len() != l.inp * l.out || l.b.len() != l.out {
                return Err("the model's layers don't fit together".into());
            }
            if !l.w.iter().chain(&l.b).all(|v| v.is_finite()) {
                return Err("the model has invalid weights".into());
            }
            inp = l.out;
        }
        if inp != self.classes.len() || self.classes.len() < 2 || self.classes.len() > MAX_CLASSES {
            return Err("the model's classes don't match its output".into());
        }
        if !self.mean.iter().chain(&self.std).all(|v| v.is_finite()) || self.std.iter().any(|s| *s <= 0.0) {
            return Err("the model's input scaling is invalid".into());
        }
        Ok(())
    }

    /// Class probabilities for one pixel's band values.
    pub fn probabilities(&self, x: &[f32]) -> Vec<f32> {
        let mut a: Vec<f32> = x.iter().zip(self.mean.iter().zip(&self.std)).map(|(v, (m, s))| (v - m) / s).collect();
        let last = self.layers.len().saturating_sub(1);
        for (k, l) in self.layers.iter().enumerate() {
            let mut z = l.b.clone();
            for (o, zo) in z.iter_mut().enumerate() {
                let row = l.w.get(o * l.inp..(o + 1) * l.inp).unwrap_or(&[]);
                *zo += row.iter().zip(&a).map(|(w, v)| w * v).sum::<f32>();
            }
            if k < last {
                z.iter_mut().for_each(|v| *v = v.max(0.0));
            }
            a = z;
        }
        softmax(&mut a);
        a
    }

    /// The most likely class (0-based) and its probability.
    pub fn predict(&self, x: &[f32]) -> (usize, f32) {
        let p = self.probabilities(x);
        p.iter().enumerate().fold((0, f32::MIN), |best, (i, v)| if *v > best.1 { (i, *v) } else { best })
    }
}

fn softmax(a: &mut [f32]) {
    let m = a.iter().copied().fold(f32::MIN, f32::max);
    let mut sum = 0.0;
    for v in a.iter_mut() {
        *v = (*v - m).exp();
        sum += *v;
    }
    if sum > 0.0 {
        a.iter_mut().for_each(|v| *v /= sum);
    }
}

/// The samples of a model's classes under its training polygons, each polygon's outline given
/// with its class and its share of the class's samples, and each class taking up to
/// `max_per_class`: the samples, and how many each class got. The polygons are counted for the
/// run's progress, and a cancelled run stops between rows of a polygon (see [`crate::control`]).
#[allow(clippy::type_complexity)]
pub fn training_samples(
    r: &Raster,
    bands: &[usize],
    polygons: &[(Vec<Vec<Coord>>, usize, usize)],
    classes: usize,
    max_per_class: usize,
) -> Result<(Vec<(Vec<f32>, usize)>, Vec<usize>), String> {
    crate::control::begin(polygons.len());
    let mut samples: Vec<(Vec<f32>, usize)> = Vec::new();
    let mut per_class = vec![0usize; classes];
    for (rings, ci, share) in polygons {
        let room = max_per_class.saturating_sub(per_class.get(*ci).copied().unwrap_or(0)).min(*share);
        let got = polygon_samples(r, bands, rings, room);
        // Stopped part of the way through the polygon: what it got isn't its share.
        crate::control::check()?;
        if let Some(n) = per_class.get_mut(*ci) {
            *n += got.len();
        }
        samples.extend(got.into_iter().map(|x| (x, *ci)));
        crate::control::advance(1);
    }
    Ok((samples, per_class))
}

/// Band values of the pixels inside a polygon (`rings`, longitude/latitude), up to `max`. Only
/// the pixels under the polygon's outline are visited, every `step`-th one in each direction when
/// there are many more than `max`, so the samples spread across the polygon. A cancelled run
/// stops between rows, with the pixels found so far.
pub fn polygon_samples(r: &Raster, bands: &[usize], rings: &[Vec<Coord>], max: usize) -> Vec<Vec<f32>> {
    let mut out = Vec::new();
    let tr = r.transformer();
    // The pixel window under the outline (its vertices, projected into the raster).
    let (mut c0, mut r0, mut c1, mut r1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for v in rings.iter().flatten().take(1_000_000) {
        let Some(w) = tr.from_lonlat(*v) else { continue };
        let (c, rw) = r.transform.to_pixel(w);
        if c.is_finite() && rw.is_finite() {
            (c0, r0, c1, r1) = (c0.min(c), r0.min(rw), c1.max(c), r1.max(rw));
        }
    }
    if c0 > c1 || max == 0 {
        return out;
    }
    let clamp = |v: f64, n: u32| v.clamp(0.0, n as f64) as i64;
    let (c0, c1) = (clamp(c0.floor() - 1.0, r.width), clamp(c1.ceil() + 1.0, r.width));
    let (r0, r1) = (clamp(r0.floor() - 1.0, r.height), clamp(r1.ceil() + 1.0, r.height));
    let window = ((c1 - c0).max(0) as f64) * ((r1 - r0).max(0) as f64);
    // A thin polygon in a large window can slip between the grid's points: the grid is made
    // denser until it finds enough pixels, within 4 million visits over all passes.
    let mut step = ((window / max as f64).sqrt().ceil() as i64).max(1);
    let mut visits = 0f64;
    loop {
        visits += window / (step * step) as f64;
        out.clear();
        let mut row = r0;
        'scan: while row < r1 {
            if crate::control::cancelled() {
                return out;
            }
            let mut col = c0;
            while col < c1 {
                if out.len() >= max {
                    break 'scan;
                }
                let w = r.transform.to_world(col as f64 + 0.5, row as f64 + 0.5);
                if let Some(ll) = tr.to_lonlat(w)
                    && rustgis_data::geometry::polygon_contains(rings, ll)
                    && let Some(v) = bands.iter().map(|b| r.value(*b, col, row)).collect::<Option<Vec<f32>>>()
                {
                    out.push(v);
                }
                col += step;
            }
            row += step;
        }
        let denser = step / 2;
        if out.len() * 2 >= max || denser < 1 || visits + window / (denser * denser) as f64 > 4e6 {
            return out;
        }
        step = denser;
    }
}

/// What training reached: the model and its accuracy on held-back samples (a fifth of them).
pub struct Trained {
    pub model: Mlp,
    pub accuracy: f64,
    pub samples: usize,
    /// The epochs run (fewer than asked when the work would pass [`MAX_WORK`]).
    pub epochs: usize,
}

/// Trains a classifier. `samples` are (band values, class index); `hidden` the units per hidden
/// layer.
pub fn train(
    bands: &[usize],
    classes: Vec<String>,
    samples: &[(Vec<f32>, usize)],
    hidden: &[usize],
    epochs: usize,
) -> Result<Trained, String> {
    let n_in = bands.len();
    if n_in == 0 || n_in > MAX_INPUTS {
        return Err(format!("use 1 to {MAX_INPUTS} bands"));
    }
    let k = classes.len();
    if !(2..=MAX_CLASSES).contains(&k) {
        return Err(format!("training needs 2 to {MAX_CLASSES} classes"));
    }
    let samples: Vec<&(Vec<f32>, usize)> =
        samples.iter().filter(|(x, c)| x.len() == n_in && *c < k && x.iter().all(|v| v.is_finite())).collect();
    if samples.len() < 2 * k {
        return Err("too few training pixels: draw larger training polygons".into());
    }
    // Input scaling from the samples.
    let mut mean = vec![0.0f32; n_in];
    let mut std = vec![0.0f32; n_in];
    for (x, _) in &samples {
        mean.iter_mut().zip(x).for_each(|(m, v)| *m += v);
    }
    mean.iter_mut().for_each(|m| *m /= samples.len() as f32);
    for (x, _) in &samples {
        std.iter_mut().zip(x.iter().zip(&mean)).for_each(|(s, (v, m))| *s += (v - m) * (v - m));
    }
    std.iter_mut().for_each(|s| *s = (*s / samples.len() as f32).sqrt().max(1e-6));
    // Layers, He-initialised.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut sizes = vec![n_in];
    sizes.extend(hidden.iter().take(MAX_LAYERS).map(|u| (*u).clamp(2, MAX_UNITS)));
    sizes.push(k);
    let mut layers: Vec<Dense> = sizes
        .windows(2)
        .map(|w| {
            let (inp, out) = (w[0], w[1]);
            let scale = (2.0 / inp as f32).sqrt();
            Dense { inp, out, w: (0..inp * out).map(|_| rng.sym() * scale).collect(), b: vec![0.0; out] }
        })
        .collect();
    // Shuffle, then hold back every fifth sample for the accuracy check.
    let mut order: Vec<usize> = (0..samples.len()).collect();
    for i in (1..order.len()).rev() {
        let j = rng.below(i + 1);
        order.swap(i, j);
    }
    let (test, fit): (Vec<usize>, Vec<usize>) = order.iter().enumerate().fold((Vec::new(), Vec::new()), |(mut t, mut f), (i, s)| {
        if i % 5 == 0 {
            t.push(*s)
        } else {
            f.push(*s)
        }
        (t, f)
    });
    let scaled = |x: &[f32]| -> Vec<f32> { x.iter().zip(mean.iter().zip(&std)).map(|(v, (m, s))| (v - m) / s).collect() };
    let inputs: Vec<Vec<f32>> = samples.iter().map(|(x, _)| scaled(x)).collect();
    // Mini-batch gradient descent with momentum.
    // Each sample costs about three passes over the weights (forward, backward, update).
    // Too many samples for even one epoch: a random subset (they're shuffled) is fitted.
    let per_sample = 3.0 * layers.iter().map(|l| l.w.len() + l.b.len()).sum::<usize>() as f64;
    let mut fit = fit;
    fit.truncate(((MAX_WORK / per_sample) as usize).max(1));
    let per_epoch = per_sample * fit.len().max(1) as f64;
    let epochs = epochs.clamp(1, MAX_EPOCHS).min(((MAX_WORK / per_epoch) as usize).max(1));
    let (lr, momentum, batch) = (0.05f32, 0.9f32, 32usize);
    let mut vel: Vec<(Vec<f32>, Vec<f32>)> = layers.iter().map(|l| (vec![0.0; l.w.len()], vec![0.0; l.b.len()])).collect();
    crate::control::begin(epochs);
    for epoch in 0..epochs {
        if crate::control::cancelled() {
            break;
        }
        crate::control::reached(epoch);
        for i in (1..fit.len()).rev() {
            let j = rng.below(i + 1);
            fit.swap(i, j);
        }
        for chunk in fit.chunks(batch) {
            let mut grads: Vec<(Vec<f32>, Vec<f32>)> = layers.iter().map(|l| (vec![0.0; l.w.len()], vec![0.0; l.b.len()])).collect();
            for &si in chunk {
                let (Some(x), Some((_, class))) = (inputs.get(si), samples.get(si)) else { continue };
                // Forward, keeping each layer's activations.
                let mut acts: Vec<Vec<f32>> = vec![x.clone()];
                for (li, l) in layers.iter().enumerate() {
                    let a = acts.last().cloned().unwrap_or_default();
                    let mut z = l.b.clone();
                    for (o, zo) in z.iter_mut().enumerate() {
                        let row = l.w.get(o * l.inp..(o + 1) * l.inp).unwrap_or(&[]);
                        *zo += row.iter().zip(&a).map(|(w, v)| w * v).sum::<f32>();
                    }
                    if li + 1 < layers.len() {
                        z.iter_mut().for_each(|v| *v = v.max(0.0));
                    }
                    acts.push(z);
                }
                // Backward: softmax cross-entropy, then ReLU layers.
                let mut delta = acts.last().cloned().unwrap_or_default();
                softmax(&mut delta);
                if let Some(d) = delta.get_mut(*class) {
                    *d -= 1.0;
                }
                for li in (0..layers.len()).rev() {
                    let (Some(l), Some(a_in), Some(g)) = (layers.get(li), acts.get(li), grads.get_mut(li)) else { break };
                    for o in 0..l.out {
                        let d = delta.get(o).copied().unwrap_or(0.0);
                        if let Some(gb) = g.1.get_mut(o) {
                            *gb += d;
                        }
                        for (i, av) in a_in.iter().enumerate() {
                            if let Some(gw) = g.0.get_mut(o * l.inp + i) {
                                *gw += d * av;
                            }
                        }
                    }
                    if li > 0 {
                        let mut prev = vec![0.0f32; l.inp];
                        for (o, d) in delta.iter().enumerate() {
                            for (i, pv) in prev.iter_mut().enumerate() {
                                *pv += l.w.get(o * l.inp + i).copied().unwrap_or(0.0) * d;
                            }
                        }
                        // ReLU: no gradient where the unit was off.
                        for (pv, av) in prev.iter_mut().zip(a_in) {
                            if *av <= 0.0 {
                                *pv = 0.0;
                            }
                        }
                        delta = prev;
                    }
                }
            }
            let scale = lr / chunk.len().max(1) as f32;
            for ((l, g), v) in layers.iter_mut().zip(&grads).zip(vel.iter_mut()) {
                for ((w, gw), vw) in l.w.iter_mut().zip(&g.0).zip(v.0.iter_mut()) {
                    *vw = momentum * *vw - scale * gw.clamp(-50.0, 50.0);
                    *w += *vw;
                }
                for ((b, gb), vb) in l.b.iter_mut().zip(&g.1).zip(v.1.iter_mut()) {
                    *vb = momentum * *vb - scale * gb.clamp(-50.0, 50.0);
                    *b += *vb;
                }
            }
        }
    }
    if !layers.iter().all(|l| l.w.iter().chain(&l.b).all(|v| v.is_finite())) {
        return Err("training diverged; try fewer epochs or smaller layers".into());
    }
    let model = Mlp { bands: bands.to_vec(), mean, std, layers, classes };
    let correct = test.iter().filter(|&&i| samples.get(i).is_some_and(|(x, c)| model.predict(x).0 == *c)).count();
    let accuracy = if test.is_empty() { 1.0 } else { correct as f64 / test.len() as f64 };
    Ok(Trained { model, accuracy, samples: samples.len(), epochs })
}

/// Classifies every pixel: class numbers (1-based, `nodata` where a band has no data) and each
/// pixel's probability of that class. Rows go on several threads when the work is large, except in
/// the browser, which uses one (#1252).
pub fn classify(r: &Raster, model: &Mlp, nodata: f32) -> Result<(Vec<f32>, Vec<f32>), String> {
    classify_on(r, model, nodata, None)
}

/// [`classify`] on `threads` threads (none: as many as pay off).
fn classify_on(r: &Raster, model: &Mlp, nodata: f32, threads: Option<usize>) -> Result<(Vec<f32>, Vec<f32>), String> {
    model.validate()?;
    // A large model on a large raster is refused rather than run for minutes.
    let weights: usize = model.layers.iter().map(|l| l.w.len() + l.b.len()).sum();
    if f64::from(r.width) * f64::from(r.height) * weights as f64 > 4.0 * MAX_WORK {
        return Err("that model is too large for this raster; use a smaller model or clip the raster first".into());
    }
    if let Some(b) = model.bands.iter().find(|b| **b >= r.bands.len()) {
        return Err(format!("{} has no band {}", r.name, b + 1));
    }
    let (w, h) = (r.width as usize, r.height as usize);
    let (mut class, mut prob) = (crate::rasterops::new_band(w, h, nodata)?, crate::rasterops::new_band(w, h, nodata)?);
    // In [`crate::parallel::MIN_ROW_WORK`]'s units (about 30 ns each): a weight of a pixel costs
    // about 1 ns.
    let work = w.saturating_mul(h).saturating_mul(weights) / 30;
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_rows(h, work, model.bands.len() * 4));
    crate::control::begin(h);
    // Each row writes only its own pixels of both bands; each thread reads into its own buffer.
    crate::parallel::for_each_row(
        class.chunks_mut(w.max(1)).zip(prob.chunks_mut(w.max(1))),
        threads,
        || vec![0.0f32; model.bands.len()],
        |x: &mut Vec<f32>, row, (class, prob): (&mut [f32], &mut [f32])| {
            for (col, (c_out, p_out)) in class.iter_mut().zip(prob.iter_mut()).enumerate() {
                // A non-finite value is treated as no data.
                let ok = model.bands.iter().zip(x.iter_mut()).all(|(b, xv)| {
                    match r.value(*b, col as i64, row as i64).filter(|v| v.is_finite()) {
                        Some(v) => {
                            *xv = v;
                            true
                        }
                        None => false,
                    }
                });
                if ok {
                    let (c, p) = model.predict(x);
                    (*c_out, *p_out) = ((c + 1) as f32, p);
                }
            }
        },
    )?;
    Ok((class, prob))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows classified on several threads give the same classes and probabilities, bit for bit,
    /// as one thread (#1252), no-data pixels included.
    #[test]
    fn classify_on_several_threads_is_classify_on_one() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform};
        let mut samples = Vec::new();
        let mut rng = Rng(11);
        for _ in 0..600 {
            let (x, y) = (rng.sym() * 2.0, rng.sym() * 2.0);
            samples.push((vec![x * 100.0 + 500.0, y * 50.0], usize::from((x * x + y * y).sqrt() < 1.0)));
        }
        let model = train(&[0, 1], vec!["out".into(), "in".into()], &samples, &[8], 40).unwrap().model;
        let (w, h) = (90u32, 70u32);
        let band = |f: &dyn Fn(f32, f32) -> f32| {
            (0..w * h).map(|i| f((i % w) as f32 / w as f32 * 4.0 - 2.0, (i / w) as f32 / h as f32 * 4.0 - 2.0)).collect()
        };
        let a: Vec<f32> = band(&|x, _| x * 100.0 + 500.0);
        let mut b: Vec<f32> = band(&|_, y| y * 50.0);
        for v in b.iter_mut().step_by(53) {
            *v = f32::NAN;
        }
        let transform = GeoTransform { origin_x: 0.0, origin_y: 45.0, pixel_width: 0.001, pixel_height: -0.001 };
        let r = Raster::new("img", w, h, vec![a, b], transform, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let (c1, p1) = classify_on(&r, &model, -1.0, Some(1)).unwrap();
        assert!(c1.contains(&1.0) && c1.contains(&2.0) && c1.contains(&-1.0), "both classes and no data");
        // A classified pixel has its class's probability; one with no data has none.
        assert!(c1.iter().zip(&p1).all(|(c, p)| if *c == -1.0 { *p == -1.0 } else { (0.0..=1.0).contains(p) && *p >= 0.5 }));
        for threads in [2, 7, 16] {
            let (c, p) = classify_on(&r, &model, -1.0, Some(threads)).unwrap();
            let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
            assert_eq!((bits(&c), bits(&p)), (bits(&c1), bits(&p1)), "{threads} threads");
        }
        // As many threads as pay off, which `classify` picks, give the same.
        let (c, p) = classify(&r, &model, -1.0).unwrap();
        assert_eq!((c, p), (c1, p1));
    }

    #[test]
    fn learns_a_nonlinear_boundary_and_validates() {
        // Class 1 inside a ring, class 0 outside: not separable by a line.
        let mut samples = Vec::new();
        let mut rng = Rng(7);
        for _ in 0..1200 {
            let (x, y) = (rng.sym() * 2.0, rng.sym() * 2.0);
            let c = usize::from((x * x + y * y).sqrt() < 1.0);
            samples.push((vec![x * 100.0 + 500.0, y * 50.0], c));
        }
        let t = train(&[0, 1], vec!["out".into(), "in".into()], &samples, &[16, 16], 150).unwrap();
        assert!(t.accuracy > 0.9, "accuracy {}", t.accuracy);
        assert_eq!(t.epochs, 150, "small trainings run every epoch asked for");
        assert_eq!(t.model.predict(&[500.0, 0.0]).0, 1);
        assert_eq!(t.model.predict(&[690.0, 95.0]).0, 0);
        t.model.validate().unwrap();
        let mut bad = t.model.clone();
        bad.layers[0].w.pop();
        assert!(bad.validate().is_err());
        let mut bad = t.model.clone();
        bad.std[0] = 0.0;
        assert!(bad.validate().is_err());
        assert!(train(&[0], vec!["a".into()], &samples, &[4], 1).is_err(), "one class");
        // The largest network for every epoch would pass the work cap: fewer epochs run.
        let big = train(&[0, 1], vec!["out".into(), "in".into()], &samples, &[MAX_UNITS; MAX_LAYERS], MAX_EPOCHS).unwrap();
        assert!(big.epochs < MAX_EPOCHS, "{} epochs", big.epochs);
        assert!(train(&[0, 1], vec!["a".into(), "b".into()], &samples[..2], &[4], 1).is_err(), "too few");
    }
}
