//! A quality band's say over a STAC collection's cells (`"mask"` on `cube.reduce` and
//! `cube.trend`): each date's cell is taken only where the same item's quality asset (Sentinel-2's
//! SCL, Landsat's `qa_pixel`, CBERS's `CMASK`) has a class kept or the bits asked for, so clouds
//! and their shadows are left out before a summary over time.

use serde_json::Value;

/// Which values of a quality band let a cell through. A cell with no value in it (no-data, or
/// outside its file) is never let through.
#[derive(Clone, Debug, PartialEq)]
pub enum Test {
    /// Only these classes (Sentinel-2's SCL 4, 5, 6: vegetation, bare soil, water).
    Keep(Vec<f32>),
    /// Every class but these.
    Drop(Vec<f32>),
    /// Whole numbers with every bit of `set` on and every bit of `unset` off (Landsat's
    /// `qa_pixel`: bit 6 on, clear).
    Bits { set: u64, unset: u64 },
}

impl Test {
    /// Whether a quality value `v` lets its cell through.
    pub fn accepts(&self, v: f32) -> bool {
        if !v.is_finite() {
            return false;
        }
        match self {
            Test::Keep(classes) => classes.contains(&v),
            Test::Drop(classes) => !classes.contains(&v),
            Test::Bits { set, unset } => {
                // u64::MAX as f32 rounds up to 2^64: below it, the cast is exact for whole numbers.
                v >= 0.0 && v.fract() == 0.0 && v < u64::MAX as f32 && {
                    let u = v as u64;
                    u & set == *set && u & unset == 0
                }
            }
        }
    }
}

/// A mask: the item's asset read and the test its values pass.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub asset: String,
    pub test: Test,
}

/// The most classes `keep` or `drop` lists.
const MAX_CLASSES: usize = 256;

/// `"mask"`: `{"asset": key, "keep": [classes]}`, `{"asset", "drop": [classes]}` or `{"asset",
/// "bits": {"set"?: [bits], "unset"?: [bits]}}`, bits 0 to 63; `None` when not given.
pub fn parse(p: &Value) -> Result<Option<Mask>, String> {
    const SHAPE: &str = "mask is {\"asset\": the items' quality band, and one of \"keep\": [classes], \"drop\": [classes] or \"bits\": {\"set\": [bits], \"unset\": [bits]}}";
    let Some(m) = p.get("mask").filter(|m| !m.is_null()) else { return Ok(None) };
    let o = m.as_object().ok_or(SHAPE)?;
    if let Some(k) = o.keys().find(|k| !["asset", "keep", "drop", "bits"].contains(&k.as_str())) {
        return Err(format!("mask takes asset, keep, drop or bits, not {k}"));
    }
    let asset = o.get("asset").and_then(Value::as_str).map(str::trim).filter(|a| !a.is_empty()).ok_or(SHAPE)?;
    // Classes are whole numbers, as a quality band stores them: 4.5 would never match a cell.
    // Within ±2^24, so f32 holds each one exactly.
    let classes = |k: &str| -> Result<Vec<f32>, String> {
        let list = o.get(k).and_then(Value::as_array).filter(|l| !l.is_empty() && l.len() <= MAX_CLASSES);
        let need = || format!("mask's {k} is a list of 1 to {MAX_CLASSES} classes (whole numbers)");
        let whole = |c: &f64| c.fract() == 0.0 && c.abs() <= 16_777_216.0;
        list.ok_or_else(need)?.iter().map(|c| c.as_f64().filter(whole).map(|c| c as f32).ok_or_else(need)).collect()
    };
    let given: Vec<&str> = ["keep", "drop", "bits"].into_iter().filter(|k| o.get(*k).is_some_and(|v| !v.is_null())).collect();
    let test = match given.as_slice() {
        ["keep"] => Test::Keep(classes("keep")?),
        ["drop"] => Test::Drop(classes("drop")?),
        ["bits"] => {
            const BITS: &str = "mask's bits is {\"set\": [bits on], \"unset\": [bits off]}, bits 0 to 63, at least one";
            let b = o.get("bits").and_then(Value::as_object).ok_or(BITS)?;
            if b.keys().any(|k| k != "set" && k != "unset") {
                return Err(BITS.into());
            }
            let word = |k: &str| -> Result<u64, String> {
                match b.get(k).filter(|v| !v.is_null()) {
                    None => Ok(0),
                    Some(v) => v.as_array().ok_or(BITS)?.iter().try_fold(0u64, |w, bit| {
                        let bit = bit.as_u64().filter(|bit| *bit < 64).ok_or(BITS)?;
                        Ok(w | 1 << bit)
                    }),
                }
            };
            let (set, unset) = (word("set")?, word("unset")?);
            if set == 0 && unset == 0 {
                return Err(BITS.into());
            }
            if set & unset != 0 {
                return Err("mask's bits has a bit both set and unset".into());
            }
            Test::Bits { set, unset }
        }
        _ => return Err(SHAPE.into()),
    };
    Ok(Some(Mask { asset: asset.to_string(), test }))
}

/// For pixels `from..to` along one axis of a grid (its first pixel's edge at `origin`, `size` a
/// pixel, negative for rows going south), the pixel of another grid along it (`m_origin`,
/// `m_size`, `m_count` pixels) whose span holds each one's centre: nearest neighbour, so a mask
/// at 20 m is read onto a band at 10 m, and the same grid onto itself. `None` outside it.
pub fn nearest(origin: f64, size: f64, (from, to): (u32, u32), m_origin: f64, m_size: f64, m_count: u32) -> Vec<Option<u32>> {
    (from..to)
        .map(|i| {
            let centre = origin + (f64::from(i) + 0.5) * size;
            let j = ((centre - m_origin) / m_size).floor();
            (j >= 0.0 && j < f64::from(m_count)).then_some(j as u32)
        })
        .collect()
}

/// The range of the pixels [`nearest`] found, to read them in one window; `None` when none.
pub fn span(v: &[Option<u32>]) -> Option<(u32, u32)> {
    v.iter().flatten().fold(None, |s, j| Some(s.map_or((*j, j + 1), |(a, b): (u32, u32)| (a.min(*j), b.max(j + 1)))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// SCL classes kept or dropped, Landsat's qa_pixel bits, and no value never let through.
    #[test]
    fn a_mask_lets_through_the_classes_or_bits_asked_for() {
        let keep = parse(&json!({"mask": {"asset": "SCL", "keep": [4, 5, 6]}})).unwrap().unwrap();
        assert_eq!(keep.asset, "SCL");
        assert!(keep.test.accepts(4.0) && keep.test.accepts(6.0) && !keep.test.accepts(8.0) && !keep.test.accepts(f32::NAN));
        let drop = parse(&json!({"mask": {"asset": "SCL", "drop": [0, 1, 3, 8, 9, 10]}})).unwrap().unwrap().test;
        assert!(drop.accepts(4.0) && !drop.accepts(9.0) && !drop.accepts(f32::NAN));
        // qa_pixel 21824 (bits 6, 8, 10, 12, 14): clear land; 22280 has bit 3 on (cloud), not 6.
        let qa = parse(&json!({"mask": {"asset": "qa_pixel", "bits": {"set": [6], "unset": [1, 3, 4]}}})).unwrap().unwrap().test;
        assert_eq!(qa, Test::Bits { set: 1 << 6, unset: 0b11010 });
        assert!(qa.accepts(21824.0) && !qa.accepts(22280.0) && !qa.accepts(-64.0) && !qa.accepts(64.5));
        assert_eq!(parse(&json!({})).unwrap(), None);
        assert_eq!(parse(&json!({"mask": null})).unwrap(), None);
        for (bad, said) in [
            (json!("SCL"), "mask is"),
            (json!({"keep": [4]}), "mask is"),
            (json!({"asset": "SCL"}), "mask is"),
            (json!({"asset": "SCL", "keep": [4], "drop": [8]}), "mask is"),
            (json!({"asset": "SCL", "keep": []}), "keep is a list"),
            (json!({"asset": "SCL", "keep": ["4"]}), "keep is a list"),
            (json!({"asset": "SCL", "keep": [4.5]}), "whole numbers"),
            (json!({"asset": "SCL", "drop": [1e30]}), "whole numbers"),
            (json!({"asset": "SCL", "keep": [4], "keeps": [5]}), "not keeps"),
            (json!({"asset": "qa", "bits": {"set": [64]}}), "bits 0 to 63"),
            (json!({"asset": "qa", "bits": {}}), "at least one"),
            (json!({"asset": "qa", "bits": {"on": [6]}}), "bits is"),
            (json!({"asset": "qa", "bits": {"set": [6], "unset": [6]}}), "both"),
        ] {
            let e = parse(&json!({ "mask": bad })).unwrap_err();
            assert!(e.contains(said), "{bad}: {e}");
        }
    }

    /// A mask on the band's grid maps pixel to pixel; one at twice the size maps two to one; rows
    /// going south too; a pixel past the mask's file has none.
    #[test]
    fn a_mask_is_read_onto_the_bands_grid_by_nearest_neighbour() {
        assert_eq!(nearest(100.0, 10.0, (0, 3), 100.0, 10.0, 3), vec![Some(0), Some(1), Some(2)]);
        assert_eq!(nearest(100.0, 10.0, (2, 6), 100.0, 20.0, 2), vec![Some(1), Some(1), None, None]);
        assert_eq!(nearest(500.0, -10.0, (0, 4), 500.0, -20.0, 2), vec![Some(0), Some(0), Some(1), Some(1)]);
        assert_eq!(nearest(90.0, 10.0, (0, 2), 100.0, 10.0, 5), vec![None, Some(0)]);
        assert_eq!(span(&[None, Some(3), Some(1), None]), Some((1, 4)));
        assert_eq!(span(&[None, None]), None);
    }
}
