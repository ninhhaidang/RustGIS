use crate::Session;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

/// Exact executable content last authored by the user. In memory, never persisted or logged.
pub(crate) type Trust = HashMap<String, Arc<Value>>;
impl Session {
    /// Whether this item's executable content still equals its user-authored record.
    pub fn item_trusted(&self, kind: &str, name: &str) -> bool {
        let records = match kind {
            "task" => &self.trusted_tasks,
            "notebook" => &self.trusted_notebooks,
            _ => &self.trusted_models,
        };
        records.get(&name.to_ascii_lowercase()).zip(self.item_content(kind, name)).is_some_and(|(a, b)| a.as_ref() == &b)
    }
    /// Snapshots only executable content; generated notebook output never confers trust.
    pub(crate) fn item_content(&self, kind: &str, name: &str) -> Option<Value> {
        match kind {
            "task" => self.project.tasks.iter().find(|t| t.name.eq_ignore_ascii_case(name)).map(|t| json!(t.steps)),
            "notebook" => self.project.notebooks.iter().find(|n| n.name.eq_ignore_ascii_case(name)).map(|n| {
                json!({"language":n.language(),"cells":n.cells.iter().map(|c|json!({"kind":c.kind,"source":c.source})).collect::<Vec<_>>()})
            }),
            _ => self.project.models.iter().find(|m| m.name.eq_ignore_ascii_case(name)).map(|m| json!({"steps":m.steps,"graph":m.graph})),
        }
    }
    /// Uses each item executor's own selector, including active/default items.
    pub(crate) fn item_name(&self, kind: &str, p: &Value) -> Result<String, String> {
        if kind == "model" && p.get("graph").is_some_and(|g| !g.is_null()) {
            return Ok(p.get("name").and_then(Value::as_str).unwrap_or("Model").into());
        }
        let given = p.get("name").and_then(Value::as_str);
        let name = match kind {
            "task" => Some(crate::cmd::items::task_ref(self, p)?.name.clone()),
            "notebook" => self.project.notebooks.get(crate::cmd::notebook::index(self, p)?).map(|n| n.name.clone()),
            _ => given
                .and_then(|name| self.project.models.iter().find(|m| m.name.eq_ignore_ascii_case(name)))
                .or_else(|| given.is_none().then(|| self.project.models.last()).flatten())
                .map(|m| m.name.clone()),
        };
        name.ok_or_else(|| format!("no {kind} with that name"))
    }
    /// Records exact user-authored executable content in memory only.
    pub(super) fn trust_record(&mut self, kind: &str, name: &str) {
        if let Some(content) = self.item_content(kind, name) {
            let records = match kind {
                "task" => &mut self.trusted_tasks,
                "notebook" => &mut self.trusted_notebooks,
                _ => &mut self.trusted_models,
            };
            records.insert(name.to_ascii_lowercase(), Arc::new(content));
        }
    }
    /// An editor may refresh only a record that matched before the user edit.
    pub(super) fn trust_editor(&self, id: &str, p: &Value) -> Option<(&'static str, String, bool)> {
        let kind = match id {
            "task.addStep" | "task.editStep" | "task.moveStep" | "task.removeStep" => "task",
            "notebook.addCell" | "notebook.editCell" | "notebook.removeCell" => "notebook",
            "model.save" => "model",
            _ => return None,
        };
        let name = if id == "model.save" {
            crate::cmd::workflows::model_save_names(p).ok()?.0
        } else {
            self.item_name(kind, p).ok().or_else(|| p.get("name").and_then(Value::as_str).map(str::to_string))?
        };
        let trusted = self.item_trusted(kind, &name)
            || (id == "model.save" && !self.project.models.iter().any(|m| m.name.eq_ignore_ascii_case(&name)));
        Some((kind, name, trusted))
    }
    /// Records successful User creation/edits; imports and non-user changes cannot promote trust.
    pub(super) fn trust_after(&mut self, id: &str, p: &Value, result: &Value, editor: Option<(&'static str, String, bool)>) {
        if !self.policy_user() || p.get("imported").and_then(Value::as_bool) == Some(true) {
            return;
        }
        if let Some((kind, _, true)) = editor {
            let name = if id == "model.save" {
                result.get("model").and_then(Value::as_str).map(str::to_owned)
            } else {
                self.item_name(kind, p).ok()
            };
            if let Some(name) = name {
                self.trust_record(kind, &name);
            }
        }
        let kind = match id {
            "task.new" => "task",
            "notebook.new" => "notebook",
            "model.new" => "model",
            _ => return,
        };
        if p.get("fromHistory").and_then(Value::as_bool) == Some(true) && p.get("steps").is_none() && self.history_from_file > 0 {
            return;
        }
        let name = if kind == "notebook" {
            result
                .get("notebook")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .and_then(|n| self.project.notebooks.get(n))
                .map(|n| n.name.clone())
        } else {
            result.get(kind).and_then(Value::as_str).map(str::to_owned)
        };
        if let Some(name) = name {
            self.trust_record(kind, &name);
        }
    }
}
