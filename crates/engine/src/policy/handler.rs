//! The sole callable command pointer. Outside policy it is metadata, not authority.
use crate::{CmdResult, Session};
use serde_json::Value;

#[derive(Clone, Copy)]
pub struct Handler(fn(&mut Session, &Value) -> CmdResult);
impl Handler {
    /// Only engine registration may wrap a raw function pointer.
    pub(crate) const fn new(f: fn(&mut Session, &Value) -> CmdResult) -> Self {
        Self(f)
    }
    /// Only policy dispatch may execute the wrapped handler.
    pub(in crate::policy) fn call(self, s: &mut Session, p: &Value) -> CmdResult {
        (self.0)(s, p)
    }
    /// Consent binds the captured handler as well as its command id and display metadata.
    pub(in crate::policy) fn same(self, other: Self) -> bool {
        std::ptr::fn_addr_eq(self.0, other.0)
    }
}
