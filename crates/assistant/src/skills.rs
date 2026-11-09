//! Skills: instructions for a kind of task that the assistant reads when it needs them, in the
//! Agent Skills format that other agents share: a folder holding `SKILL.md`, which is front
//! matter giving a `name` and a `description` between `---` lines, then Markdown. The system
//! prompt lists only names and descriptions; the model reads instructions with `load_skill`,
//! within the shared tool-output budget. The
//! MCP server offers the same skills as prompts.
//!
//! Where skills come from ([`Source`]):
//!
//! - built in;
//! - plugins: a plugin package can ship skills (`skills/<name>/SKILL.md`), named
//!   `<plugin>:<name>` and offered while the plugin is enabled;
//! - on the desktop and the command line, the user's: folders in the settings folder's `skills`
//!   folder, added with [`add`] or by hand;
//! - shared: `~/.agents/skills`, where other agents keep theirs. These are off until switched on,
//!   since they were written for other tools.
//!
//! A user's skill replaces a shared or built-in one of the same name. Which skills are switched
//! on is kept in the settings folder's `skills.json`.

use serde_json::json;

use crate::provider::ToolDef;

/// The largest `SKILL.md` read.
pub const MAX_SKILL: usize = 64 * 1024;

/// Where a skill comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    /// A plugin's (its id).
    Plugin(String),
    /// The user's, in this folder.
    User(std::path::PathBuf),
    /// Shared with other agents (`~/.agents/skills`), in this folder.
    Shared(std::path::PathBuf),
}

/// A skill: what it's for, and its instructions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// Lower case, digits and hyphens; a plugin's skill is `<plugin>:<name>`.
    pub name: String,
    /// What it does and when to use it (all the model sees until it loads the skill).
    pub description: String,
    /// The instructions, in Markdown.
    pub body: String,
    pub source: Source,
}

/// The skills that come with RustGIS.
pub fn built_in() -> Vec<Skill> {
    [
        include_str!("../skills/thematic-map/SKILL.md"),
        include_str!("../skills/explore-data/SKILL.md"),
        include_str!("../skills/print-layout/SKILL.md"),
        include_str!("../skills/geoprocessing/SKILL.md"),
        include_str!("../skills/add-data/SKILL.md"),
        include_str!("../skills/share-web-map/SKILL.md"),
    ]
    .iter()
    .filter_map(|text| parse(text).ok())
    .collect()
}

/// Reads a `SKILL.md`: its front matter's `name` and `description`, then the body. Its source is
/// [`Source::BuiltIn`] until the caller says otherwise.
pub fn parse(text: &str) -> Result<Skill, String> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text.strip_prefix("---").ok_or("a skill starts with front matter between --- lines")?;
    // The opening line is `---` alone; the front matter ends at a line that is `---` alone.
    let mut parts = rest.split_inclusive('\n');
    if !parts.next().unwrap_or("").trim().is_empty() {
        return Err("a skill starts with front matter between --- lines".into());
    }
    let mut lines: Vec<&str> = Vec::new();
    let mut body = None;
    let mut used = rest.find('\n').map_or(rest.len(), |i| i + 1);
    for l in parts {
        used += l.len();
        if l.trim_end() == "---" {
            body = rest.get(used..);
            break;
        }
        lines.push(l.trim_end_matches(['\n', '\r']));
    }
    let body = body.ok_or("the front matter has no closing --- line")?;
    let field = |key: &str| {
        // A key at the top level only: not one nested under another, nor a block's line.
        let i = lines.iter().position(|l| !l.starts_with([' ', '\t']) && l.split_once(':').is_some_and(|(k, _)| k.trim() == key))?;
        let value = lines.get(i)?.split_once(':')?.1.trim();
        // A folded (`>`) or literal (`|`) block: the indented lines after it, joined.
        if value.starts_with(['>', '|']) {
            let block: Vec<&str> =
                lines.iter().skip(i + 1).take_while(|l| l.starts_with([' ', '\t']) || l.trim().is_empty()).map(|l| l.trim()).collect();
            return Some(block.join(" ").split_whitespace().collect::<Vec<_>>().join(" "));
        }
        Some(value.trim_matches(|c| c == '"' || c == '\'').to_string())
    };
    let name = field("name").filter(|n| !n.is_empty()).ok_or("the front matter has no name")?;
    if !valid_name(&name) {
        return Err(format!("'{name}': a skill's name is lower case letters, digits and hyphens"));
    }
    let description = field("description").filter(|d| !d.is_empty()).ok_or("the front matter has no description")?;
    let body = body.trim().to_string();
    Ok(Skill {
        name,
        description: description.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(1024).collect(),
        body,
        source: Source::BuiltIn,
    })
}

/// A skill's own name: lower case letters, digits and hyphens, at most 64.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `extra` added to `skills`, a skill of the same name replacing one.
pub fn with(mut skills: Vec<Skill>, extra: Vec<Skill>) -> Vec<Skill> {
    for s in extra {
        skills.retain(|k| k.name != s.name);
        skills.push(s);
    }
    skills
}

/// The skills enabled plugins ship, as `<plugin>:<name>`, and the problems with any that can't be
/// read.
pub fn from_plugins() -> (Vec<Skill>, Vec<String>) {
    let (mut skills, mut problems) = (Vec::new(), Vec::new());
    for p in rustgis_engine::plugins::skills() {
        match parse(&p.text) {
            Ok(s) if s.name == p.name => {
                skills.push(Skill { name: format!("{}:{}", p.plugin, p.name), source: Source::Plugin(p.plugin), ..s });
            }
            Ok(s) => problems.push(format!("{}:{}: its SKILL.md is named '{}'", p.plugin, p.name, s.name)),
            Err(e) => problems.push(format!("{}:{}: {e}", p.plugin, p.name)),
        }
    }
    (skills, problems)
}

/// The most of the system prompt the list of skills takes (in bytes): past it, the rest are
/// counted, not listed (load_skill names them all).
pub const MAX_CATALOG: usize = 16 * 1024;

/// What the system prompt says about the skills there are.
pub fn catalog(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out =
        String::from("\n\nSkills (instructions for kinds of task): when a request matches one, read it with load_skill before acting.");
    // Where each listed skill's line starts, so the note replaces whole lines.
    let mut starts = Vec::new();
    for s in skills {
        let line = format!("\n- {}: {}", s.name, s.description.split_whitespace().collect::<Vec<_>>().join(" "));
        if out.len() + line.len() > MAX_CATALOG {
            let note = |n: usize| format!("\n- … and {n} more (load_skill reads a skill by name)");
            while out.len() + note(skills.len() - starts.len()).len() > MAX_CATALOG
                && let Some(start) = starts.pop()
            {
                out.truncate(start);
            }
            out.push_str(&note(skills.len() - starts.len()));
            break;
        }
        starts.push(out.len());
        out.push_str(&line);
    }
    out
}

/// The `load_skill` tool.
pub fn tool() -> ToolDef {
    ToolDef {
        name: "load_skill",
        description: "Read a skill's instructions by name (the system prompt lists the skills). Read it before doing a task it covers. Results use the configured tool output limit; truncation is explicit.",
        schema: json!({
            "type": "object",
            "properties": {"name": {"type": "string", "description": "The skill's name"}},
            "required": ["name"]
        }),
    }
}

/// Runs `load_skill`.
pub fn load(skills: &[Skill], name: &str) -> Result<String, String> {
    let name = name.trim();
    skills.iter().find(|s| s.name == name).map(|s| s.body.clone()).ok_or_else(|| {
        let names: Vec<&str> = skills.iter().take(20).map(|s| s.name.as_str()).collect();
        let more = skills.len().saturating_sub(names.len());
        let tail = if more == 0 { String::new() } else { format!("; {more} more (see Skills in Settings)") };
        format!("no skill '{name}'; available names: {}{tail}", names.join(", "))
    })
}

/// The skills the assistant uses here: on the desktop and the command line, those switched on
/// ([`list`]); in the browser, the built-in ones and the plugins'.
pub fn installed() -> Vec<Skill> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        list(&Places::here()).0.into_iter().filter(|l| l.on).map(|l| l.skill).collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        with(built_in(), from_plugins().0)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use local::*;

/// The user's and shared skills, and which are switched on (desktop and command line).
#[cfg(not(target_arch = "wasm32"))]
mod local {
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::{MAX_SKILL, Skill, Source, built_in, from_plugins, parse, with};

    /// The most skills read from one folder or archive.
    const MAX_SKILLS: usize = 200;
    /// Candidate SKILL.md entries, including nested examples, before the nesting check.
    const MAX_SKILL_FILES: usize = 4 * MAX_SKILLS;
    /// The largest skills archive [`add`] reads.
    const MAX_ARCHIVE: u64 = 16 * 1024 * 1024;

    /// The folders skills are kept in.
    #[derive(Clone, Debug, Default)]
    pub struct Places {
        /// The user's skills: one folder each.
        pub user: Option<PathBuf>,
        /// Skills shared with other agents.
        pub shared: Option<PathBuf>,
        /// Which are switched on or off (`skills.json`).
        pub state: Option<PathBuf>,
    }

    impl Places {
        /// The usual folders: `skills` and `skills.json` in the settings folder, and
        /// `~/.agents/skills`.
        pub fn here() -> Places {
            let settings = rustgis_engine::data::dirs::config_dir();
            let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).filter(|h| !h.is_empty());
            Places {
                user: settings.as_ref().map(|d| d.join("skills")),
                shared: home.map(|h| PathBuf::from(h).join(".agents").join("skills")),
                state: settings.map(|d| d.join("skills.json")),
            }
        }
    }

    /// A skill and whether the assistant uses it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Listed {
        pub skill: Skill,
        pub on: bool,
    }

    /// Every skill (built-in, shared, the user's, the plugins'), whether each is on, and the
    /// problems with any that couldn't be read.
    pub fn list(places: &Places) -> (Vec<Listed>, Vec<String>) {
        let mut problems = Vec::new();
        let mut from = |dir: &Option<PathBuf>, shared: bool| {
            let (skills, p) = dir.as_deref().map(load_dir).unwrap_or_default();
            problems.extend(p);
            skills
                .into_iter()
                .map(|s| match (shared, s.source) {
                    (true, Source::User(d)) => Skill { source: Source::Shared(d), ..s },
                    (_, source) => Skill { source, ..s },
                })
                .collect::<Vec<_>>()
        };
        let shared = from(&places.shared, true);
        let user = from(&places.user, false);
        let (plugins, p) = from_plugins();
        problems.extend(p);
        let state = read_state(places);
        // Disabled shared instructions must not hide a built-in. Keep them as separate
        // settings rows so the user can explicitly choose the shared version later.
        let (active, inactive): (Vec<_>, Vec<_>) = shared.into_iter().partition(|s| is_on(&state, s));
        let skills = with(with(with(built_in(), active), user.clone()), plugins);
        let mut listed: Vec<_> = skills.into_iter().map(|skill| Listed { on: is_on(&state, &skill), skill }).collect();
        listed.extend(inactive.into_iter().filter(|s| !user.iter().any(|u| u.name == s.name)).map(|skill| Listed { on: false, skill }));
        (listed, problems)
    }

    /// Whether `skill` is on: a shared one when switched on, any other unless switched off.
    fn is_on(state: &Value, skill: &Skill) -> bool {
        let names = |key: &str| state[key].as_array().is_some_and(|a| a.iter().any(|n| n.as_str() == Some(skill.name.as_str())));
        match skill.source {
            Source::Shared(_) => names("enabled"),
            _ => !names("disabled"),
        }
    }

    fn read_state(places: &Places) -> Value {
        places
            .state
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))
    }

    /// Switches a skill on or off, and remembers it.
    pub fn set_enabled(places: &Places, name: &str, on: bool) -> Result<(), String> {
        let rows: Vec<Listed> = list(places).0.into_iter().filter(|l| l.skill.name == name).collect();
        // A name can have two rows (a built-in and an inactive shared one): switch the one whose
        // state changes, so `enable` can turn on the shared version.
        let skill = rows.iter().find(|l| l.on != on).or_else(|| rows.first()).ok_or_else(|| format!("no skill '{name}'"))?;
        set_source_enabled(places, name, &skill.skill.source, on)
    }

    /// Switches the specific settings row, including a shared/built-in name collision.
    pub fn set_source_enabled(places: &Places, name: &str, source: &Source, on: bool) -> Result<(), String> {
        if !list(places).0.iter().any(|l| l.skill.name == name && &l.skill.source == source) {
            return Err(format!("no skill '{name}' from that source"));
        }
        let mut state = read_state(places);
        let mut names = |key: &str, keep: bool| {
            let mut v: Vec<Value> = state[key].as_array().cloned().unwrap_or_default();
            v.retain(|n| n.as_str() != Some(name));
            if keep {
                v.push(json!(name));
            }
            state[key] = Value::Array(v);
        };
        let shared = matches!(source, Source::Shared(_));
        if shared {
            names("enabled", on);
        } else {
            names("disabled", !on);
        }
        let path = places.state.as_ref().ok_or("there is no settings folder")?;
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let text = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
        rustgis_engine::data::io::replace_atomic(path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Adds skills to the user's: a `SKILL.md`, a skill's folder, a folder of skill folders, or
    /// a `.zip` of any of these. Each is kept as `<name>/SKILL.md`, replacing a user's skill of
    /// the same name. Only `SKILL.md` is kept: the assistant reads nothing else. Returns the
    /// names added. Nothing is added if any can't be read, two have the same name, or one can't
    /// be written: each is written beside its place first, then all are put in place (a rename
    /// that fails then, rare, leaves those before it in place and cleans up the rest).
    pub fn add(places: &Places, path: &Path) -> Result<Vec<String>, String> {
        add_with(places, path, std::io::Write::write_all)
    }

    fn add_with(
        places: &Places,
        path: &Path,
        write: impl FnMut(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
    ) -> Result<Vec<String>, String> {
        add_with_ops(places, path, write, |from, to| std::fs::rename(from, to))
    }

    fn add_with_ops(
        places: &Places,
        path: &Path,
        mut write: impl FnMut(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
        mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
    ) -> Result<Vec<String>, String> {
        let texts = if path.is_dir() {
            if path.join("SKILL.md").is_file() {
                vec![read_skill(&path.join("SKILL.md"))?]
            } else {
                let (skills, problems) = load_dir(path);
                if let Some(p) = problems.into_iter().next() {
                    return Err(p);
                }
                skills.iter().map(skill_file).collect::<Result<Vec<_>, _>>()?
            }
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
            from_zip(path)?
        } else {
            vec![read_skill(path)?]
        };
        if texts.is_empty() {
            return Err(format!("{}: no SKILL.md found", path.display()));
        }
        let skills = texts.iter().map(|t| parse(t).map(|s| (s.name, t))).collect::<Result<Vec<_>, _>>()?;
        let mut seen = std::collections::HashSet::new();
        if let Some((name, _)) = skills.iter().find(|(name, _)| !seen.insert(name.clone())) {
            return Err(format!("two of the skills are named '{name}'"));
        }
        if cfg!(windows)
            && let Some((name, _)) = skills.iter().find(|(name, _)| windows_device_name(name))
        {
            return Err(format!("'{name}': reserved Windows device name; rename the skill before installing"));
        }
        let dir = places.user.as_ref().ok_or("there is no settings folder")?;
        // Where an existing skill of each name is (its folder may be named otherwise).
        let mine = load_dir(dir).0;
        // Written beside its place first (`SKILL.md.new`), all of them, then put in place.
        let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut made: Vec<PathBuf> = Vec::new();
        let mut stage = |folder: &Path, text: &str| -> Result<(), String> {
            if !folder.exists() {
                std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
                made.push(folder.to_path_buf());
            }
            let new = folder.join("SKILL.md.new");
            // A leftover from an earlier add goes first, and the file is made new: never written
            // through a link someone left in its place.
            let _ = std::fs::remove_file(&new);
            let mut f =
                std::fs::OpenOptions::new().write(true).create_new(true).open(&new).map_err(|e| format!("{}: {e}", new.display()))?;
            staged.push((new.clone(), folder.join("SKILL.md")));
            write(&mut f, text.as_bytes()).map_err(|e| format!("{}: {e}", new.display()))?;
            Ok(())
        };
        // Each skill's folder: its own when it has one, else one named after it. A new skill's
        // folder may already hold another of the user's skills (named otherwise): refused, so
        // the add doesn't half happen.
        let mut folders: Vec<(PathBuf, &String)> = Vec::new();
        for (name, text) in &skills {
            let own = mine.iter().find_map(|s| match &s.source {
                Source::User(d) if &s.name == name => Some(d.clone()),
                _ => None,
            });
            let folder = match own {
                Some(d) => d,
                None => {
                    let d = dir.join(name);
                    if let Some(other) = mine.iter().find(|s| s.source == Source::User(d.clone())) {
                        return Err(format!("'{name}': its folder {} holds your skill '{}'", d.display(), other.name));
                    }
                    d
                }
            };
            folders.push((folder, *text));
        }
        let mut failed = None;
        for (folder, text) in &folders {
            if let Err(e) = stage(folder, text) {
                failed = Some(e);
                break;
            }
        }
        if let Some(e) = failed {
            for (new, _) in &staged {
                let _ = std::fs::remove_file(new);
            }
            for folder in made.iter().rev() {
                let _ = std::fs::remove_dir(folder);
            }
            return Err(e);
        }
        // Each rename replaces one file; if one fails (a file locked on Windows), those already
        // renamed stay and the rest are cleaned up.
        for (i, (new, file)) in staged.iter().enumerate() {
            if let Err(e) = rename(new, file) {
                for (rest, _) in staged.iter().skip(i) {
                    let _ = std::fs::remove_file(rest);
                }
                for folder in made.iter().rev() {
                    // remove_dir preserves successful installs and any other nonempty folder.
                    let _ = std::fs::remove_dir(folder);
                }
                let installed: Vec<&str> = skills.iter().take(i).map(|(name, _)| name.as_str()).collect();
                return Err(format!("{}: {e}; already installed before this failure: [{}]", file.display(), installed.join(", ")));
            }
        }
        Ok(skills.into_iter().map(|(name, _)| name).collect())
    }

    fn windows_device_name(name: &str) -> bool {
        matches!(name, "con" | "prn" | "aux" | "nul")
            || name
                .strip_prefix("com")
                .or_else(|| name.strip_prefix("lpt"))
                .is_some_and(|n| n.len() == 1 && matches!(n.as_bytes().first(), Some(b'1'..=b'9')))
    }

    /// Removes one of the user's skills: its `SKILL.md`, and its folder when nothing else is in
    /// it (files the user keeps beside a skill stay). Built-in, shared and plugin skills can only
    /// be switched off.
    pub fn remove(places: &Places, name: &str) -> Result<(), String> {
        let dir = places.user.as_ref().ok_or("there is no settings folder")?;
        let folder = load_dir(dir)
            .0
            .into_iter()
            .find_map(|s| match s.source {
                Source::User(d) if s.name == name => Some(d),
                _ => None,
            })
            .ok_or_else(|| format!("'{name}' isn't one of your skills (others can be switched off)"))?;
        let file = folder.join("SKILL.md");
        std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        // Only an empty folder goes.
        let _ = std::fs::remove_dir(&folder);
        Ok(())
    }

    /// A skill's `SKILL.md`, read again from its folder.
    fn skill_file(s: &Skill) -> Result<String, String> {
        match &s.source {
            Source::User(d) | Source::Shared(d) => read_skill(&d.join("SKILL.md")),
            _ => Err(format!("{}: not in a folder", s.name)),
        }
    }

    fn read_skill(path: &Path) -> Result<String, String> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // The file itself must be regular. Linked skill folders are intentionally supported,
        // including shared ~/.agents/skills folders; they are user-selected instruction sources.
        if meta.file_type().is_symlink() {
            return Err(format!("{}: a link, not a skill's own file", path.display()));
        }
        let len = meta.len();
        if len as usize > MAX_SKILL {
            return Err(format!("{}: larger than 64 KB", path.display()));
        }
        use std::io::Read;
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut text = String::new();
        file.take(MAX_SKILL as u64 + 1).read_to_string(&mut text).map_err(|e| format!("{}: {e}", path.display()))?;
        if text.len() > MAX_SKILL {
            return Err(format!("{}: larger than 64 KB", path.display()));
        }
        Ok(text)
    }

    /// Every `SKILL.md` in a zip archive.
    fn from_zip(path: &Path) -> Result<Vec<String>, String> {
        use std::io::Read;
        let len = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
        if len > MAX_ARCHIVE {
            return Err(format!("{}: larger than 16 MB", path.display()));
        }
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: not a zip archive: {e}", path.display()))?;
        // Iterate actual entries, not by_name/file_names: archive name maps can hide duplicates.
        // Normalize Windows separators and case for portable discovery and collision checks;
        // entries are read by index and never extracted to a filesystem path.
        let mut all = Vec::new();
        for index in 0..zip.len() {
            let entry = zip.by_index_raw(index).map_err(|e| format!("{}: {e}", path.display()))?;
            // zip decodes names that are not UTF-8 as CP437, so a name fails only when corrupt;
            // such an entry cannot be a SKILL.md and is skipped.
            let Ok(name) = entry.name() else { continue };
            let name = name.replace('\\', "/");
            if name.rsplit('/').next().is_none_or(|n| !n.eq_ignore_ascii_case("SKILL.md")) {
                continue;
            }
            if name.split('/').any(|p| p == "..") {
                return Err(format!("{name}: parent path is not allowed"));
            }
            all.push((index, name.to_ascii_lowercase()));
            if all.len() > MAX_SKILL_FILES {
                return Err(format!("{}: more than {MAX_SKILL_FILES} SKILL.md files", path.display()));
            }
        }
        let mut unique = std::collections::HashSet::new();
        if let Some((_, twice)) = all.iter().find(|(_, n)| !unique.insert(n.as_str())) {
            return Err(format!("{}: {twice} is in it twice", path.display()));
        }
        let folder = |name: &str| name.strip_suffix("skill.md").unwrap_or(name).to_string();
        let folders: Vec<String> = all.iter().map(|(_, n)| folder(n)).collect();
        let entries: Vec<&(usize, String)> = all
            .iter()
            .zip(&folders)
            .filter(|((index, _), f)| !all.iter().zip(&folders).any(|((other, _), of)| other != index && f.starts_with(of.as_str())))
            .map(|(entry, _)| entry)
            .collect();
        if entries.len() > MAX_SKILLS {
            return Err(format!("{}: more than {MAX_SKILLS} skills", path.display()));
        }
        let mut texts = Vec::new();
        for (index, name) in entries {
            let f = zip.by_index(*index).map_err(|e| format!("{name}: {e}"))?;
            let mut text = String::new();
            f.take(MAX_SKILL as u64 + 1).read_to_string(&mut text).map_err(|e| format!("{name}: {e}"))?;
            if text.len() > MAX_SKILL {
                return Err(format!("{name}: larger than 64 KB"));
            }
            texts.push(text);
        }
        Ok(texts)
    }

    /// The skills in `dir`: each folder holding a `SKILL.md`, as [`Source::User`]. Unreadable or
    /// malformed ones are left out, with why.
    pub fn load_dir(dir: &Path) -> (Vec<Skill>, Vec<String>) {
        let (mut skills, mut problems) = (Vec::new(), Vec::new());
        let Ok(entries) = std::fs::read_dir(dir) else { return (skills, problems) };
        let mut folders: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join("SKILL.md").is_file()).collect();
        folders.sort();
        if folders.len() > MAX_SKILLS {
            problems.push(format!("{}: more than {MAX_SKILLS} skill folders; later folders are ignored", dir.display()));
        }
        for folder in folders.into_iter().take(MAX_SKILLS) {
            let path = folder.join("SKILL.md");
            match read_skill(&path).and_then(|text| parse(&text).map_err(|e| format!("{}: {e}", path.display()))) {
                Ok(s) => skills.push(Skill { source: Source::User(folder), ..s }),
                Err(e) => problems.push(e),
            }
        }
        (skills, problems)
    }

    /// Where the user's skills are: the `skills` folder in the app's settings folder.
    pub fn user_dir() -> Option<PathBuf> {
        Places::here().user
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn shared_toggles_preserve_a_disabled_builtin_with_the_same_name() {
            let root = tempfile::tempdir().unwrap();
            let folder = root.path().join("shared/thematic-map");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("SKILL.md"), "---\nname: thematic-map\ndescription: shared\n---\nInstructions.").unwrap();
            let places = Places { user: None, shared: Some(root.path().join("shared")), state: Some(root.path().join("state.json")) };
            set_source_enabled(&places, "thematic-map", &Source::BuiltIn, false).unwrap();
            let source = Source::Shared(folder);
            set_source_enabled(&places, "thematic-map", &source, true).unwrap();
            assert!(read_state(&places)["disabled"].as_array().unwrap().contains(&json!("thematic-map")));
            set_source_enabled(&places, "thematic-map", &source, false).unwrap();
            assert!(list(&places).0.iter().any(|l| l.skill.name == "thematic-map" && l.skill.source == Source::BuiltIn && !l.on));
        }

        #[test]
        fn oversized_skill_folder_lists_report_the_unchanged_limit() {
            let root = tempfile::tempdir().unwrap();
            for i in 0..=MAX_SKILLS {
                let folder = root.path().join(format!("skill-{i:03}"));
                std::fs::create_dir_all(&folder).unwrap();
                std::fs::write(folder.join("SKILL.md"), format!("---\nname: skill-{i}\ndescription: test\n---\nInstructions.")).unwrap();
            }
            let (skills, problems) = load_dir(root.path());
            assert_eq!(skills.len(), MAX_SKILLS);
            assert!(problems.iter().any(|p| p.contains("more than 200 skill folders")));
        }

        #[test]
        fn failed_rename_removes_empty_new_folders_but_preserves_completed_installs() {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source");
            for name in ["first", "second"] {
                let folder = source.join(name);
                std::fs::create_dir_all(&folder).unwrap();
                std::fs::write(folder.join("SKILL.md"), format!("---\nname: {name}\ndescription: test\n---\nInstructions.")).unwrap();
            }
            let places = Places { user: Some(root.path().join("skills")), shared: None, state: None };
            let mut count = 0;
            let result = add_with_ops(&places, &source, std::io::Write::write_all, |from, to| {
                count += 1;
                if count == 2 {
                    return Err(std::io::Error::other("injected rename failure"));
                }
                std::fs::rename(from, to)
            });
            let error = result.unwrap_err();
            assert!(error.contains("injected rename failure") && error.contains("already installed before this failure: [first]"));
            assert!(root.path().join("skills/first/SKILL.md").is_file());
            assert!(!root.path().join("skills/second").exists());
        }

        #[test]
        fn windows_device_names_are_recognized_without_rejecting_ordinary_names() {
            for name in ["con", "nul", "prn", "aux", "com1", "com9", "lpt1", "lpt9"] {
                assert!(windows_device_name(name));
            }
            for name in ["console", "null", "auxiliary", "com0", "com10", "lpt10", "con-map"] {
                assert!(!windows_device_name(name));
            }
        }

        #[test]
        fn enabling_by_name_turns_on_a_shared_skill_that_shares_a_builtins_name() {
            let root = tempfile::tempdir().unwrap();
            let folder = root.path().join("shared/thematic-map");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("SKILL.md"), "---\nname: thematic-map\ndescription: shared\n---\nShared instructions.").unwrap();
            let places = Places { user: None, shared: Some(root.path().join("shared")), state: Some(root.path().join("state.json")) };
            // The built-in is on, so `enable` means the shared one (`rustgis-cli skills enable`).
            set_enabled(&places, "thematic-map", true).unwrap();
            assert!(list(&places).0.iter().any(|l| l.skill.name == "thematic-map" && l.on && matches!(l.skill.source, Source::Shared(_))));
            // And `disable` switches off the one that's on: the shared one, bringing back the built-in.
            set_enabled(&places, "thematic-map", false).unwrap();
            assert!(list(&places).0.iter().any(|l| l.skill.name == "thematic-map" && l.on && l.skill.source == Source::BuiltIn));
        }

        #[test]
        fn disabled_shared_collision_preserves_builtin_and_can_be_enabled() {
            let root = tempfile::tempdir().unwrap();
            let folder = root.path().join("shared/thematic-map");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("SKILL.md"), "---\nname: thematic-map\ndescription: shared\n---\nShared instructions.").unwrap();
            let places = Places { user: None, shared: Some(root.path().join("shared")), state: Some(root.path().join("state.json")) };
            let rows = list(&places).0;
            assert!(rows.iter().any(|l| l.skill.name == "thematic-map" && l.on && l.skill.source == Source::BuiltIn));
            let shared = rows.iter().find(|l| matches!(l.skill.source, Source::Shared(_))).unwrap();
            assert!(!shared.on);
            set_source_enabled(&places, "thematic-map", &shared.skill.source, true).unwrap();
            let rows = list(&places).0;
            assert!(!rows.iter().any(|l| l.skill.name == "thematic-map" && l.skill.source == Source::BuiltIn));
            assert!(rows.iter().any(|l| l.skill.name == "thematic-map" && l.on && matches!(l.skill.source, Source::Shared(_))));
            set_source_enabled(&places, "thematic-map", &shared.skill.source, false).unwrap();
            assert!(list(&places).0.iter().any(|l| l.skill.name == "thematic-map" && l.on && l.skill.source == Source::BuiltIn));
        }

        #[test]
        fn failed_partial_write_removes_staging_and_preserves_existing_skill() {
            let root = tempfile::tempdir().unwrap();
            let places = Places { user: Some(root.path().join("skills")), shared: None, state: Some(root.path().join("state.json")) };
            let folder = places.user.as_ref().unwrap().join("one");
            std::fs::create_dir_all(&folder).unwrap();
            let original = "---\nname: one\ndescription: original\n---\nOriginal instructions.";
            std::fs::write(folder.join("SKILL.md"), original).unwrap();
            let source = root.path().join("replacement.md");
            std::fs::write(&source, "---\nname: one\ndescription: updated\n---\nReplacement instructions.").unwrap();
            let result = add_with(&places, &source, |file, text| {
                std::io::Write::write_all(file, &text[..8])?;
                Err(std::io::Error::other("injected partial write failure"))
            });
            assert!(result.unwrap_err().contains("injected partial write failure"));
            assert!(!folder.join("SKILL.md.new").exists());
            assert_eq!(std::fs::read_to_string(folder.join("SKILL.md")).unwrap(), original);
        }
    }
}
