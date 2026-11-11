//! `plugin.json`: what a plugin is and what it adds (commands, geoprocessing tools), checked
//! like any other hostile input.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The plugin ABI this build speaks (see `docs/plugins.md`).
pub const ABI: u32 = 1;

/// Limits on what a manifest may declare.
const MAX_COMMANDS: usize = 256;
const MAX_TOOLS: usize = 64;
const MAX_PARAMS: usize = 32;
const MAX_CHOICES: usize = 64;
const MAX_TEXT: usize = 2_000;
const MAX_LANGS: usize = 64;
const MAX_STRINGS: usize = 20_000;
const MAX_GROUPS: usize = 8;
const MAX_LARGE_GROUP_ITEMS: usize = 16;
const MAX_SMALL_GROUP_ITEMS: usize = 48;
const MAX_MENU_ITEMS: usize = 32;
const MAX_MENU_DEPTH: usize = 4;
const MAX_TAB_ITEMS: usize = 16_384;
const MAX_TAB_LABEL: usize = 32;
const MAX_RUN_ARGS: usize = 64;
const MAX_SKILLS: usize = 32;
/// A command may run for at most a day before RustGIS stops it.
const MAX_COMMAND_TIMEOUT: u64 = 86_400;

/// RustGIS's own ribbon tabs (and the Project button), and the names they had before: a plugin's
/// tab can't take them.
pub const RESERVED_TABS: &[&str] = &[
    "Project",
    "Home",
    "Style",
    "Analyze",
    "Publish",
    "Map",
    "Insert",
    "Analysis",
    "View",
    "Edit",
    "Imagery",
    "Share",
    "Plugins",
    "Settings",
    "Help",
    "Feature Layer",
    "Labeling",
    "Data",
    "Raster Layer",
    "Raster Data",
    "Layout",
];

/// What a plugin may do beyond reading the map. Shown to the user before installing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Change the project: run commands that edit it, add layers.
    Edit,
    /// Run commands that read or write files the user names.
    Files,
    /// Have RustGIS fetch from addresses the plugin gives it: web tile layers, data files
    /// (`layer.addUrl`) and web services.
    Network,
}

impl Permission {
    pub fn code(self) -> &'static str {
        match self {
            Permission::Edit => "edit",
            Permission::Files => "files",
            Permission::Network => "network",
        }
    }
    /// What it lets a plugin do, for the install prompt.
    pub fn describe(self) -> &'static str {
        match self {
            Permission::Edit => "Change the project (add layers, edit features, change symbology)",
            Permission::Files => "Read and write files on this computer",
            Permission::Network => "Fetch map tiles, data and web services from addresses it chooses",
        }
    }
}

/// How a plugin runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    /// A WebAssembly module, sandboxed: the same on the desktop, the command line and the web.
    #[default]
    Wasm,
    /// A program on the user's computer (`run`), not sandboxed: the desktop and command line
    /// only. It speaks the same ABI over its standard input and output.
    Process,
    /// Skills for the AI assistant only (`skills`): no code, nothing that runs.
    Skills,
}

/// A process plugin's program and arguments, the same everywhere or by platform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RunDecl {
    Everywhere(Vec<String>),
    ByPlatform {
        #[serde(default)]
        windows: Option<Vec<String>>,
        #[serde(default)]
        macos: Option<Vec<String>>,
        #[serde(default)]
        linux: Option<Vec<String>>,
        #[serde(default)]
        default: Option<Vec<String>>,
    },
}

impl RunDecl {
    /// The program and arguments for this platform, if the plugin runs on it.
    pub fn for_this_platform(&self) -> Option<&[String]> {
        match self {
            RunDecl::Everywhere(v) => Some(v),
            RunDecl::ByPlatform { windows, macos, linux, default } => {
                let own = if cfg!(target_os = "windows") {
                    windows
                } else if cfg!(target_os = "macos") {
                    macos
                } else {
                    linux
                };
                own.as_ref().or(default.as_ref()).map(Vec::as_slice)
            }
        }
    }

    fn all(&self) -> Vec<&Vec<String>> {
        match self {
            RunDecl::Everywhere(v) => vec![v],
            RunDecl::ByPlatform { windows, macos, linux, default } => [windows, macos, linux, default].into_iter().flatten().collect(),
        }
    }
}

/// A process plugin's Python environment, which RustGIS makes for it: a Python of `version`
/// with the packages `requirements` lists (a requirements file in the plugin's folder). The
/// plugin's `run` then names `python` for that environment's interpreter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PythonDecl {
    /// The Python version (`3.12`).
    #[serde(default = "default_python")]
    pub version: String,
    /// A requirements file in the plugin's folder (`requirements.txt`); empty for none.
    #[serde(default)]
    pub requirements: String,
}

fn default_python() -> String {
    "3.12".into()
}

/// A command the plugin adds: a button on the Plugins tab, an entry in command search and a
/// command id for scripts and the command line (`plugin:<plugin>/<command>`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDecl {
    pub id: String,
    pub label: String,
    /// A longer description, shown on hover.
    #[serde(default)]
    pub help: String,
    /// The name of one of RustGIS's own icons (`plugin` when unset or unknown).
    #[serde(default)]
    pub icon: String,
    /// Parameter description for scripts and agents, like a built-in command's.
    #[serde(default = "no_params")]
    pub params: String,
    /// The command's dialog: when given, its button opens a dialog with these fields, and
    /// **Run** runs the command with their values as parameters and shows what it answers (its
    /// `report` text). Fields are declared as tool parameters, plus `rasterLayer` and without
    /// `file`; a `field` lists the fields of the layer field above it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub form: Vec<ParamDecl>,
    /// Seconds the command may run before it is stopped; unset is 120. Set it for work
    /// that takes longer, such as downloading a dataset.
    #[serde(default)]
    pub timeout: Option<u64>,
}

fn no_params() -> String {
    "{}".into()
}

/// A tool parameter: the Tools pane turns its kind into a widget.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamDecl {
    pub name: String,
    pub label: String,
    /// `layer`, `vectorLayer`, `polygonLayer`, `pointLayer`, `field`, `numericField`, `number`,
    /// `text`, `choice`, `bool` or `file` (needs the `files` permission).
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    /// The default value (any JSON).
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// For `choice`: the choices.
    #[serde(default)]
    pub choices: Vec<String>,
    /// For `file`: the extensions the file picker offers (`nc`, `tif`); empty for any file.
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub help: String,
}

/// A ribbon tab of the plugin's own, beside RustGIS's tabs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabDecl {
    pub label: String,
    pub groups: Vec<GroupDecl>,
}

/// The size of ribbon controls; popup menu rows always use the standard menu layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// Full-height ribbon controls (the default).
    #[default]
    Large,
    /// Compact ribbon controls, stacked three per column.
    Small,
}

/// A group of buttons on a plugin's tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupDecl {
    pub label: String,
    /// The size of the group's ribbon controls.
    #[serde(default)]
    pub layout: Layout,
    pub items: Vec<ItemDecl>,
}

/// A button: a command id, `tool:<id>` (opens the tool in the Tools pane) or `panel` (opens the
/// plugin's panel), or a dropdown menu of them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ItemDecl {
    Entry(String),
    Menu(MenuDecl),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MenuDecl {
    /// The menu button's label.
    pub menu: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub help: String,
    /// The ribbon trigger's size, inheriting the group when unset. Ignored inside popups.
    #[serde(default)]
    pub layout: Option<Layout>,
    pub items: Vec<ItemDecl>,
}

/// What a tab entry (`command-id`, `tool:tool-id` or `panel`) refers to.
#[derive(Clone, Copy, Debug)]
pub enum Entry<'a> {
    Command(&'a CommandDecl),
    Tool(&'a ToolDecl),
    /// The button that opens the plugin's panel.
    Panel(&'a crate::panel::PanelDecl),
}

/// The parameter kinds a tool may use.
pub const PARAM_KINDS: &[&str] =
    &["layer", "vectorLayer", "polygonLayer", "pointLayer", "field", "numericField", "number", "text", "choice", "bool", "file"];

/// The field kinds a command's dialog may use: a tool's, and raster layers (a command reads
/// layers itself, by name, so a dialog can offer rasters too), but no `file` (a command that
/// wants a file has host calls for it, behind the `files` permission).
pub const FORM_KINDS: &[&str] =
    &["layer", "vectorLayer", "polygonLayer", "pointLayer", "rasterLayer", "field", "numericField", "number", "text", "choice", "bool"];

/// The most extensions a `file` parameter may list.
const MAX_EXTENSIONS: usize = 16;

/// Whether a parameter kind names a layer (its features are passed to the tool).
pub fn is_layer_kind(kind: &str) -> bool {
    matches!(kind, "layer" | "vectorLayer" | "polygonLayer" | "pointLayer")
}

/// A geoprocessing tool the plugin adds (`plugin:<plugin>/<tool>`), listed in the
/// Tools pane under "Plugins › <toolbox>".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDecl {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub summary: String,
    /// The toolbox under "Plugins ›" (the plugin's name when unset).
    #[serde(default)]
    pub toolbox: String,
    #[serde(default)]
    pub params: Vec<ParamDecl>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub license: String,
    /// The WebAssembly module, relative to `plugin.json` (WebAssembly plugins).
    #[serde(default = "default_entry")]
    pub entry: String,
    /// `wasm` (the default) or `process`.
    #[serde(default)]
    pub runtime: Runtime,
    /// For a process plugin: the program and its arguments, run in the plugin's folder. A bare
    /// name is looked up on `PATH`; a relative path is in the plugin's folder.
    #[serde(default)]
    pub run: Option<RunDecl>,
    /// For a process plugin in Python: the environment RustGIS makes for it.
    #[serde(default)]
    pub python: Option<PythonDecl>,
    /// For a process plugin: keep the program running between its commands (a session), so it
    /// can ask RustGIS for things at any time and hear when the map changes. See the `session` module.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub session: bool,
    /// The plugin ABI the module was built for.
    #[serde(default = "default_abi")]
    pub abi: u32,
    /// The oldest RustGIS that can run it.
    #[serde(default)]
    pub min_rustgis_version: String,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    #[serde(default)]
    pub tools: Vec<ToolDecl>,
    /// A ribbon tab of its own (optional): its commands and tools in groups and menus.
    #[serde(default)]
    pub tab: Option<TabDecl>,
    /// A panel of its own (optional): a pane of widgets bound to its commands (see
    /// [`crate::panel`]).
    #[serde(default)]
    pub panel: Option<crate::panel::PanelDecl>,
    /// Skills for the AI assistant: folders in the package's `skills` folder, each holding a
    /// `SKILL.md`. The assistant offers them as `<plugin>:<name>` while the plugin is enabled.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Translations of the plugin's own strings (labels, help, tool and parameter names), by
    /// language code (`de`, `zh-CN`): `{"de": {"Say Hello": "Hallo sagen"}}`.
    #[serde(default)]
    pub i18n: BTreeMap<String, BTreeMap<String, String>>,
}

/// `s` in language `lang` from `i18n` (translations by language code), or `s`. Tries the exact
/// code, then the language without its region, then any region of it, so `zh` finds `zh-CN`.
pub fn translate<'a>(i18n: &'a BTreeMap<String, BTreeMap<String, String>>, lang: &str, s: &'a str) -> &'a str {
    let base_of = |l: &str| l.split(['-', '_']).next().unwrap_or(l).to_ascii_lowercase();
    let base = base_of(lang);
    let pick = |want: &dyn Fn(&str) -> bool| {
        i18n.iter().filter(|(k, _)| want(k)).find_map(|(_, t)| t.get(s)).map(String::as_str).filter(|t| !t.is_empty())
    };
    pick(&|k| k.eq_ignore_ascii_case(lang))
        .or_else(|| pick(&|k| k.eq_ignore_ascii_case(&base)))
        .or_else(|| pick(&|k| base_of(k) == base))
        .unwrap_or(s)
}

fn default_entry() -> String {
    "plugin.wasm".into()
}

fn default_abi() -> u32 {
    ABI
}

/// A plugin, command, tool or parameter id: lower-case letters, digits and hyphens (and
/// underscores, for parameter names), starting with a letter, at most 64 characters.
pub fn valid_id(s: &str, underscore: bool) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || (underscore && (c == '_' || c.is_ascii_uppercase())))
}

/// A path inside a package: relative, forward slashes, no `..`.
pub fn valid_path(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && !s.starts_with('/')
        && !s.contains('\\')
        && !s.contains(':')
        && s.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// A version's numbers (`1.2.3`, `1.2`, `1.2.3-beta.1`: the pre-release part is ignored).
pub fn version_parts(v: &str) -> Option<[u64; 3]> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut out = [0u64; 3];
    let mut n = 0;
    for (i, part) in core.split('.').enumerate() {
        *out.get_mut(i)? = part.parse().ok()?;
        n = i + 1;
    }
    (n > 0).then_some(out)
}

/// Compares two versions; an unreadable one sorts first.
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    version_parts(a).cmp(&version_parts(b))
}

/// RustGIS's own version.
pub const RUSTGIS_VERSION: &str = env!("CARGO_PKG_VERSION");

fn check_text(what: &str, s: &str, required: bool) -> Result<(), String> {
    if required && s.trim().is_empty() {
        return Err(format!("{what} is required"));
    }
    if s.len() > MAX_TEXT {
        return Err(format!("{what} is too long"));
    }
    Ok(())
}

/// The number in `text`, as a command dialog's number field reads it and sends it: an integer
/// exactly, as one (for commands that read integers); a whole number written otherwise (`2.0`,
/// `1e3`) as an integer too; others as floats. `None` when empty, not a number, an integer past
/// `i64`, a number written otherwise from 2^52 (`1e16`, `9007199254740993.0`,
/// `4503599627370496.5`), where a float holds no fraction and soon not every whole number, or a
/// number too small for a float (`1e-400`): a float would round those unseen.
pub fn form_number(text: &str) -> Option<serde_json::Value> {
    let t = text.trim();
    if let Ok(i) = t.parse::<i64>() {
        return Some(serde_json::json!(i));
    }
    if t.strip_prefix(['-', '+']).unwrap_or(t).bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let x = t.parse::<f64>().ok().filter(|x| x.is_finite())?;
    // Too small for a float, a number other than zero reads as zero: not taken.
    if x == 0.0 && t.split(['e', 'E']).next().is_some_and(|m| m.bytes().any(|b| (b'1'..=b'9').contains(&b))) {
        return None;
    }
    // From 2^52 a float holds no fraction: a number written otherwise may already be rounded.
    if x.abs() >= 4_503_599_627_370_496.0 {
        return None;
    }
    Some(if x.fract() == 0.0 { serde_json::json!(x as i64) } else { serde_json::json!(x) })
}

/// [`form_number`] for a JSON value, a number or a number written as text: an integer past `i64`
/// is `None` either way.
pub fn form_number_value(v: &serde_json::Value) -> Option<serde_json::Value> {
    match v {
        serde_json::Value::String(t) => form_number(t),
        serde_json::Value::Number(n) => form_number(&n.to_string()),
        _ => None,
    }
}

/// Checks what a dialog's fields need beyond a tool parameter's, so that nothing is lost unseen:
/// a default the field can show and send (a number, or a number written as text, for `number`;
/// `true` or `false` for `bool`; one of its choices for `choice`; text for the other kinds), and
/// a vector layer field (`vectorLayer`, `polygonLayer` or `pointLayer`) above each `field` or
/// `numericField`, whose fields it lists, and required when the field is.
fn check_form(owner: &str, form: &[ParamDecl]) -> Result<(), String> {
    // The nearest layer field above: its kind, and whether it is required.
    let mut layer: Option<(&str, bool)> = None;
    for p in form {
        if let Some(d) = &p.default {
            let fits = match p.kind.as_str() {
                "number" => form_number_value(d).is_some(),
                "bool" => d.is_boolean(),
                "choice" => d.as_str().is_some_and(|t| p.choices.iter().any(|c| c == t)),
                _ => d.is_string(),
            };
            if !fits {
                return Err(format!("plugin.json: {owner}: '{}' has a default of another kind than its field ({})", p.name, p.kind));
            }
        }
        match p.kind.as_str() {
            "layer" | "vectorLayer" | "polygonLayer" | "pointLayer" | "rasterLayer" => layer = Some((p.kind.as_str(), p.required)),
            // `layer` may pick a raster too, which has no fields.
            "field" | "numericField" if !matches!(layer, Some(("vectorLayer" | "polygonLayer" | "pointLayer", _))) => {
                return Err(format!(
                    "plugin.json: {owner}: '{}' lists the fields of a layer, so it needs a vector layer field above it",
                    p.name
                ));
            }
            // Left at `—`, an optional layer would leave a required field nothing to pick.
            "field" | "numericField" if p.required && layer.is_some_and(|(_, required)| !required) => {
                return Err(format!("plugin.json: {owner}: '{}' is required, so the layer field above it must be too", p.name));
            }
            _ => {}
        }
    }
    Ok(())
}

impl Manifest {
    /// Reads and checks `plugin.json`.
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let m: Manifest = serde_json::from_str(text).map_err(|e| format!("plugin.json: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    /// Checks everything a host relies on: ids, sizes, kinds, versions.
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id, false) {
            return Err(format!(
                "plugin.json: id '{}' must be lower-case letters, digits and hyphens, starting with a letter",
                self.id.chars().take(80).collect::<String>()
            ));
        }
        check_text("name", &self.name, true)?;
        check_text("description", &self.description, false)?;
        check_text("author", &self.author, false)?;
        check_text("homepage", &self.homepage, false)?;
        check_text("license", &self.license, false)?;
        if version_parts(&self.version).is_none() {
            return Err(format!("plugin.json: version '{}' is not a version number like 1.0.0", self.version));
        }
        match self.runtime {
            Runtime::Wasm => {
                if !valid_path(&self.entry) || !self.entry.ends_with(".wasm") {
                    return Err("plugin.json: entry must be a relative path to a .wasm file inside the package".into());
                }
                if self.run.is_some() || self.python.is_some() || self.session {
                    return Err("plugin.json: 'run', 'python' and 'session' are for process plugins (\"runtime\": \"process\")".into());
                }
            }
            Runtime::Process => {
                if let Some(py) = &self.python {
                    let mut parts = py.version.split('.');
                    let numbers = py.version.split('.').all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()));
                    if !numbers || parts.next() != Some("3") || py.version.len() > 16 {
                        return Err(format!("plugin.json: python version '{}' must look like 3.12", py.version));
                    }
                    if !py.requirements.is_empty() && !valid_path(&py.requirements) {
                        return Err("plugin.json: python requirements must be a file in the plugin's folder".into());
                    }
                }
                let run = self.run.as_ref().ok_or("plugin.json: a process plugin needs 'run': its program and arguments")?;
                let all = run.all();
                if all.is_empty() {
                    return Err("plugin.json: 'run' names no program".into());
                }
                for argv in all {
                    if argv.first().is_none_or(|p| p.trim().is_empty())
                        || argv.len() > MAX_RUN_ARGS
                        || argv.iter().any(|a| a.len() > MAX_TEXT || a.contains('\0'))
                    {
                        return Err(format!("plugin.json: 'run' must be a program and at most {} arguments", MAX_RUN_ARGS - 1));
                    }
                    // A program by a relative path is in the plugin's folder: no `..` out of it.
                    if let Some(p) = argv.first()
                        && p.contains(['/', '\\'])
                        && !std::path::Path::new(p).is_absolute()
                        && !valid_path(p)
                    {
                        return Err(format!(
                            "plugin.json: 'run' names '{p}': a program in the plugin's folder (forward slashes, no '..'), one on PATH, or a full path"
                        ));
                    }
                }
            }
            Runtime::Skills => {
                if self.run.is_some() || self.python.is_some() || self.session {
                    return Err("plugin.json: 'run', 'python' and 'session' are for process plugins (\"runtime\": \"process\")".into());
                }
                if !self.commands.is_empty()
                    || !self.tools.is_empty()
                    || self.tab.is_some()
                    || self.panel.is_some()
                    || !self.permissions.is_empty()
                {
                    return Err(
                        "plugin.json: a skills package (\"runtime\": \"skills\") has no commands, tools, tab, panel or permissions".into(),
                    );
                }
                if self.skills.is_empty() {
                    return Err("plugin.json: a skills package (\"runtime\": \"skills\") lists its skills in 'skills'".into());
                }
            }
        }
        if self.skills.len() > MAX_SKILLS {
            return Err(format!("plugin.json: at most {MAX_SKILLS} skills"));
        }
        let mut names = std::collections::HashSet::new();
        for name in &self.skills {
            if !valid_id(name, false) || !names.insert(name.as_str()) {
                return Err(format!(
                    "plugin.json: skill '{}' is invalid or repeated (lower-case letters, digits and hyphens)",
                    name.chars().take(80).collect::<String>()
                ));
            }
        }
        if self.abi != ABI {
            return Err(format!("{} was built for plugin ABI {}; this RustGIS runs ABI {ABI}", self.name, self.abi));
        }
        if !self.min_rustgis_version.is_empty() {
            if version_parts(&self.min_rustgis_version).is_none() {
                return Err("plugin.json: minRustgisVersion is not a version number".into());
            }
            if version_cmp(RUSTGIS_VERSION, &self.min_rustgis_version).is_lt() {
                return Err(format!("{} needs RustGIS {} or newer (this is {RUSTGIS_VERSION})", self.name, self.min_rustgis_version));
            }
        }
        if self.commands.len() > MAX_COMMANDS || self.tools.len() > MAX_TOOLS {
            return Err(format!("plugin.json: at most {MAX_COMMANDS} commands and {MAX_TOOLS} tools"));
        }
        let mut seen = std::collections::HashSet::new();
        for c in &self.commands {
            if !valid_id(&c.id, false) || !seen.insert(("c", c.id.as_str())) {
                return Err(format!("plugin.json: command id '{}' is invalid or repeated", c.id));
            }
            check_text("command label", &c.label, true)?;
            check_text("command help", &c.help, false)?;
            check_text("command params", &c.params, false)?;
            check_text("command icon", &c.icon, false)?;
            self.check_params(&c.label, &c.form, FORM_KINDS)?;
            check_form(&c.label, &c.form)?;
            if let Some(seconds) = c.timeout
                && (seconds == 0 || seconds > MAX_COMMAND_TIMEOUT)
            {
                return Err(format!(
                    "plugin.json: command '{}' has a timeout of {seconds} seconds; it must be 1 to {MAX_COMMAND_TIMEOUT}",
                    c.id
                ));
            }
        }
        for t in &self.tools {
            if !valid_id(&t.id, false) || !seen.insert(("t", t.id.as_str())) {
                return Err(format!("plugin.json: tool id '{}' is invalid or repeated", t.id));
            }
            check_text("tool name", &t.name, true)?;
            check_text("tool summary", &t.summary, false)?;
            check_text("toolbox", &t.toolbox, false)?;
            self.check_params(&t.name, &t.params, PARAM_KINDS)?;
        }
        // Before the tab, which may name it.
        if let Some(panel) = &self.panel {
            if self.commands.iter().any(|c| c.id == "panel") {
                return Err(
                    "plugin.json: a plugin with a panel can't have a command called 'panel' (a tab's 'panel' opens the panel)".into()
                );
            }
            panel.validate(&|c| self.commands.iter().any(|x| x.id == c))?;
        }
        if let Some(tab) = &self.tab {
            self.validate_tab(tab)?;
        }
        if self.i18n.len() > MAX_LANGS || self.i18n.values().map(BTreeMap::len).sum::<usize>() > MAX_STRINGS {
            return Err("plugin.json: too many translations".into());
        }
        for (lang, strings) in &self.i18n {
            check_text("language", lang, true)?;
            for (k, v) in strings {
                check_text("translated string", k, true)?;
                check_text("translation", v, false)?;
            }
        }
        Ok(())
    }

    /// Checks a tool's parameters or a command's dialog fields (`owner` names it in errors).
    fn check_params(&self, owner: &str, params: &[ParamDecl], kinds: &[&str]) -> Result<(), String> {
        if params.len() > MAX_PARAMS {
            return Err(format!("plugin.json: {owner} has more than {MAX_PARAMS} parameters"));
        }
        let mut names = std::collections::HashSet::new();
        for p in params {
            if !valid_id(&p.name, true) || !names.insert(p.name.as_str()) {
                return Err(format!("plugin.json: {owner}: parameter '{}' is invalid or repeated", p.name));
            }
            check_text("parameter label", &p.label, true)?;
            check_text("parameter help", &p.help, false)?;
            if !kinds.contains(&p.kind.as_str()) {
                return Err(format!("plugin.json: {owner}: unknown parameter kind '{}'", p.kind));
            }
            if p.kind == "choice" && (p.choices.is_empty() || p.choices.len() > MAX_CHOICES) {
                return Err(format!("plugin.json: {owner}: '{}' needs 1 to {MAX_CHOICES} choices", p.name));
            }
            for c in &p.choices {
                check_text("choice", c, true)?;
            }
            // A file parameter hands the tool a file's contents: only with `files`.
            if p.kind == "file" && !self.has(Permission::Files) {
                return Err(format!("plugin.json: {owner}: '{}' is a file, which needs the 'files' permission", p.name));
            }
            if p.extensions.len() > MAX_EXTENSIONS
                || p.extensions.iter().any(|e| e.is_empty() || e.len() > 16 || !e.chars().all(|c| c.is_ascii_alphanumeric()))
            {
                return Err(format!(
                    "plugin.json: {owner}: '{}' has invalid extensions (at most {MAX_EXTENSIONS}, letters and digits without the dot)",
                    p.name
                ));
            }
            if let Some(d) = &p.default
                && d.to_string().len() > MAX_TEXT
            {
                return Err(format!("plugin.json: {owner}: '{}' has too long a default", p.name));
            }
        }
        Ok(())
    }

    /// `s` in language `lang` (`de`, `zh-CN`), or `s` (see [`translate`]).
    pub fn translate<'a>(&'a self, lang: &str, s: &'a str) -> &'a str {
        translate(&self.i18n, lang, s)
    }

    /// The command, tool or panel a tab entry names.
    pub fn entry(&self, e: &str) -> Option<Entry<'_>> {
        if e == "panel"
            && let Some(p) = &self.panel
        {
            return Some(Entry::Panel(p));
        }
        match e.strip_prefix("tool:") {
            Some(t) => self.tools.iter().find(|x| x.id == t).map(Entry::Tool),
            None => self.commands.iter().find(|x| x.id == e).map(Entry::Command),
        }
    }

    fn validate_tab(&self, tab: &TabDecl) -> Result<(), String> {
        let label = tab.label.trim();
        if label.is_empty() || label.chars().count() > MAX_TAB_LABEL || label.contains('\n') {
            return Err(format!("plugin.json: the tab's label must be one line of 1 to {MAX_TAB_LABEL} characters"));
        }
        if RESERVED_TABS.iter().any(|r| r.eq_ignore_ascii_case(label)) {
            return Err(format!("plugin.json: the tab can't be called '{label}', the name of one of RustGIS's tabs"));
        }
        if tab.groups.is_empty() || tab.groups.len() > MAX_GROUPS {
            return Err(format!("plugin.json: the tab needs 1 to {MAX_GROUPS} groups"));
        }
        let mut total = 0;
        for g in &tab.groups {
            check_text("group label", &g.label, true)?;
            let max_items = match g.layout {
                Layout::Large => MAX_LARGE_GROUP_ITEMS,
                Layout::Small => MAX_SMALL_GROUP_ITEMS,
            };
            if g.items.is_empty() || g.items.len() > max_items {
                return Err(format!("plugin.json: tab group '{}' needs 1 to {max_items} items", g.label));
            }
            for item in &g.items {
                self.validate_tab_item(item, 0, &mut total)?;
            }
        }
        Ok(())
    }

    fn validate_tab_item(&self, item: &ItemDecl, menu_depth: usize, total: &mut usize) -> Result<(), String> {
        if *total >= MAX_TAB_ITEMS {
            return Err(format!("plugin.json: the tab has more than {MAX_TAB_ITEMS} total items"));
        }
        *total += 1;
        match item {
            ItemDecl::Entry(e) => {
                if self.entry(e).is_none() {
                    return Err(format!(
                        "plugin.json: the tab names '{e}', which isn't one of the plugin's commands, tools (tool:<id>) or its panel (panel)"
                    ));
                }
            }
            ItemDecl::Menu(m) => {
                if menu_depth >= MAX_MENU_DEPTH {
                    return Err(format!("plugin.json: menus may nest at most {MAX_MENU_DEPTH} levels"));
                }
                check_text("menu label", &m.menu, true)?;
                check_text("menu help", &m.help, false)?;
                check_text("menu icon", &m.icon, false)?;
                if m.items.is_empty() || m.items.len() > MAX_MENU_ITEMS {
                    return Err(format!("plugin.json: menu '{}' needs 1 to {MAX_MENU_ITEMS} items", m.menu));
                }
                for item in &m.items {
                    self.validate_tab_item(item, menu_depth + 1, total)?;
                }
            }
        }
        Ok(())
    }

    pub fn has(&self, p: Permission) -> bool {
        self.permissions.contains(&p)
    }
}
