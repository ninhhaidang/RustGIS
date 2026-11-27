//! A layer's geometry as a map draws it ([`crate::Layer::projected_in`]), row for row: the
//! layer's own rows, read in place, on a map in the system it holds them in (no copy is made,
//! #2032 C2f), or the copies made for another projection, or thinned for display.

use rustgis_data::{Features, Geometry};

/// A layer's geometry as drawn, row for row ([`crate::Layer::projected_in`]): each row's
/// geometry, `None` for a row without one.
#[derive(Clone, Copy)]
pub enum Shapes<'a> {
    /// The layer's own rows, as they are: on a map in the system the layer holds them in.
    Rows(&'a Features),
    /// Copies made for the map (projected, or thinned for display).
    Copies(&'a [Option<Geometry>]),
}

impl<'a> Shapes<'a> {
    /// No rows.
    pub const EMPTY: Shapes<'static> = Shapes::Copies(&[]);

    /// How many rows.
    pub fn len(self) -> usize {
        match self {
            Shapes::Rows(f) => f.len(),
            Shapes::Copies(c) => c.len(),
        }
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Row `row`'s geometry: `None` past the last row, or for a row without one.
    pub fn get(self, row: usize) -> Option<&'a Geometry> {
        match self {
            Shapes::Rows(f) => f.geometry(row),
            Shapes::Copies(c) => c.get(row)?.as_ref(),
        }
    }

    /// Each row's geometry, in row order.
    pub fn iter(self) -> Iter<'a> {
        match self {
            Shapes::Rows(f) => Iter::Rows(f, 0..f.len()),
            Shapes::Copies(c) => Iter::Copies(c.iter()),
        }
    }

    /// Each row's geometry, copied.
    pub fn to_vec(self) -> Vec<Option<Geometry>> {
        self.iter().map(|g| g.cloned()).collect()
    }

    /// Whether both are the same geometry, not a copy of it: the same rows or the same copies.
    pub fn same(self, other: Shapes<'_>) -> bool {
        match (self, other) {
            (Shapes::Rows(a), Shapes::Rows(b)) => a.shares(b),
            (Shapes::Copies(a), Shapes::Copies(b)) => std::ptr::eq(a, b),
            _ => false,
        }
    }

    /// The copies, when these are copies: `None` for the layer's own rows.
    pub fn copies(self) -> Option<&'a [Option<Geometry>]> {
        match self {
            Shapes::Rows(_) => None,
            Shapes::Copies(c) => Some(c),
        }
    }
}

impl std::fmt::Debug for Shapes<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl PartialEq for Shapes<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl PartialEq<[Option<Geometry>]> for Shapes<'_> {
    fn eq(&self, other: &[Option<Geometry>]) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter().map(Option::as_ref))
    }
}

impl<const N: usize> PartialEq<[Option<Geometry>; N]> for Shapes<'_> {
    fn eq(&self, other: &[Option<Geometry>; N]) -> bool {
        *self == other[..]
    }
}

impl<'a> IntoIterator for Shapes<'a> {
    type Item = Option<&'a Geometry>;
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

/// [`Shapes::iter`].
#[derive(Clone)]
pub enum Iter<'a> {
    Rows(&'a Features, std::ops::Range<usize>),
    Copies(std::slice::Iter<'a, Option<Geometry>>),
}

impl<'a> Iterator for Iter<'a> {
    type Item = Option<&'a Geometry>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Iter::Rows(f, rows) => rows.next().map(|row| f.geometry(row)),
            Iter::Copies(c) => c.next().map(Option::as_ref),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Iter::Rows(_, rows) => rows.size_hint(),
            Iter::Copies(c) => c.size_hint(),
        }
    }
}

impl ExactSizeIterator for Iter<'_> {}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Iter::Rows(f, rows) => rows.next_back().map(|row| f.geometry(row)),
            Iter::Copies(c) => c.next_back().map(Option::as_ref),
        }
    }
}
