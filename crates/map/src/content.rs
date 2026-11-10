//! Counted content bounds shared by reports, presentations and notebooks.

/// Content discarded while fitting an item read from a file to the supported limits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentRepairs {
    /// Text fields shortened, excluding report predicates.
    pub text: usize,
    /// Report predicates shortened: these can change which records a report includes.
    pub filters: usize,
    /// Entries removed from bounded lists, including pages, cells and report sections.
    pub entries: usize,
    /// Oversized pictures or video posters removed.
    pub images: usize,
    /// Numeric page, timing or map-view settings brought into the supported range.
    pub settings: usize,
}

impl ContentRepairs {
    /// Shortens one text field and counts it only when characters were discarded.
    pub fn cut(&mut self, text: &mut String, limit: usize) {
        self.text += usize::from(cut(text, limit));
    }

    /// Counts a shortened report predicate separately from ordinary display text.
    pub fn cut_filter(&mut self, text: &mut String, limit: usize) {
        self.filters += usize::from(cut(text, limit));
    }

    /// Bounds a list and counts the entries it loses.
    pub fn truncate<T>(&mut self, entries: &mut Vec<T>, limit: usize) {
        self.entries += entries.len().saturating_sub(limit);
        entries.truncate(limit);
    }
}

/// Checks only through the limit before allocating the retained prefix.
fn cut(text: &mut String, limit: usize) -> bool {
    if text.chars().nth(limit).is_none() {
        return false;
    }
    *text = text.chars().take(limit).collect();
    true
}
