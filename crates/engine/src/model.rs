//! ModelBuilder's models as graphs: the ports each node offers, the problems that keep a model
//! from running, the order its nodes run in, an automatic layout, and the run itself.
//!
//! A graph has three kinds of node. An **input** names a layer of the map; a **tool** is a
//! geoprocessing tool, whose layer parameters are its input ports; an **output** keeps the
//! result wired into it as a layer of its name. Every input and tool node has one output port,
//! [`OUT`] (its layer); every output node one input port, [`IN`]. A tool's results that only feed
//! other tools are intermediate: they are removed from the map once the model has run.

use std::collections::{HashMap, HashSet};

use rustgis_analysis::{ParamKind, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Session;
use crate::project::{ModelEdge, ModelGraph, ModelNode, ModelNodeKind, ModelStep};

/// The most nodes and edges a graph may have (files and parameters are hostile).
pub const MAX_NODES: usize = 2000;
pub const MAX_EDGES: usize = 4000;
/// Identifier length and canvas coordinate limits used by graph commands and file checks.
pub(crate) const MAX_NODE_ID: usize = 100;
const MAX_NODE_POSITION: f32 = 1e6;
/// The output port of input and tool nodes.
pub const OUT: &str = "out";
/// The input port of output nodes.
pub const IN: &str = "in";
/// The layout grid: the distance between columns and rows, and the margin.
pub const COLUMN: f32 = 216.0;
pub const ROW: f32 = 100.0;
pub const MARGIN: f32 = 40.0;

/// What a port carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PortKind {
    Vector,
    Raster,
    Any,
}

impl PortKind {
    /// Whether a port of this kind can feed one of `other`.
    pub fn fits(self, other: PortKind) -> bool {
        self == PortKind::Any || other == PortKind::Any || self == other
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Port {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: PortKind,
    pub required: bool,
    /// Takes several edges (a list of layers).
    pub many: bool,
}

/// The port a tool parameter of this kind makes, if it takes layers: its kind and whether it
/// takes several.
pub fn port_kind(k: ParamKind) -> Option<(PortKind, bool)> {
    match k {
        ParamKind::Layer => Some((PortKind::Any, false)),
        ParamKind::PolygonLayer | ParamKind::PointLayer | ParamKind::VectorLayer => Some((PortKind::Vector, false)),
        ParamKind::RasterLayer => Some((PortKind::Raster, false)),
        ParamKind::RasterLayers => Some((PortKind::Raster, true)),
        ParamKind::VectorLayers => Some((PortKind::Vector, true)),
        _ => None,
    }
}

/// A tool node's tool, if RustGIS has it.
pub fn tool_of(node: &ModelNode) -> Option<&'static ToolSpec> {
    node.tool.as_deref().and_then(|id| rustgis_analysis::resolve_published(id).map(|r| r.spec()))
}

/// The ports edges can come into.
pub fn input_ports(node: &ModelNode) -> Vec<Port> {
    match node.kind {
        ModelNodeKind::Input => Vec::new(),
        ModelNodeKind::Output => vec![Port { id: IN, label: "Result", kind: PortKind::Any, required: true, many: false }],
        ModelNodeKind::Tool => tool_of(node)
            .map(|t| {
                t.params
                    .iter()
                    .filter_map(|p| {
                        port_kind(p.kind).map(|(kind, many)| Port { id: p.name, label: p.label, kind, required: p.required, many })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The ports edges can leave from.
pub fn output_ports(node: &ModelNode) -> Vec<Port> {
    match node.kind {
        ModelNodeKind::Output => Vec::new(),
        ModelNodeKind::Tool if tool_of(node).is_some_and(crate::cmd::gp::writes_files) => Vec::new(),
        _ => vec![Port { id: OUT, label: "Output", kind: PortKind::Any, required: false, many: false }],
    }
}

/// What an input node's layer carries (any, when it isn't in the map).
fn layer_kind(s: &Session, node: &ModelNode) -> PortKind {
    match node.layer.as_deref().and_then(|n| s.map().layer_by_name(n)) {
        Some(l) if l.is_raster() => PortKind::Raster,
        Some(_) => PortKind::Vector,
        None => PortKind::Any,
    }
}

/// What a node's output port carries: an input's layer kind; a tool's isn't known until it runs.
pub fn output_kind(s: &Session, node: &ModelNode) -> PortKind {
    match node.kind {
        ModelNodeKind::Input => layer_kind(s, node),
        _ => PortKind::Any,
    }
}

/// A node's title: its layer, its result's name or its tool's name.
pub fn title(node: &ModelNode) -> String {
    match node.kind {
        ModelNodeKind::Input => node.layer.clone().filter(|l| !l.is_empty()).unwrap_or_else(|| "Input".into()),
        ModelNodeKind::Output => node.name.clone().filter(|l| !l.is_empty()).unwrap_or_else(|| "Model output".into()),
        ModelNodeKind::Tool => tool_of(node).map(|t| t.name.to_string()).or_else(|| node.tool.clone()).unwrap_or_else(|| "Tool".into()),
    }
}

/// Something that keeps a model from running.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Problem {
    pub code: &'static str,
    /// The node it concerns, if one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub message: String,
}

fn problem(code: &'static str, node: Option<&str>, message: impl Into<String>) -> Problem {
    Problem { code, node: node.map(str::to_string), message: message.into() }
}

/// Whether a parameter value is set: not null, nor an empty string.
fn is_set(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        _ => true,
    }
}

/// The problems that keep a graph from running; empty when it can run.
pub fn validate(s: &Session, g: &ModelGraph) -> Vec<Problem> {
    let mut out = Vec::new();
    if g.nodes.len() > MAX_NODES || g.edges.len() > MAX_EDGES {
        out.push(problem("too-big", None, format!("A model can have at most {MAX_NODES} nodes and {MAX_EDGES} connections.")));
        return out;
    }
    if g.nodes.iter().all(|n| n.kind != ModelNodeKind::Tool) {
        out.push(problem("no-tool", None, "Add a tool to the model."));
    }
    let mut seen = HashSet::new();
    for n in &g.nodes {
        if !seen.insert(n.id.as_str()) {
            out.push(problem("duplicate-node", Some(&n.id), "Two or more nodes share the same id."));
        }
    }
    // Looked up by id rather than scanned: a graph may have thousands of nodes.
    let by_id: HashMap<&str, &ModelNode> = g.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let node = |id: &str| by_id.get(id).copied();
    let wired_ports: HashSet<(&str, &str)> = g.edges.iter().map(|e| (e.to.as_str(), e.to_port.as_str())).collect();
    let mut into: HashMap<(&str, &str), usize> = HashMap::new();
    for e in &g.edges {
        let (Some(from), Some(to)) = (node(&e.from), node(&e.to)) else {
            out.push(problem("dangling-edge", None, "A connection points at a node that no longer exists."));
            continue;
        };
        if e.from == e.to {
            out.push(problem("self-edge", Some(&e.to), "A node cannot connect to itself."));
            continue;
        }
        let Some(fp) = output_ports(from).into_iter().find(|p| p.id == e.from_port) else {
            out.push(problem("unknown-port", Some(&e.from), "A connection refers to a port that no longer exists."));
            continue;
        };
        let Some(tp) = input_ports(to).into_iter().find(|p| p.id == e.to_port) else {
            out.push(problem("unknown-port", Some(&e.to), "A connection refers to a port that no longer exists."));
            continue;
        };
        let n = into.entry((e.to.as_str(), tp.id)).or_insert(0);
        *n += 1;
        if *n == 2 && !tp.many {
            out.push(problem("duplicate-input", Some(&e.to), format!("\"{}\" already has an incoming connection.", tp.label)));
        }
        if !output_kind(s, from).max_kind(fp.kind).fits(tp.kind) {
            out.push(problem("type-mismatch", Some(&e.to), format!("\"{}\" takes a {} layer.", tp.label, kind_name(tp.kind))));
        }
    }
    // One output per result: a second would rename the layer the first named.
    let mut kept: HashSet<&str> = HashSet::new();
    for e in g.edges.iter().filter(|e| node(&e.to).is_some_and(|n| n.kind == ModelNodeKind::Output)) {
        if !kept.insert(e.from.as_str()) {
            out.push(problem("shared-output", Some(&e.to), "That result already has an output; a result keeps one name."));
        }
    }
    for n in &g.nodes {
        let wired = |port: &str| wired_ports.contains(&(n.id.as_str(), port));
        match n.kind {
            ModelNodeKind::Input => match n.layer.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
                None => out.push(problem("missing-layer", Some(&n.id), "Choose an input layer.")),
                Some(l) if s.map().layer_by_name(l).is_none() && crate::project::find_table(&s.project.tables, l).is_none() => {
                    out.push(problem("missing-layer", Some(&n.id), format!("The map has no layer named \"{l}\".")))
                }
                Some(_) => {}
            },
            ModelNodeKind::Output => {
                if !wired(IN) {
                    out.push(problem("missing-input", Some(&n.id), "Connect a result to this output."));
                }
            }
            ModelNodeKind::Tool => {
                let Some(t) = tool_of(n) else {
                    out.push(problem("unknown-tool", Some(&n.id), format!("Unknown tool \"{}\".", n.tool.as_deref().unwrap_or(""))));
                    continue;
                };
                for p in t.params.iter().filter(|p| p.required) {
                    let port = port_kind(p.kind).is_some();
                    if !(port && wired(p.name) || is_set(n.params.get(p.name))) && p.default.is_empty() {
                        let msg = if port { "\"{p}\" needs a connection or a layer." } else { "\"{p}\" needs a value." };
                        out.push(problem("missing-input", Some(&n.id), msg.replace("{p}", p.label)));
                    }
                }
            }
        }
    }
    if topological_order(g).is_none() {
        out.push(problem("cycle", None, "The model contains a loop."));
    }
    out
}

impl PortKind {
    /// The more specific of a node's known kind and its port's.
    fn max_kind(self, port: PortKind) -> PortKind {
        if self == PortKind::Any { port } else { self }
    }
}

fn kind_name(k: PortKind) -> &'static str {
    match k {
        PortKind::Vector => "vector",
        PortKind::Raster => "raster",
        PortKind::Any => "",
    }
}

/// The nodes (by index) in an order where every node comes after the ones feeding it; `None`
/// when the graph has a loop. Ties keep the left-to-right, top-to-bottom order of the canvas.
pub fn topological_order(g: &ModelGraph) -> Option<Vec<usize>> {
    let index: HashMap<&str, usize> = g.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let mut indegree = vec![0usize; g.nodes.len()];
    let mut next: Vec<Vec<usize>> = vec![Vec::new(); g.nodes.len()];
    for e in &g.edges {
        if let (Some(&a), Some(&b)) = (index.get(e.from.as_str()), index.get(e.to.as_str()))
            && let (Some(d), Some(nx)) = (indegree.get_mut(b), next.get_mut(a))
        {
            *d += 1;
            nx.push(b);
        }
    }
    let place = |i: &usize| g.nodes.get(*i).map(|n| (n.x, n.y)).unwrap_or_default();
    let mut ready: Vec<usize> = (0..g.nodes.len()).filter(|i| indegree.get(*i) == Some(&0)).collect();
    let mut order = Vec::with_capacity(g.nodes.len());
    while !ready.is_empty() {
        // The left-most (then top-most) ready node next.
        ready.sort_by(|a, b| {
            let (pa, pb) = (place(a), place(b));
            pb.0.total_cmp(&pa.0).then(pb.1.total_cmp(&pa.1))
        });
        let Some(i) = ready.pop() else { break };
        order.push(i);
        for &j in next.get(i).map(Vec::as_slice).unwrap_or(&[]) {
            if let Some(d) = indegree.get_mut(j) {
                *d -= 1;
                if *d == 0 {
                    ready.push(j);
                }
            }
        }
    }
    (order.len() == g.nodes.len()).then_some(order)
}

/// Lays the nodes out along the flow: each node in the column of its longest path from an
/// input, the nodes of a column in their current top-to-bottom order.
pub fn layout(g: &mut ModelGraph) {
    let order = topological_order(g).unwrap_or_else(|| (0..g.nodes.len()).collect());
    let index: HashMap<String, usize> = g.nodes.iter().enumerate().map(|(i, n)| (n.id.clone(), i)).collect();
    let mut depth = vec![0usize; g.nodes.len()];
    for &i in &order {
        let Some(id) = g.nodes.get(i).map(|n| n.id.clone()) else { continue };
        let d = depth.get(i).copied().unwrap_or(0);
        for e in g.edges.iter().filter(|e| e.from == id) {
            if let Some(dj) = index.get(&e.to).and_then(|&j| depth.get_mut(j)) {
                *dj = (*dj).max(d + 1).min(MAX_NODES);
            }
        }
    }
    // Outputs line up in the column after their result.
    let mut columns: Vec<Vec<usize>> = Vec::new();
    for (i, d) in depth.iter().enumerate() {
        if columns.len() <= *d {
            columns.resize(*d + 1, Vec::new());
        }
        if let Some(c) = columns.get_mut(*d) {
            c.push(i);
        }
    }
    for (c, nodes) in columns.iter_mut().enumerate() {
        nodes.sort_by(|a, b| {
            let y = |i: &usize| g.nodes.get(*i).map(|n| n.y).unwrap_or_default();
            y(a).total_cmp(&y(b))
        });
        for (r, &i) in nodes.iter().enumerate() {
            if let Some(n) = g.nodes.get_mut(i) {
                n.x = MARGIN + c as f32 * COLUMN;
                n.y = MARGIN + r as f32 * ROW;
            }
        }
    }
}

/// An id not used by any node: `prefix` and the first free number.
pub fn new_id(g: &ModelGraph, prefix: &str) -> String {
    (1..).map(|i| format!("{prefix}{i}")).find(|id| !g.nodes.iter().any(|n| &n.id == id)).unwrap_or_else(|| prefix.to_string())
}

/// A graph from steps run in order (`%n` for step n's output): an input node for each layer
/// the steps read, a tool node per step, laid out.
pub fn steps_to_graph(steps: &[ModelStep]) -> ModelGraph {
    let mut g = ModelGraph::default();
    let mut inputs: HashMap<String, String> = HashMap::new();
    for (i, st) in steps.iter().take(MAX_NODES / 2).enumerate() {
        let id = format!("t{}", i + 1);
        let mut params = match &st.params {
            Value::Object(o) => o.clone(),
            _ => serde_json::Map::new(),
        };
        let node = ModelNode { id: id.clone(), kind: ModelNodeKind::Tool, tool: Some(st.tool.clone()), ..Default::default() };
        for port in input_ports(&node) {
            let Some(v) = params.get(port.id).and_then(Value::as_str).map(str::to_string) else { continue };
            let parts: Vec<&str> = if port.many { v.split(';').map(str::trim).filter(|x| !x.is_empty()).collect() } else { vec![v.trim()] };
            for part in parts {
                let from = match part.strip_prefix('%').and_then(|n| n.parse::<usize>().ok()) {
                    Some(n) if (1..=i).contains(&n) => format!("t{n}"),
                    Some(_) => continue,
                    None => {
                        let next = format!("i{}", inputs.len() + 1);
                        let iid = inputs.entry(part.to_ascii_lowercase()).or_insert_with(|| next.clone()).clone();
                        if iid == next {
                            g.nodes.push(ModelNode {
                                id: iid.clone(),
                                kind: ModelNodeKind::Input,
                                layer: Some(part.to_string()),
                                ..Default::default()
                            });
                        }
                        iid
                    }
                };
                g.edges.push(ModelEdge { from, from_port: OUT.into(), to: id.clone(), to_port: port.id.into() });
            }
            params.remove(port.id);
        }
        g.nodes.push(ModelNode { params: Value::Object(params), ..node });
    }
    layout(&mut g);
    g
}

/// Converts steps within the graph's supported limits, returning an error instead of truncating.
/// Referenced steps must be earlier and have a layer output.
pub fn checked_steps_to_graph(steps: &[ModelStep]) -> Result<ModelGraph, String> {
    let limit = MAX_NODES / 2;
    if steps.len() > limit {
        return Err(format!(
            "this model has {} steps; graph conversion supports at most {limit}. The original steps have not been changed.",
            steps.len()
        ));
    }
    let mut inputs = HashSet::new();
    let mut edges = 0;
    for (index, step) in steps.iter().enumerate() {
        if !step.params.is_null() && !step.params.is_object() {
            return Err(format!("model step {} needs an object for its parameters", index + 1));
        }
        let node = ModelNode { tool: Some(step.tool.clone()), kind: ModelNodeKind::Tool, ..Default::default() };
        for port in input_ports(&node) {
            let Some(value) = step.params.get(port.id).and_then(Value::as_str) else { continue };
            for part in value.split(|c| port.many && c == ';').map(str::trim) {
                if port.many && part.is_empty() {
                    continue;
                }
                match part.strip_prefix('%').and_then(|n| n.parse::<usize>().ok()) {
                    Some(n) if !(1..=index).contains(&n) => {
                        return Err(format!(
                            "model step {} uses %{n}, which is not an earlier step. The original steps have not been changed.",
                            index + 1
                        ));
                    }
                    Some(n) => {
                        let has_no_output = steps.get(n.saturating_sub(1)).is_some_and(|previous| {
                            let node = ModelNode { tool: Some(previous.tool.clone()), kind: ModelNodeKind::Tool, ..Default::default() };
                            output_ports(&node).is_empty()
                        });
                        if has_no_output {
                            return Err(format!(
                                "model step {} uses %{n}, but step {n} has no layer output. The original steps have not been changed.",
                                index + 1
                            ));
                        }
                    }
                    None => {
                        inputs.insert(part.to_ascii_lowercase());
                    }
                }
                edges += 1;
                if steps.len() + inputs.len() > MAX_NODES || edges > MAX_EDGES {
                    return Err(format!(
                        "this model exceeds the graph limits of {MAX_NODES} nodes and {MAX_EDGES} connections. The original steps have not been changed."
                    ));
                }
            }
        }
    }
    Ok(steps_to_graph(steps))
}

/// The graph's tools as steps in run order, edges as layer names (from inputs) and `%n` (from
/// step n); empty when the graph has a loop.
pub fn graph_to_steps(g: &ModelGraph) -> Vec<ModelStep> {
    let Some(order) = topological_order(g) else { return Vec::new() };
    let mut step_of: HashMap<&str, usize> = HashMap::new();
    let mut steps = Vec::new();
    for i in order {
        let Some(n) = g.nodes.get(i).filter(|n| n.kind == ModelNodeKind::Tool) else { continue };
        let mut params = match &n.params {
            Value::Object(o) => o.clone(),
            _ => serde_json::Map::new(),
        };
        for port in input_ports(n) {
            let from: Vec<String> = g
                .edges
                .iter()
                .filter(|e| e.to == n.id && e.to_port == port.id)
                .filter_map(|e| {
                    let up = g.nodes.iter().find(|u| u.id == e.from)?;
                    match up.kind {
                        ModelNodeKind::Input => up.layer.clone(),
                        _ => step_of.get(up.id.as_str()).map(|k| format!("%{k}")),
                    }
                })
                .collect();
            if !from.is_empty() {
                params.insert(port.id.into(), json!(from.join(";")));
            }
        }
        steps.push(ModelStep { tool: n.tool.clone().unwrap_or_default(), params: Value::Object(params) });
        step_of.insert(n.id.as_str(), steps.len());
    }
    steps
}

/// Reads a graph from JSON, dropping what can't be used: nodes without an id, repeated ids,
/// edges to missing nodes or to themselves. Bounded in size.
/// Repeated connections into many-valued input ports retain their multiplicity.
pub fn graph_from_json(v: &Value) -> Result<ModelGraph, String> {
    graph_from_json_counted(v).map(|(g, _)| g)
}

/// What [`graph_from_json_counted`] left out of a graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphDropped {
    /// Nodes that could not be read, had no id, or repeated another's id.
    pub nodes: usize,
    /// Connections that could not be read, joined a missing node or itself, had no input port,
    /// or repeated another.
    pub edges: usize,
    /// Kept nodes whose id was trimmed of spaces or cut to its longest length.
    pub ids: usize,
    /// Kept nodes whose position was not a number (moved to 0, 0) or out of range (clamped).
    pub positions: usize,
    /// Kept nodes whose parameters were not an object, so they were left out.
    pub params: usize,
}

/// Reads a graph as [`graph_from_json`] does, and counts the nodes and connections it left out
/// and the nodes it changed, so a command can say so. Errors when `nodes` is missing or either list is too long.
pub fn graph_from_json_counted(v: &Value) -> Result<(ModelGraph, GraphDropped), String> {
    let nodes = v.get("nodes").and_then(Value::as_array).ok_or("a model graph needs \"nodes\"")?;
    if nodes.len() > MAX_NODES {
        return Err(format!("a model can have at most {MAX_NODES} nodes"));
    }
    let edges = v.get("edges").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    if edges.len() > MAX_EDGES {
        return Err(format!("a model can have at most {MAX_EDGES} connections"));
    }
    let mut g = ModelGraph::default();
    let mut dropped = GraphDropped::default();
    // Ids trimmed or cut, from what the file gave to what is kept: connections naming a node by
    // the id it was given still reach it.
    let mut renamed: Vec<(String, String)> = Vec::new();
    // The ids given to the nodes kept, and to those left out for repeating a kept one's id once
    // trimmed or cut: a connection naming only a left-out node is left out with it, rather than
    // moved onto the node that kept the id.
    let mut given_kept: Vec<String> = Vec::new();
    let mut given_dropped: Vec<String> = Vec::new();
    for n in nodes {
        let Ok(mut node) = serde_json::from_value::<ModelNode>(n.clone()) else {
            dropped.nodes += 1;
            continue;
        };
        let id: String = node.id.trim().chars().take(MAX_NODE_ID).collect();
        if id.is_empty() || g.nodes.iter().any(|m| m.id == id) {
            dropped.nodes += 1;
            if !id.is_empty() {
                given_dropped.push(node.id);
            }
            continue;
        }
        given_kept.push(node.id.clone());
        if id != node.id {
            dropped.ids += 1;
            renamed.push((std::mem::take(&mut node.id), id.clone()));
            node.id = id;
        }
        let (x, y) = (node.x, node.y);
        if !node.x.is_finite() || !node.y.is_finite() {
            node.x = 0.0;
            node.y = 0.0;
        }
        node.x = node.x.clamp(-MAX_NODE_POSITION, MAX_NODE_POSITION);
        node.y = node.y.clamp(-MAX_NODE_POSITION, MAX_NODE_POSITION);
        if node.x.to_bits() != x.to_bits() || node.y.to_bits() != y.to_bits() {
            dropped.positions += 1;
        }
        if !node.params.is_object() && !node.params.is_null() {
            dropped.params += 1;
            node.params = Value::Null;
        }
        g.nodes.push(node);
    }
    for e in edges {
        let Ok(mut e) = serde_json::from_value::<ModelEdge>(e.clone()) else {
            dropped.edges += 1;
            continue;
        };
        let left_out = |end: &String| given_dropped.contains(end) && !given_kept.contains(end);
        if left_out(&e.from) || left_out(&e.to) {
            dropped.edges += 1;
            continue;
        }
        for end in [&mut e.from, &mut e.to] {
            if let Some((_, id)) = renamed.iter().find(|(given, _)| given == end) {
                *end = id.clone();
            }
        }
        let has = |id: &str| g.nodes.iter().any(|n| n.id == id);
        let repeated_input =
            g.nodes.iter().find(|n| n.id == e.to).is_some_and(|n| input_ports(n).iter().any(|p| p.id == e.to_port && p.many));
        if e.from != e.to && has(&e.from) && has(&e.to) && !e.to_port.is_empty() && (repeated_input || !g.edges.contains(&e)) {
            g.edges.push(e);
        } else {
            dropped.edges += 1;
        }
    }
    // A graph with no positions (written by hand or by an agent) is laid out.
    if g.nodes.iter().all(|n| n.x == 0.0 && n.y == 0.0) {
        layout(&mut g);
    }
    Ok((g, dropped))
}

/// How one node fared in a run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeRun {
    pub node: String,
    /// `done`, `pending` (a browser file awaiting delivery), or `error`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// The layer the node handed on, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
}

/// What the last run of a model did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RunReport {
    pub model: String,
    pub ok: bool,
    pub nodes: Vec<NodeRun>,
    /// The layers the model kept.
    pub outputs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Correlates browser acknowledgements with this run only, never a newer report.
    #[serde(skip)]
    pending_files: HashMap<u64, String>,
}

impl RunReport {
    pub(crate) fn file_delivered(&mut self, id: u64, error: Option<&str>) {
        let Some(node_id) = self.pending_files.remove(&id) else { return };
        let Some(node) = self.nodes.iter_mut().find(|n| n.node == node_id && n.status == "pending") else { return };
        node.status = if error.is_some() { "error" } else { "done" };
        if let Some(error) = error {
            node.message = error.to_string();
            if self.error.is_none() {
                self.error = Some(error.to_string());
            }
        }
        self.ok = self.error.is_none() && self.nodes.iter().all(|n| n.status == "done");
    }
}

/// Runs a graph: its nodes in order, each tool on the layers wired into it, each output renaming
/// its result; then removes the intermediate results. `trusted` is false for a model that came
/// in a file, whose tools may not read or write files. The report is kept in
/// `Session::model_run`, whatever happens. A failed run discards its queued exports, so their
/// nodes become errors rather than waiting for delivery.
pub(crate) fn run(s: &mut Session, op: crate::policy::Authorized<crate::policy::ModelOp>) -> Result<Value, String> {
    let crate::policy::ModelOp { name, graph: g } = op.op();
    let mut report = RunReport { model: name.to_string(), ..Default::default() };
    let res = run_into(s, g, &mut report);
    if res.is_err() {
        for node in report.nodes.iter_mut().filter(|node| node.status == "pending") {
            node.status = "error";
            node.message = "Queued export discarded because the model run failed.".to_string();
        }
        report.pending_files.clear();
    }
    report.ok = res.is_ok() && !report.nodes.iter().any(|n| n.status == "pending");
    report.error = res.as_ref().err().cloned();
    let out = res.map(|ran| json!({"model": name, "ran": ran, "outputs": report.outputs}));
    s.model_run = Some(report);
    out
}

/// Validate the graph and reject an imported graph's external tools before executing any node.
/// Trusted graphs retain the normal tool execution and output wiring.
fn run_into(s: &mut Session, g: &ModelGraph, report: &mut RunReport) -> Result<Vec<Value>, String> {
    if let Some(p) = validate(s, g).first() {
        return Err(p.message.clone());
    }
    let order = topological_order(g).ok_or("The model contains a loop.")?;
    // The layer each node hands on, and the layers each tool node added (by id).
    let mut value: HashMap<&str, String> = HashMap::new();
    let mut made: Vec<(&str, Vec<u32>)> = Vec::new();
    // The layer (by id) each tool node handed on, for its output to rename.
    let mut handed: HashMap<&str, u32> = HashMap::new();
    let mut ran = Vec::new();
    let mut file_pending = false;
    let mut tool_completed = false;
    for i in order {
        let Some(n) = g.nodes.get(i) else { continue };
        let upstream = |port: &str| -> Vec<String> {
            g.edges.iter().filter(|e| e.to == n.id && e.to_port == port).filter_map(|e| value.get(e.from.as_str()).cloned()).collect()
        };
        match n.kind {
            ModelNodeKind::Input => {
                let l = n.layer.clone().unwrap_or_default();
                report.nodes.push(NodeRun { node: n.id.clone(), status: "done", message: String::new(), layer: Some(l.clone()) });
                value.insert(n.id.as_str(), l);
            }
            ModelNodeKind::Tool => {
                let tool = tool_of(n).map(|t| t.id).unwrap_or_default();
                let mut params = match &n.params {
                    Value::Object(o) => o.clone(),
                    _ => serde_json::Map::new(),
                };
                for port in input_ports(n) {
                    let from = upstream(port.id);
                    let wired = g.edges.iter().any(|e| e.to == n.id && e.to_port == port.id);
                    if wired && from.is_empty() {
                        let e = format!("{}: the tool before it made no layer for \"{}\"", title(n), port.label);
                        report.nodes.push(NodeRun { node: n.id.clone(), status: "error", message: e.clone(), layer: None });
                        return Err(e);
                    }
                    if !from.is_empty() {
                        params.insert(port.id.into(), json!(from.join(";")));
                    }
                }
                let params = Value::Object(params);
                let before: HashSet<u32> = s.map().layers.iter().map(|l| l.id).collect();
                let history = s.project.gp_history.len();
                match crate::cmd::gp::run_tool(s, tool, &params) {
                    Ok(r) => {
                        let ids: Vec<u32> = s.map().layers.iter().filter(|l| !before.contains(&l.id)).map(|l| l.id).collect();
                        // The first layer it added (else the first output it recorded: a table).
                        let first_id = ids.iter().rev().copied().find(|id| s.map().layer(*id).is_some());
                        if let Some(id) = first_id {
                            handed.insert(n.id.as_str(), id);
                        }
                        let first = first_id.and_then(|id| s.map().layer(id).map(|l| l.name.clone())).or_else(|| {
                            (s.project.gp_history.len() > history)
                                .then(|| s.project.gp_history.last().and_then(|h| h.outputs.first().cloned()))
                                .flatten()
                        });
                        let pending = r.get("pending").and_then(Value::as_bool) == Some(true);
                        file_pending |= pending;
                        tool_completed |= !pending;
                        let status = if pending { "pending" } else { "done" };
                        report.nodes.push(NodeRun { node: n.id.clone(), status, message: String::new(), layer: first.clone() });
                        if pending && let Some(id) = r.get("download").and_then(Value::as_u64) {
                            report.pending_files.insert(id, n.id.clone());
                        }
                        if let Some(f) = first {
                            value.insert(n.id.as_str(), f);
                        }
                        made.push((n.id.as_str(), ids));
                        ran.push(json!({"step": ran.len() + 1, "node": n.id, "tool": tool, "result": r}));
                    }
                    Err(e) => {
                        let e = format!("{}: {e}", title(n));
                        report.nodes.push(NodeRun { node: n.id.clone(), status: "error", message: e.clone(), layer: None });
                        return Err(e);
                    }
                }
            }
            ModelNodeKind::Output => {
                let Some(layer) = upstream(IN).into_iter().next() else {
                    let e = format!("{}: the tool before it made no layer", title(n));
                    report.nodes.push(NodeRun { node: n.id.clone(), status: "error", message: e.clone(), layer: None });
                    return Err(e);
                };
                let wanted = n.name.as_deref().map(str::trim).filter(|x| !x.is_empty()).unwrap_or("Model output");
                let wanted: String = wanted.chars().take(200).collect();
                // A tool's own layer is renamed; an input passed straight through (or a tool's
                // table) stays as it is.
                let source = g.edges.iter().find(|e| e.to == n.id && e.to_port == IN).and_then(|e| handed.get(e.from.as_str()).copied());
                let kept = match source {
                    Some(id) => rename(s, id, &wanted).unwrap_or(layer),
                    None => layer,
                };
                // Tools after the renamed one read it by its new name.
                if let Some(e) = g.edges.iter().find(|e| e.to == n.id && e.to_port == IN) {
                    value.insert(e.from.as_str(), kept.clone());
                }
                report.nodes.push(NodeRun { node: n.id.clone(), status: "done", message: String::new(), layer: Some(kept.clone()) });
                report.outputs.push(kept.clone());
                value.insert(n.id.as_str(), kept);
            }
        }
    }
    // Intermediate results: the layers of tools whose output feeds only other tools.
    for (id, layers) in &made {
        let mut to = g.edges.iter().filter(|e| e.from == *id).filter_map(|e| g.nodes.iter().find(|n| n.id == e.to));
        let feeds_tools_only = to.clone().next().is_some() && to.all(|n| n.kind == ModelNodeKind::Tool);
        if feeds_tools_only {
            crate::cmd::layer::drop_layers(s, layers);
            for l in layers {
                s.map_mut().set_group(*l, None);
            }
        }
    }
    // Tools at the end of the flow with no output node keep their results too.
    for (id, layers) in &made {
        if !g.edges.iter().any(|e| e.from == *id) {
            report.outputs.extend(layers.iter().filter_map(|l| s.map().layer(*l).map(|l| l.name.clone())));
        }
    }
    if s.active_layer.is_some_and(|a| s.map().layer(a).is_none()) {
        s.active_layer = s.map().layers.first().map(|l| l.id);
    }
    // Queued bytes alone are request-only; completed tools still need the model's undo step.
    if tool_completed && file_pending {
        s.request_only = false;
    }
    Ok(ran)
}

/// Renames layer `id` to `wanted` (made unique among the map's other layers) and returns the
/// name it got; `None` when the map no longer has it.
fn rename(s: &mut Session, id: u32, wanted: &str) -> Option<String> {
    let m = s.map_mut();
    m.layer(id)?;
    let taken = |n: &str| m.layers.iter().any(|l| l.id != id && l.name.eq_ignore_ascii_case(n));
    let mut name = wanted.to_string();
    let mut k = 1;
    while taken(&name) && k < 10_000 {
        k += 1;
        name = format!("{wanted} {k}");
    }
    if let Some(i) = m.layer_index(id)
        && let Some(l) = m.layers.get_mut(i)
    {
        l.name = name.clone();
        l.dataset.name = name.clone();
    }
    Some(name)
}

/// The JSON a model file holds (`*.model.json`).
pub const SCHEMA: &str = "https://rustgis.app/schemas/model-graph-v1.json";

/// The model-file version understood by this reader and emitted by this writer.
pub(crate) const FILE_VERSION: &str = "1.0.0";
/// Web tools store steps under a separate, integer version marker.
pub(crate) const WEB_TOOL_VERSION: u64 = 1;

/// Keep unknown fields visible at the file boundary without changing the command data types.
#[derive(Deserialize)]
struct ImportItem<T> {
    #[serde(flatten)]
    known: T,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

/// Check declared versions and model contents before import can discard data.
/// Missing versions are legacy files. The stricter field checks apply only to file import.
pub(crate) fn check_file(v: &Value) -> Result<(), String> {
    let graph = v.get("graph");
    if let Some(version) = v.get("version") {
        let supported = if graph.is_some() { version.as_str() == Some(FILE_VERSION) } else { version.as_u64() == Some(WEB_TOOL_VERSION) };
        if !supported {
            return Err("this model or web tool has an unsupported version. Update RustGIS to import it.".into());
        }
    }
    if graph.is_some() && v.get("steps").is_some() {
        return Err("a model file cannot contain both a graph and steps. Choose one representation before importing it.".into());
    }
    let fields: &[&str] =
        if graph.is_some() { &["$schema", "version", "name", "graph"] } else { &["format", "version", "name", "steps", "parameters"] };
    if v.as_object().is_some_and(|file| file.keys().any(|key| !fields.contains(&key.as_str()))) {
        return Err("model file has unsupported fields. Update RustGIS to import this model.".into());
    }
    if let Some(graph) = graph {
        let nodes = graph.get("nodes").and_then(Value::as_array).ok_or("a model graph needs \"nodes\"")?;
        if nodes.len() > MAX_NODES {
            return Err(format!("a model can have at most {MAX_NODES} nodes"));
        }
        if graph.as_object().is_some_and(|g| g.keys().any(|k| !matches!(k.as_str(), "nodes" | "edges"))) {
            return Err("model graph has unsupported fields. Update RustGIS to import this model.".into());
        }
        for (index, node) in nodes.iter().enumerate() {
            let node = ImportItem::<ModelNode>::deserialize(node)
                .map_err(|_| format!("model node {} is unsupported or malformed. Update RustGIS to import this model.", index + 1))?;
            if !node.extra.is_empty() {
                return Err(format!("model node {} has unsupported fields. Update RustGIS to import this model.", index + 1));
            }
            if node.known.id.trim() != node.known.id || node.known.id.chars().nth(MAX_NODE_ID).is_some() {
                return Err(format!(
                    "model node {} has an ID that would be trimmed or truncated. Use at most {MAX_NODE_ID} characters without surrounding whitespace.",
                    index + 1
                ));
            }
            if !(-MAX_NODE_POSITION..=MAX_NODE_POSITION).contains(&node.known.x)
                || !(-MAX_NODE_POSITION..=MAX_NODE_POSITION).contains(&node.known.y)
            {
                return Err(format!(
                    "model node {} has a position outside the supported canvas bounds (-{MAX_NODE_POSITION} to {MAX_NODE_POSITION})",
                    index + 1
                ));
            }
            if !node.known.params.is_null() && !node.known.params.is_object() {
                return Err(format!("model node {} needs an object for its parameters", index + 1));
            }
        }
        let edges = match graph.get("edges") {
            None => &[][..],
            Some(edges) => edges.as_array().map(Vec::as_slice).ok_or("a model graph's connections must be an array")?,
        };
        if edges.len() > MAX_EDGES {
            return Err(format!("a model can have at most {MAX_EDGES} connections"));
        }
        for (index, edge) in edges.iter().enumerate() {
            let edge = ImportItem::<ModelEdge>::deserialize(edge).map_err(|_| format!("model connection {} is malformed", index + 1))?;
            if !edge.extra.is_empty() {
                return Err(format!("model connection {} has unsupported fields. Update RustGIS to import this model.", index + 1));
            }
        }
        let normalized = graph_from_json(graph)?;
        let lost_nodes = nodes.len().saturating_sub(normalized.nodes.len());
        let lost_edges = edges.len().saturating_sub(normalized.edges.len());
        if lost_nodes > 0 || lost_edges > 0 {
            return Err(format!(
                "this model would discard {lost_nodes} node(s) and {lost_edges} connection(s). Fix empty or duplicate node IDs and invalid connections before importing it."
            ));
        }
        let by_id: HashMap<&str, &ModelNode> = normalized.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let mut single_inputs = HashSet::new();
        for (index, edge) in normalized.edges.iter().enumerate() {
            let from_known = by_id.get(edge.from.as_str()).is_some_and(|n| output_ports(n).iter().any(|p| p.id == edge.from_port));
            let to_port = by_id.get(edge.to.as_str()).and_then(|n| input_ports(n).into_iter().find(|p| p.id == edge.to_port));
            if !from_known || to_port.is_none() {
                return Err(format!(
                    "model connection {} refers to an unsupported port. Update RustGIS or fix the connection before importing this model.",
                    index + 1
                ));
            }
            if let Some(input) = by_id.get(edge.from.as_str()).filter(|n| n.kind == ModelNodeKind::Input) {
                let layer = input.layer.as_deref().filter(|layer| !layer.trim().is_empty()).ok_or_else(|| {
                    format!(
                        "model connection {} has an input without a layer. Choose a layer for this input before importing this model.",
                        index + 1
                    )
                })?;
                if by_id.get(edge.to.as_str()).is_some_and(|n| n.kind == ModelNodeKind::Tool)
                    && (layer.trim() != layer
                        || to_port.as_ref().is_some_and(|port| port.many && layer.contains(';'))
                        || layer.strip_prefix('%').and_then(|n| n.parse::<usize>().ok()).is_some())
                {
                    return Err(format!(
                        "model connection {} has an input layer name that cannot be represented in steps. Rename the layer to avoid surrounding whitespace, numeric %n references, or semicolons on multi-input ports before importing this model.",
                        index + 1
                    ));
                }
            }
            if to_port.is_some_and(|p| !p.many) && !single_inputs.insert((edge.to.as_str(), edge.to_port.as_str())) {
                return Err(format!(
                    "model connection {} duplicates a single-valued input port. Keep one connection to this port before importing this model.",
                    index + 1
                ));
            }
        }
        if topological_order(&normalized).is_none() {
            return Err("this model contains a loop and cannot be converted to steps. Fix the loop before importing it.".into());
        }
    } else if let Some(steps) = v.get("steps") {
        let steps = steps.as_array().ok_or("model steps must be an array")?;
        if steps.len() > MAX_NODES {
            return Err(format!("a model can have at most {MAX_NODES} steps"));
        }
        for (index, step) in steps.iter().enumerate() {
            let step = ImportItem::<ModelStep>::deserialize(step)
                .map_err(|_| format!("model step {} is unsupported or malformed. Update RustGIS to import this model.", index + 1))?;
            if !step.extra.is_empty() {
                return Err(format!("model step {} has unsupported fields. Update RustGIS to import this model.", index + 1));
            }
            if !step.known.params.is_null() && !step.known.params.is_object() {
                return Err(format!("model step {} needs an object for its parameters", index + 1));
            }
        }
    }
    Ok(())
}

/// A graph's current file envelope; imports also accept legacy envelopes without a version.
pub fn to_file(name: &str, g: &ModelGraph) -> Value {
    json!({"$schema": SCHEMA, "version": FILE_VERSION, "name": name, "graph": g})
}
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::Project;

    fn session() -> Session {
        let mut s = Session::new(Project::default());
        let mut points = rustgis_data::Dataset::new("Points", rustgis_data::GeometryType::Point, Vec::new());
        points.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(20.0, 30.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(0, points));
        s.queue_gp_files = true;
        s.mark_saved();
        s
    }

    fn file_graph() -> Value {
        json!({"nodes": [
            {"id": "file", "kind": "tool", "tool": "vectorToPmtiles",
             "params": {"input": "Points", "minZoom": 0, "maxZoom": 0}}
        ], "edges": []})
    }

    fn run_files(s: &mut Session, graph: Value) -> Vec<u64> {
        let result = s.run("model.run", &json!({"graph": graph})).unwrap();
        assert!(result["ran"].as_array().unwrap().iter().all(|r| r["result"]["pending"] == true));
        s.take_gp_files()
            .into_iter()
            .map(|(id, _, bytes)| {
                assert!(bytes.starts_with(b"PMTiles\x03"));
                id
            })
            .collect()
    }

    #[test]
    fn queued_file_does_not_hide_undo_for_prior_model_tools() {
        // Other native connection tests rotate process-wide accounts. This local-byte/undo
        // scenario must not acquire their account changes between producing and delivering bytes.
        #[cfg(not(target_arch = "wasm32"))]
        if std::env::var_os("RUSTGIS_MODEL_FILE_CHILD").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "model::tests::queued_file_does_not_hide_undo_for_prior_model_tools", "--nocapture"])
                .env("RUSTGIS_MODEL_FILE_CHILD", "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));
            return;
        }
        let mut s = session();
        let depth = s.undo_depth();
        let graph = json!({"nodes": [
            {"id": "points", "kind": "input", "layer": "Points"},
            {"id": "buffer", "kind": "tool", "tool": "buffer", "params": {"distance": 100}},
            {"id": "output", "kind": "output", "name": "Buffered"},
            {"id": "file", "kind": "tool", "tool": "vectorToPmtiles", "params": {"minZoom": 0, "maxZoom": 0}}
        ], "edges": [
            {"from": "points", "fromPort": "out", "to": "buffer", "toPort": "input"},
            {"from": "buffer", "fromPort": "out", "to": "output", "toPort": "in"},
            {"from": "buffer", "fromPort": "out", "to": "file", "toPort": "input"}
        ]});
        s.run("model.run", &json!({"graph": graph})).unwrap();
        assert!(s.map().layer_by_name("Buffered").is_some());
        assert_eq!(s.project.gp_history.len(), 1, "only the buffer has completed");
        assert!(!s.model_run.as_ref().unwrap().ok);
        assert!(s.model_run.as_ref().unwrap().nodes.iter().any(|n| n.node == "file" && n.status == "pending"));
        let files = s.take_gp_files();
        assert_eq!(files.len(), 1, "{:?}", s.notices);
        assert!(files[0].2.starts_with(b"PMTiles\x03"));
        assert!(s.dirty());
        assert_eq!(s.undo_depth(), depth + 1);
        assert!(s.undo());
        assert!(s.map().layer_by_name("Buffered").is_none());
        assert_eq!(s.map().layers.len(), 1);
        assert!(s.project.gp_history.is_empty());
        assert!(!s.dirty(), "undo restores the pre-model saved state");
    }

    #[test]
    fn file_only_model_still_skips_an_empty_undo_step_and_completes_on_delivery() {
        let mut s = session();
        let depth = s.undo_depth();
        let ids = run_files(&mut s, file_graph());
        assert_eq!(ids.len(), 1);
        assert!(!s.dirty());
        assert_eq!(s.undo_depth(), depth);
        assert!(!s.model_run.as_ref().unwrap().ok);
        s.run("gp.fileDelivered", &json!({"id": ids[0]})).unwrap();
        let report = s.model_run.as_ref().unwrap();
        assert!(report.ok);
        assert_eq!(report.nodes[0].status, "done");
        assert_eq!(report.error, None);
        assert!(s.project.gp_history[0].ok);
    }

    #[test]
    fn later_tool_failure_discards_queued_exports_and_keeps_a_terminal_failed_report() {
        let mut s = session();
        let previous_id = s.next_pending;
        let graph = json!({"nodes": [
            {"id": "file", "kind": "tool", "tool": "vectorToPmtiles", "x": 0,
             "params": {"input": "Points", "minZoom": 0, "maxZoom": 0}},
            {"id": "failure", "kind": "tool", "tool": "buffer", "x": 100,
             "params": {"input": "Missing", "distance": 100}}
        ], "edges": []});
        let error = s.run("model.run", &json!({"graph": graph})).unwrap_err();
        let discarded_id = s.next_pending;
        assert_eq!(discarded_id, previous_id.wrapping_add(1), "the export queued before the later tool failed");
        assert!(s.take_gp_files().is_empty());
        assert!(s.file_outputs.is_empty(), "discarded exports cannot await delivery");
        let report = s.model_run.as_ref().unwrap();
        assert!(!report.ok);
        assert_eq!(report.error.as_deref(), Some(error.as_str()));
        assert_eq!(report.nodes.len(), 2);
        assert!(report.nodes.iter().all(|node| node.status == "error"));
        assert!(
            report
                .nodes
                .iter()
                .any(|node| { node.node == "file" && node.message == "Queued export discarded because the model run failed." })
        );
        assert!(report.nodes.iter().any(|node| node.node == "failure" && node.message == error));
        assert!(report.pending_files.is_empty());
        let before = report.clone();
        assert_eq!(s.run("gp.fileDelivered", &json!({"id": discarded_id})).unwrap_err(), "no pending file with that id");
        assert_eq!(s.model_run.as_ref(), Some(&before), "the original failed report survives a stale acknowledgement");
    }

    #[test]
    fn model_file_failure_updates_the_matching_node_and_survives_other_deliveries() {
        let mut s = session();
        let graph = json!({"nodes": [
            {"id": "first", "kind": "tool", "tool": "vectorToPmtiles",
             "params": {"input": "Points", "minZoom": 0, "maxZoom": 0, "output": "first.pmtiles"}},
            {"id": "second", "kind": "tool", "tool": "vectorToPmtiles",
             "params": {"input": "Points", "minZoom": 0, "maxZoom": 0, "output": "second.pmtiles"}}
        ], "edges": []});
        let ids = run_files(&mut s, graph);
        assert_eq!(ids.len(), 2);
        s.run("gp.fileDelivered", &json!({"id": ids[1], "error": "download denied"})).unwrap();
        let report = s.model_run.as_ref().unwrap();
        assert!(!report.ok);
        assert_eq!(report.error.as_deref(), Some("download denied"));
        assert!(report.nodes.iter().any(|n| n.node == "second" && n.status == "error" && n.message == "download denied"));
        assert!(report.nodes.iter().any(|n| n.node == "first" && n.status == "pending"));
        s.run("gp.fileDelivered", &json!({"id": ids[0]})).unwrap();
        let report = s.model_run.as_ref().unwrap();
        assert!(!report.ok);
        assert_eq!(report.error.as_deref(), Some("download denied"));
        assert!(report.nodes.iter().any(|n| n.node == "first" && n.status == "done"));
        assert!(report.nodes.iter().all(|n| n.status != "pending"));
    }

    /// Terminal authority refusal updates the matching model node without claiming delivery.
    #[test]
    fn retired_model_file_does_not_remain_pending() {
        let mut s = session();
        let login = s
            .run("connection.new", &json!({"kind":"wms","address":"https://model-retire.invalid/wms","auth":"header","keyName":"X-Key"}))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        s.run("connection.login", &json!({"id":login,"apiKey":"RGSYNTH_model_retire"})).unwrap();
        s.run("model.run", &json!({"graph":file_graph()})).unwrap();
        let id = s.file_outputs[0].id;
        assert_eq!(s.model_run.as_ref().unwrap().nodes[0].status, "pending");
        s.run("connection.logout", &json!({"id":login})).unwrap();
        assert!(s.take_gp_files().is_empty());
        let report = s.model_run.as_ref().unwrap();
        assert_eq!(report.nodes[0].status, "error", "a retired export cannot wait for an impossible acknowledgement");
        assert!(!report.ok && report.error.is_some());
        assert!(report.pending_files.is_empty());
        assert!(s.project.gp_history.is_empty(), "refused bytes cannot record successful delivery");
        let before = report.clone();
        assert!(s.acknowledge_gp_file(id, None).is_err());
        assert_eq!(s.model_run.as_ref(), Some(&before));
    }

    #[test]
    fn model_waits_for_all_downloads_and_ignores_older_run_acknowledgements() {
        let mut s = session();
        let old = run_files(&mut s, file_graph())[0];
        let mut graph = file_graph();
        let mut second = graph["nodes"][0].clone();
        second["id"] = json!("second");
        graph["nodes"].as_array_mut().unwrap().push(second);
        let ids = run_files(&mut s, graph);
        let before = s.model_run.clone();
        s.run("gp.fileDelivered", &json!({"id": old, "error": "old run failed"})).unwrap();
        assert_eq!(s.model_run, before, "matching node names do not identify a model run");
        s.run("gp.fileDelivered", &json!({"id": ids[1]})).unwrap();
        assert!(!s.model_run.as_ref().unwrap().ok, "the other download is still pending");
        assert_eq!(s.model_run.as_ref().unwrap().nodes.iter().filter(|n| n.status == "pending").count(), 1);
        s.run("gp.fileDelivered", &json!({"id": ids[0]})).unwrap();
        let report = s.model_run.as_ref().unwrap();
        assert!(report.ok);
        assert!(report.nodes.iter().all(|n| n.status == "done"));
        assert_eq!(report.error, None);
    }
}
