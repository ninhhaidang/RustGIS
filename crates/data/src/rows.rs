//! Definition-query compilation and row matching shared by layer consumers.
//! Callers own parse-error notices, traversal, cancellation and dataset ownership.

use crate::{Dataset, query};

/// A definition query compiled against the schema of the data being consumed.
/// An absent or blank query matches every row. An unreadable query is an error so the caller
/// can choose its fallback and report it without losing the parser's explanation.
pub struct Query {
    expr: Option<query::Expr>,
}

impl Query {
    /// Compile once per traversal; field references use `dataset`'s schema.
    pub fn compile(text: Option<&str>, dataset: &Dataset) -> Result<Self, String> {
        let expr = text.filter(|text| !text.trim().is_empty()).map(|text| query::parse_fields(text, dataset)).transpose()?;
        Ok(Self { expr })
    }

    /// Whether a query was supplied, so consumers can avoid allocating an all-true mask.
    pub fn is_filtered(&self) -> bool {
        self.expr.is_some()
    }

    /// Match an existing row using the expression language's normal null and variable rules.
    /// A chunked consumer must supply source-wide row numbering through its traversal adapter.
    pub fn matches(&self, dataset: &Dataset, row: usize, env: &query::Env) -> bool {
        self.expr.as_ref().is_none_or(|expr| query::matches(expr, dataset, row, env))
    }
}
