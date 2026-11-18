//! Layer policies around the data crate's definition-query matcher.

use crate::Layer;
use rustgis_data::Dataset;

/// Which layer restrictions a consumer honors. Whole-layer tools deliberately use `All`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Ignore selection, the definition query and `within`; consume the whole dataset.
    All,
    Query,
    SelectionAndQuery,
}

/// An empty subset means no rows, never every row. Legacy analysis adapters must refuse an
/// empty subset before handing a list to tools whose empty-list convention means all rows.
#[derive(Debug, PartialEq, Eq)]
pub enum Rows {
    All,
    Subset(Vec<usize>),
}

/// Resolved rows and an unreadable-query fallback the consumer must report. Keeping the
/// parser error beside the rows avoids silently treating a broken query as an absent one.
#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    pub rows: Rows,
    pub query_error: Option<String>,
}

impl Layer {
    /// Resolve against the consumed dataset, not a streamed layer's resident window. `within`
    /// is an optional extent restriction, used only without a selection. Invalid queries leave
    /// rows unfiltered, as on the map; `query_error` retains the parser explanation for a notice.
    /// Out-of-range caller indices are retained, matching the legacy analysis adapter, with or
    /// without a query. A query keeping every row returns `All` unless a caller subset applies.
    /// Query evaluation and source-ID remapping each run without Cancel checks; callers retain
    /// traversal control. A large source-ID scan or query pass can finish before Cancel is seen.
    pub fn resolve_rows(&self, dataset: &Dataset, policy: Policy, within: Option<&[usize]>) -> Result<Resolved, String> {
        if policy == Policy::All {
            return Ok(Resolved { rows: Rows::All, query_error: None });
        }
        let current = if policy == Policy::SelectionAndQuery && !self.selection.is_empty() {
            Some(self.selected_rows_in(dataset)?)
        } else {
            within.map(<[usize]>::to_vec)
        };
        let query = match rustgis_data::rows::Query::compile(self.definition_query.as_deref(), dataset) {
            Ok(query) if query.is_filtered() => query,
            other => return Ok(Resolved { rows: current.map_or(Rows::All, Rows::Subset), query_error: other.err() }),
        };
        let env = rustgis_data::query::Env::default();
        let kept = |row: &usize| *row >= dataset.features.len() || query.matches(dataset, *row, &env);
        let rows = match current {
            Some(rows) => Rows::Subset(rows.into_iter().filter(kept).collect()),
            None => {
                // Allocate indices only once the query excludes a row. The all-matching path
                // stays implicit and doesn't turn every source row into an explicit selection.
                let first_hidden = (0..dataset.features.len()).find(|row| !kept(row));
                if let Some(first_hidden) = first_hidden {
                    let mut rows: Vec<usize> = (0..first_hidden).collect();
                    rows.extend((first_hidden + 1..dataset.features.len()).filter(kept));
                    Rows::Subset(rows)
                } else {
                    Rows::All
                }
            }
        };
        Ok(Resolved { rows, query_error: None })
    }

    /// Map window selections by source feature ID when consuming another dataset. Selection
    /// order is retained, and a disappeared feature is an error rather than silently dropped.
    /// This source-wide ID scan has no internal Cancel check.
    fn selected_rows_in(&self, dataset: &Dataset) -> Result<Vec<usize>, String> {
        if std::ptr::eq(dataset, &self.dataset) {
            return Ok(self.selection.clone());
        }
        let missing = || format!("{}: a selected feature is no longer in the source", self.name);
        let mut by_id = self
            .selection
            .iter()
            .map(|row| self.dataset.features.get(*row).map(|f| (f.id, None)).ok_or_else(missing))
            .collect::<Result<std::collections::HashMap<u64, Option<usize>>, _>>()?;
        for (row, feature) in dataset.features.iter().enumerate() {
            if let Some(selected) = by_id.get_mut(&feature.id) {
                *selected = Some(row);
            }
        }
        self.selection
            .iter()
            .map(|row| self.dataset.features.get(*row).and_then(|f| by_id.get(&f.id)).copied().flatten().ok_or_else(missing))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Field, FieldType, GeometryType, Value};

    #[test]
    fn row_policy_matrix_keeps_empty_subsets_and_source_identity() {
        let mut ds = Dataset::new("rows", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        for n in 1..=3 {
            ds.push(None, vec![Value::Int(n)]);
        }
        let mut layer = Layer::new(1, ds);
        for (query, selection, within, expected) in [
            (None, vec![], None, Rows::All),
            (Some("n >= 2"), vec![], None, Rows::Subset(vec![1, 2])),
            (Some("n >= 1"), vec![], None, Rows::All),
            (Some("n >= 2"), vec![], Some(vec![99, 1]), Rows::Subset(vec![99, 1])),
            (None, vec![], Some(vec![99, 1]), Rows::Subset(vec![99, 1])),
            (None, vec![2, 0], None, Rows::Subset(vec![2, 0])),
            (Some("n >= 2"), vec![0, 2], None, Rows::Subset(vec![2])),
            (Some("n > 3"), vec![], None, Rows::Subset(vec![])),
            (Some("n >= 2"), vec![0], None, Rows::Subset(vec![])),
            (None, vec![], Some(vec![]), Rows::Subset(vec![])),
            (Some("n >= 2"), vec![], Some(vec![0, 1]), Rows::Subset(vec![1])),
            (Some("n >= 2"), vec![2], Some(vec![]), Rows::Subset(vec![2])),
            (Some("unknown = 1"), vec![2], None, Rows::Subset(vec![2])),
            (Some("n = = 1"), vec![], None, Rows::All),
            (Some("   "), vec![], None, Rows::All),
        ] {
            layer.definition_query = query.map(str::to_string);
            layer.selection = selection;
            let resolved = layer.resolve_rows(&layer.dataset, Policy::SelectionAndQuery, within.as_deref()).unwrap();
            assert_eq!(resolved.query_error.is_some(), matches!(query, Some("unknown = 1" | "n = = 1")));
            assert_eq!(resolved.rows, expected);
        }
        layer.selection = vec![0];
        layer.definition_query = Some("n = 2".into());
        assert_eq!(layer.resolve_rows(&layer.dataset, Policy::Query, None).unwrap().rows, Rows::Subset(vec![1]));
        assert_eq!(layer.resolve_rows(&layer.dataset, Policy::All, Some(&[])).unwrap().rows, Rows::All);
        let mut full = layer.dataset.clone();
        // A streamed window's first row is the full source's last: position must not select n=1.
        layer.dataset.features = vec![full.features[2].clone()].into();
        layer.definition_query = Some("n = 3".into());
        assert_eq!(layer.resolve_rows(&full, Policy::SelectionAndQuery, None).unwrap().rows, Rows::Subset(vec![2]));
        full.features.pop();
        assert!(layer.resolve_rows(&full, Policy::SelectionAndQuery, None).unwrap_err().contains("no longer in the source"));
    }
}
