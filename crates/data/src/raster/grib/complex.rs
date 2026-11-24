//! GRIB2's complex packing (data representation templates 5.2 and 5.3, data templates 7.2 and
//! 7.3): the values in groups, each with its own reference, width and length, some of whose values
//! may be marked missing; in 5.3, differenced along the scan (once or twice) before they are
//! grouped.

use super::{MAX_BITS, Packing, Skip, be, bits, signed};

/// The largest packed value made from differences: past it `f64` doesn't hold every whole number
/// (packed values have at most 32 bits, so real ones are far below).
const MAX_VALUE: i64 = 1 << 53;

/// A complex packing, as the data representation section says it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Complex {
    ref_bits: u32,
    /// Code table 5.5: 0 no missing values, 1 the primary one, 2 the primary and secondary.
    missing: u8,
    groups: usize,
    width_ref: u32,
    width_bits: u32,
    length_ref: u32,
    length_step: u32,
    last_length: u32,
    length_bits: u32,
    /// The order of the spatial differencing; 0 without it (template 5.2).
    order: usize,
    /// The octets of each of its descriptors (the first values and the least difference).
    octets: usize,
}

impl Complex {
    /// The packing a data representation section of template 5.`template` (2 or 3) says. One of no
    /// groups is a constant field (NCEP's libraries write one so, and the rest of its template
    /// unset), as simple packing in no bits.
    pub(super) fn of(s: &[u8], template: u32) -> Result<Packing, Skip> {
        if s.len() < if template == 3 { 49 } else { 47 } {
            return Err(Skip::Damaged("a damaged data representation section".to_string()));
        }
        let groups = be(s, 31, 4) as usize;
        if groups == 0 {
            return Ok(Packing::Simple(0));
        }
        let with = |what: String| Skip::Other(format!("GRIB2 complex packing with {what} (template 5.{template})"));
        if s[21] != 1 {
            return Err(with(format!("group splitting method {}", s[21])));
        }
        if s[22] > 2 {
            return Err(with(format!("missing value management {}", s[22])));
        }
        let (order, octets) = if template == 3 { (usize::from(s[47]), usize::from(s[48])) } else { (0, 0) };
        if template == 3 && !(1..=2).contains(&order) {
            return Err(with(format!("spatial differencing of order {order}")));
        }
        if octets > 4 {
            return Err(with(format!("descriptors of {octets} octets")));
        }
        Ok(Packing::Complex(Complex {
            ref_bits: u32::from(s[19]),
            missing: s[22],
            groups,
            width_ref: u32::from(s[35]),
            width_bits: u32::from(s[36]),
            length_ref: be(s, 37, 4),
            length_step: u32::from(s[41]),
            last_length: be(s, 42, 4),
            length_bits: u32::from(s[46]),
            order,
            octets,
        }))
    }

    /// The `count` packed values of `data` (the data section's octets after its header), once its
    /// groups are found to hold them all.
    pub(super) fn values<'a>(&self, data: &'a [u8], count: usize) -> Result<Values<'a>, Skip> {
        let widest = self.ref_bits.max(self.width_bits).max(self.length_bits);
        if widest > MAX_BITS {
            return Err(Skip::Other(format!("values of {widest} bits")));
        }
        if self.groups > count {
            return Err(Skip::Damaged("a field of more groups than values".to_string()));
        }
        // The descriptors, then each group's reference, width and length, each list starting on
        // a whole octet, then the values of every group.
        let list = |width: u32| (self.groups as u64 * u64::from(width)).div_ceil(8) * 8;
        let descriptors = if self.order == 0 { 0 } else { (self.order + 1) * self.octets };
        let refs_at = descriptors as u64 * 8;
        let widths_at = refs_at + list(self.ref_bits);
        let lengths_at = widths_at + list(self.width_bits);
        let at = lengths_at + list(self.length_bits);
        let there = data.len() as u64 * 8;
        if at > there {
            return Err(Skip::Damaged("a data section shorter than its groups".to_string()));
        }
        let mut values = Values {
            data,
            packing: *self,
            refs_at,
            widths_at,
            lengths_at,
            at,
            group: 0,
            left: 0,
            reference: 0,
            width: 0,
            first: [0; 2],
            least: 0,
            seen: 0,
            last: 0,
            before: 0,
            too_large: false,
        };
        let (mut held, mut need) = (0u64, at);
        for g in 0..self.groups {
            let (width, length) = values.size(g);
            if width > u64::from(MAX_BITS) {
                return Err(Skip::Other(format!("values of {width} bits")));
            }
            held = held.saturating_add(length);
            need = need.saturating_add(width.saturating_mul(length));
        }
        if held != count as u64 {
            return Err(Skip::Damaged("a field whose groups don't hold its values".to_string()));
        }
        if need > there {
            return Err(Skip::Damaged("a data section shorter than its values".to_string()));
        }
        // The first values, unsigned as every packed value (GDAL reads them so), then the least
        // difference, a sign bit and magnitude; of no octets, 0.
        if descriptors > 0 {
            for (i, v) in values.first.iter_mut().enumerate().take(self.order) {
                *v = i64::from(be(data, i * self.octets, self.octets));
            }
            values.least = signed(data, self.order * self.octets, self.octets);
        }
        Ok(values)
    }
}

/// A field's packed values, one at a time, so they are never held apart from the field.
pub(super) struct Values<'a> {
    data: &'a [u8],
    packing: Complex,
    /// Where each list of the groups' starts, and the next value, in bits.
    refs_at: u64,
    widths_at: u64,
    lengths_at: u64,
    at: u64,
    /// The next group; what is left of the one being read, its reference and its width.
    group: usize,
    left: u64,
    reference: u64,
    width: u32,
    /// The differencing's first values, its least difference, how many values it has seen (the
    /// missing ones aren't differenced) and the last two.
    first: [i64; 2],
    least: i64,
    seen: usize,
    last: i64,
    before: i64,
    /// Whether the differences added up past [`MAX_VALUE`]: the field is damaged.
    pub(super) too_large: bool,
}

impl Values<'_> {
    /// Entry `g` of the list at `at`, of `width` bits.
    fn entry(&self, at: u64, g: usize, width: u32) -> u64 {
        bits(self.data, at + g as u64 * u64::from(width), width)
    }

    /// Group `g`'s width in bits, and how many values it holds (the last one's is given apart).
    fn size(&self, g: usize) -> (u64, u64) {
        let p = &self.packing;
        let width = u64::from(p.width_ref) + self.entry(self.widths_at, g, p.width_bits);
        let length = if g + 1 == p.groups {
            u64::from(p.last_length)
        } else {
            u64::from(p.length_ref) + u64::from(p.length_step) * self.entry(self.lengths_at, g, p.length_bits)
        };
        (width, length)
    }

    /// The next value; `None` where it is missing.
    pub(super) fn next(&mut self) -> Option<f64> {
        let p = self.packing;
        while self.left == 0 {
            if self.group >= p.groups {
                return None;
            }
            let (width, length) = self.size(self.group);
            self.reference = self.entry(self.refs_at, self.group, p.ref_bits);
            (self.width, self.left) = (width as u32, length);
            self.group += 1;
        }
        self.left -= 1;
        let packed = bits(self.data, self.at, self.width);
        self.at += u64::from(self.width);
        // Missing: every bit set (one less, the secondary missing value) in the value, or in the
        // reference of a group of no bits.
        if p.missing > 0 {
            let (v, width) = if self.width == 0 { (self.reference, p.ref_bits) } else { (packed, self.width) };
            let ones = (1u64 << width) - 1;
            if v == ones || (p.missing == 2 && v + 1 == ones) {
                return None;
            }
        }
        // At most 33 bits, so the sums below are far from `i64`'s range.
        let x = (self.reference + packed) as i64;
        if p.order == 0 {
            return Some(x as f64);
        }
        let mut v = match self.first.get(self.seen) {
            Some(first) if self.seen < p.order => *first,
            _ if p.order == 1 => x + self.least + self.last,
            _ => x + self.least + 2 * self.last - self.before,
        };
        if v.abs() > MAX_VALUE {
            (v, self.too_large) = (0, true);
        }
        self.seen += 1;
        (self.before, self.last) = (self.last, v);
        Some(v as f64)
    }
}
