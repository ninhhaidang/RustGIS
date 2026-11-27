//! The legend's limits and columns.

/// The most layers a legend leaves out.
pub const MAX_LEGEND_EXCLUDED: usize = 1000;

/// The most layers a legend labels.
pub const MAX_LEGEND_LABELS: usize = 1000;

/// The most characters of a legend's label for a layer.
pub const MAX_LEGEND_LABEL_CHARS: usize = 200;

/// The most columns a legend lays its rows in.
pub const MAX_LEGEND_COLUMNS: usize = 3;

pub(super) fn one_column() -> usize {
    1
}

pub(super) fn is_one_column(columns: &usize) -> bool {
    *columns == 1
}
