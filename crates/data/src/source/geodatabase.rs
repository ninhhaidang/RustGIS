//! The catalog of an Esri enterprise geodatabase, as its own system tables describe it. The
//! geodatabase keeps one `GDB_ITEMS` row per feature dataset, feature class and table (with an
//! XML `Definition` holding the alias, shape column, field aliases and versioning), and
//! `SDE_table_registry`, `SDE_layers` and `SDE_spatial_references` say which column is the row id,
//! where the shape is and in what coordinate system. This module builds the model of that catalog
//! from those tables' rows as plain strings, so the parsing is tested without a server and shared
//! by every platform; the queries that fetch the rows live with each connection.
//!
//! The rows and their XML are hostile input: lists are bounded by [`MAX_ITEMS`], a definition by
//! [`MAX_DEFINITION_BYTES`], and one that is missing, too large or isn't XML leaves its item
//! listed with the definition's details unknown (and [`read_plan`] then refuses to guess how it
//! is versioned).

use std::collections::HashMap;

use super::Family;

/// Most items (and registry, layer and coordinate system rows) a catalog keeps.
pub const MAX_ITEMS: usize = 10_000;

/// Largest `Definition` XML read; a larger one is skipped.
pub const MAX_DEFINITION_BYTES: usize = 1 << 20;

/// Most field aliases kept for one item.
const MAX_FIELDS: usize = 4096;

/// The columns a branch-versioned table holds that reading its Default rows needs: each edit
/// inserts a row on its branch (`GDB_BRANCH_ID`, 0 for Default) stamped `GDB_FROM_DATE`, a delete
/// being a row with `GDB_IS_DELETE` set, so the Default rows are the latest row per ObjectID on
/// branch 0 that isn't a delete. (Written from the documented columns; not yet run against a real
/// branch-versioned class.)
const BRANCH_COLUMNS: [&str; 3] = ["GDB_BRANCH_ID", "GDB_IS_DELETE", "GDB_FROM_DATE"];

/// What an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemKind {
    FeatureDataset,
    FeatureClass,
    Table,
}

/// How an item's edits are kept apart from its Default rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Versioning {
    #[serde(rename = "none")]
    Unversioned,
    Traditional,
    Branch,
}

/// A feature dataset, feature class or table of the geodatabase.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// `GDB_ITEMS.Name` as given (`utility.sde.Mains` or `sde.Mains`).
    pub name: String,
    /// The owner, from `PhysicalName`: its second-to-last part (`SDE.MAINS` and the database
    /// qualified `UTILITY.SDE.MAINS` both give `SDE`); empty when it has only one part.
    pub schema: String,
    /// The table, from `PhysicalName`: its last part (`MAINS`).
    pub table: String,
    pub kind: ItemKind,
    /// The feature dataset a class is in, from `Path` (`\Water\Mains` gives `Water`); `None` at
    /// the root.
    pub dataset: Option<String>,
    /// The definition's `AliasName`, when it differs from the short name.
    pub alias: Option<String>,
    pub shape_column: Option<String>,
    /// `(field name, alias)` for the fields whose alias differs from their name.
    pub field_aliases: Vec<(String, String)>,
    /// `Traditional` when the definition says `Versioned` is true; `Branch` when its
    /// `VersioningType` names branch (or is `2`); else `Unversioned` (which means unknown when
    /// `definition_ok` is false).
    pub versioning: Versioning,
    /// The definition was there, within [`MAX_DEFINITION_BYTES`] and parsed; when false, how the
    /// item is versioned is unknown.
    pub definition_ok: bool,
    /// The kind of geometry the definition's `ShapeType` gives a feature class.
    pub family: Option<Family>,
    /// The definition says `IsArchived` is true.
    pub archived: bool,
    /// `SDE_table_registry.rowid_column`.
    pub rowid_column: Option<String>,
    /// `SDE_layers.srid`.
    pub srid: Option<i64>,
}

impl Item {
    /// The name without its owner and database (`Mains` for `utility.sde.Mains`).
    pub fn short_name(&self) -> &str {
        short(&self.name)
    }
}

/// A spatial reference of `SDE_spatial_references`.
#[derive(Clone, Debug, PartialEq)]
pub struct Srs {
    pub srid: i64,
    /// The authority `auth_srid` is a code of (`EPSG`, `ESRI`), when the row names one.
    pub auth_name: Option<String>,
    pub auth_srid: Option<i64>,
    pub srtext: Option<String>,
}

/// A `GDB_ITEMS` row (with its type's name), as a platform's query returns it. The definition is
/// parsed here, so tests need no server.
#[derive(Clone, Debug)]
pub struct ItemRow {
    pub name: String,
    pub physical: String,
    pub path: String,
    pub type_name: String,
    pub definition: Option<String>,
}

/// An `SDE_table_registry` row.
#[derive(Clone, Debug)]
pub struct RegistryRow {
    pub owner: String,
    pub table: String,
    pub rowid_column: Option<String>,
}

/// An `SDE_layers` row.
#[derive(Clone, Debug)]
pub struct LayerRow {
    pub owner: String,
    pub table: String,
    pub column: String,
    pub srid: Option<i64>,
}

/// The items of a geodatabase and its coordinate systems.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub owner: String,
    pub release: Option<String>,
    pub items: Vec<Item>,
    pub srs: Vec<Srs>,
    /// Whether the item rows reached [`MAX_ITEMS`], so there may be more ([`Catalog::cut_note`]).
    pub cut: bool,
}

/// What a `Definition` says, every part optional.
#[derive(Default)]
struct Definition {
    alias: Option<String>,
    shape_column: Option<String>,
    field_aliases: Vec<(String, String)>,
    versioning: Option<Versioning>,
    archived: bool,
    family: Option<Family>,
}

impl Catalog {
    /// What to say when the catalog was cut at [`MAX_ITEMS`]: the statement asks for no more
    /// rows than that, so a full answer means the geodatabase has more.
    pub fn cut_note(&self) -> Option<String> {
        self.cut.then(|| format!("the geodatabase lists {MAX_ITEMS} items or more: only the first {MAX_ITEMS} are shown"))
    }

    /// Builds the catalog from the rows of the system tables. A row of another type (a
    /// relationship class, a domain) is left out; registry and layer rows are matched to items by
    /// owner and table, case-insensitively.
    pub fn from_rows(
        owner: &str,
        release: Option<String>,
        items: Vec<ItemRow>,
        registry: Vec<RegistryRow>,
        layers: Vec<LayerRow>,
        srs: Vec<Srs>,
    ) -> Catalog {
        let registry: HashMap<(String, String), Option<String>> =
            registry.into_iter().take(MAX_ITEMS).map(|r| (key(&r.owner, &r.table), r.rowid_column.filter(|c| !c.is_empty()))).collect();
        let mut layer_of: HashMap<(String, String), (String, Option<i64>)> = HashMap::new();
        for l in layers.into_iter().take(MAX_ITEMS) {
            layer_of.entry(key(&l.owner, &l.table)).or_insert((l.column, l.srid));
        }
        let cut = items.len() >= MAX_ITEMS;
        let items = items
            .into_iter()
            .take(MAX_ITEMS)
            .filter_map(|row| {
                let kind = kind_of(&row.type_name)?;
                // A row whose name couldn't be read goes by its table; one with neither is left out.
                let name = if row.name.trim().is_empty() { row.physical.trim().to_string() } else { row.name };
                if name.is_empty() {
                    return None;
                }
                let (schema, table) = split_physical(&row.physical);
                let read = row.definition.as_deref().and_then(read_definition);
                let definition_ok = read.is_some();
                let def = read.unwrap_or_default();
                let layer = layer_of.get(&key(schema, table));
                let segments: Vec<&str> = row.path.split('\\').filter(|s| !s.is_empty()).collect();
                let dataset = match (kind, segments.len().checked_sub(2)) {
                    (ItemKind::FeatureDataset, _) | (_, None) => None,
                    (_, Some(i)) => segments.get(i).map(|s| s.to_string()),
                };
                Some(Item {
                    schema: schema.to_string(),
                    table: table.to_string(),
                    kind,
                    dataset,
                    alias: def.alias.filter(|a| a != short(&name)),
                    shape_column: def.shape_column.or_else(|| layer.map(|l| l.0.clone()).filter(|c| !c.is_empty())),
                    field_aliases: def.field_aliases,
                    versioning: def.versioning.unwrap_or(Versioning::Unversioned),
                    definition_ok,
                    family: def.family,
                    archived: def.archived,
                    rowid_column: registry.get(&key(schema, table)).cloned().flatten(),
                    srid: layer.and_then(|l| l.1),
                    name,
                })
            })
            .collect();
        Catalog { owner: owner.to_string(), release, items, srs: srs.into_iter().take(MAX_ITEMS).collect(), cut }
    }

    /// As [`Catalog::find`], for a decision that rests on a name being absent: a catalog cut at
    /// [`MAX_ITEMS`] can't say a name it doesn't list is unregistered, and says why instead.
    pub fn registered(&self, name: &str) -> std::result::Result<Option<&Item>, String> {
        match self.find(name) {
            None if self.cut => Err(format!(
                "the geodatabase lists {MAX_ITEMS} items or more, so whether {name} is registered in it isn't known: it isn't read, as a versioned class's own table doesn't show its Default version"
            )),
            found => Ok(found),
        }
    }

    /// The feature class or table a name picks: `Mains`, `sde.Mains`, `utility.sde.Mains` or
    /// `SDE.MAINS`, in any case (a three-part name loses its first part, the database). A bare
    /// name matches the short name of an item. Feature datasets are not picked, as a dataset and
    /// a class can share a name; see [`Catalog::feature_datasets`].
    pub fn find(&self, name: &str) -> Option<&Item> {
        let name = name.trim();
        let query = without_database(name).unwrap_or_else(|| name.to_string());
        let same = |a: &str| !a.is_empty() && a.eq_ignore_ascii_case(&query);
        let data = || self.items.iter().filter(|i| i.kind != ItemKind::FeatureDataset);
        // The name as the geodatabase holds it wins over the looser forms.
        data().find(|i| same(&i.name)).or_else(|| {
            data().find(|i| {
                without_database(&i.name).is_some_and(|n| same(&n))
                    || (!i.table.is_empty() && same(&format!("{}.{}", i.schema, i.table)))
                    || (!query.contains('.') && same(i.short_name()))
            })
        })
    }

    /// The feature datasets, in catalog order.
    pub fn feature_datasets(&self) -> Vec<&Item> {
        self.items.iter().filter(|i| i.kind == ItemKind::FeatureDataset).collect()
    }

    /// The feature classes and tables at the root (`None`) or in a feature dataset (named as its
    /// path names it, or by its qualified name).
    pub fn children(&self, dataset: Option<&str>) -> Vec<&Item> {
        self.items
            .iter()
            .filter(|i| i.kind != ItemKind::FeatureDataset)
            .filter(|i| match (dataset, i.dataset.as_deref()) {
                (None, None) => true,
                (Some(want), Some(have)) => short(want).eq_ignore_ascii_case(short(have)),
                _ => false,
            })
            .collect()
    }

    /// The spatial reference with this id.
    pub fn srs(&self, srid: i64) -> Option<&Srs> {
        self.srs.iter().find(|s| s.srid == srid)
    }
}

/// The geodatabase's own tables, never listed as data: `GDB_*`, `SDE_*`, `A<n>`, `D<n>` and `i<n>`
/// (a registered table's add and delete tables and its index), in any case. A registered
/// table's views and archive are told by [`derived_from`].
pub fn is_machinery(table: &str) -> bool {
    let t = table.to_ascii_lowercase();
    let numbered = t.strip_prefix(['a', 'd', 'i']).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    numbered || t.starts_with("gdb_") || t.starts_with("sde_")
}

/// The table a name would be a versioned view (`*_evw`, `*_vw`) or the archive (`*_H`) of, in
/// any case: the name without that ending. It is the geodatabase's own only when that table is
/// registered; a table of the user's own can end the same way.
pub fn derived_from(table: &str) -> Option<&str> {
    ["_evw", "_vw", "_h"].iter().find_map(|end| {
        let cut = table.len().checked_sub(end.len()).filter(|cut| *cut > 0)?;
        table.get(cut..).filter(|tail| tail.eq_ignore_ascii_case(end)).and_then(|_| table.get(..cut))
    })
}

/// How a registered class is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadPlan {
    /// The base table as it is.
    Base,
    /// The versioned view (its name as the schema holds it), which shows the Default version.
    VersionedView(String),
    /// The base table's branch-versioned Default rows: the latest row per ObjectID (the column,
    /// as the table names it) on branch 0 that isn't a delete. Each platform writes the SQL.
    Branch(String),
    /// Not read, for this reason.
    Refused(String),
}

/// How a registered class is read, given whether its shape column is a native spatial column
/// (`geometry` or `geography` in `INFORMATION_SCHEMA`; false for SDEBINARY and other storage), the
/// views named `<table>_evw` or `<table>_vw` that exist in its schema and the columns of its base
/// table.
///
/// The table's own shape outranks its definition: a `GDB_BRANCH_ID` column makes it branch
/// versioned whatever the definition says, and a versioned view makes a class the definition calls
/// unversioned read through that view. A definition that couldn't be read is otherwise refused,
/// as reading the base table of a versioned class would show rows that aren't the Default's.
pub fn read_plan(item: &Item, native_spatial: bool, views: &[String], columns: &[String]) -> ReadPlan {
    if !native_spatial {
        return ReadPlan::Refused(format!(
            "{} is stored in a format RustGIS doesn't read (SDEBINARY or another non-native storage)",
            item.name
        ));
    }
    let has = |want: &str| columns.iter().find(|c| c.eq_ignore_ascii_case(want));
    if item.versioning == Versioning::Branch || has("GDB_BRANCH_ID").is_some() {
        let Some(rowid) = item.rowid_column.as_deref() else {
            return ReadPlan::Refused(format!("{} is branch versioned but the geodatabase registers no ObjectID column for it", item.name));
        };
        let mut missing: Vec<&str> = BRANCH_COLUMNS.iter().copied().filter(|need| has(need).is_none()).collect();
        let column = has(rowid);
        if column.is_none() {
            missing.push(rowid);
        }
        return match column {
            Some(column) if missing.is_empty() => ReadPlan::Branch(column.clone()),
            _ => ReadPlan::Refused(format!("{} is branch versioned but its table lacks {}", item.name, missing.join(", "))),
        };
    }
    if !item.definition_ok {
        return ReadPlan::Refused(format!("{}: its geodatabase definition couldn't be read, so how it is versioned is unknown", item.name));
    }
    let find = |suffix: &str| {
        let want = format!("{}{suffix}", item.table);
        views.iter().find(|v| v.eq_ignore_ascii_case(&want))
    };
    match (item.versioning, find("_evw").or_else(|| find("_vw"))) {
        (_, Some(view)) => ReadPlan::VersionedView(view.clone()),
        (Versioning::Traditional, None) => ReadPlan::Refused(format!(
            "{} is versioned but its versioned view is missing; ask the geodatabase administrator to enable SQL access for it",
            item.name
        )),
        // A class archived without versioning keeps every past row in its base table (the
        // current ones are those its view shows), so the base table isn't the class. (Not yet
        // run against a real archived class.)
        _ if item.archived || has("GDB_TO_DATE").is_some() => ReadPlan::Refused(format!(
            "{} is archived but its view is missing, and its table holds past rows too; ask the geodatabase administrator to enable SQL access for it",
            item.name
        )),
        _ => ReadPlan::Base,
    }
}

/// The part of a name after its last `.`.
fn short(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(_, s)| s)
}

/// The owner and table of a `PhysicalName` (`SDE.MAINS` or `UTILITY.SDE.MAINS` give `SDE` and
/// `MAINS`): its last two parts, the owner empty when there is only one.
fn split_physical(physical: &str) -> (&str, &str) {
    let mut parts = physical.rsplit('.');
    let table = parts.next().unwrap_or(physical);
    (parts.next().unwrap_or(""), table)
}

/// A three-part name (`utility.sde.Mains`) without its first part (`sde.Mains`); `None` for any
/// other.
fn without_database(name: &str) -> Option<String> {
    let mut parts = name.split('.');
    let (_database, owner, table) = (parts.next()?, parts.next()?, parts.next()?);
    parts.next().is_none().then(|| format!("{owner}.{table}"))
}

/// A table's key for matching rows of different system tables.
fn key(owner: &str, table: &str) -> (String, String) {
    (owner.to_ascii_lowercase(), table.to_ascii_lowercase())
}

/// The kind of item a `GDB_ITEMTYPES` name is, `None` for one that isn't listed as data.
fn kind_of(type_name: &str) -> Option<ItemKind> {
    match type_name.trim().to_ascii_lowercase().as_str() {
        "feature dataset" => Some(ItemKind::FeatureDataset),
        "feature class" => Some(ItemKind::FeatureClass),
        "table" => Some(ItemKind::Table),
        _ => None,
    }
}

/// The kind of geometry an `esriGeometryType` names (`esriGeometryPolyline` gives lines); `None`
/// for any other.
fn family_of(shape_type: &str) -> Option<Family> {
    let t = shape_type.trim();
    match t.strip_prefix("esriGeometry").unwrap_or(t).to_ascii_lowercase().as_str() {
        "point" | "multipoint" => Some(Family::Points),
        "polyline" => Some(Family::Lines),
        "polygon" | "multipatch" => Some(Family::Polygons),
        _ => None,
    }
}

/// Reads a `Definition`; `None` for one that is too large or not XML.
fn read_definition(xml: &str) -> Option<Definition> {
    if xml.len() > MAX_DEFINITION_BYTES {
        return None;
    }
    let doc = crate::xml::parse(xml, roxmltree::ParsingOptions::default()).ok()?;
    let named = |n: &roxmltree::Node, name: &str| n.is_element() && n.tag_name().name() == name;
    let text = |n: roxmltree::Node| n.text().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    // Element names differ by root (`DEFeatureClassInfo`, `DETableInfo`, ...), so descendants are
    // searched by local name; a field's own `AliasName` isn't the class's.
    let item_value =
        |name: &str| doc.descendants().find(|n| named(n, name) && !n.ancestors().any(|a| named(&a, "GPFieldInfoEx"))).and_then(text);
    let field_aliases = doc
        .descendants()
        .filter(|n| named(n, "GPFieldInfoEx"))
        .take(MAX_FIELDS)
        .filter_map(|f| {
            let child = |name: &str| f.children().find(|c| named(c, name)).and_then(text);
            let (name, alias) = (child("Name")?, child("AliasName")?);
            (name != alias).then_some((name, alias))
        })
        .collect();
    let flag = |name: &str| item_value(name).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let branch = item_value("VersioningType").is_some_and(|v| v == "2" || v.to_ascii_lowercase().contains("branch"));
    let versioning = if branch {
        Some(Versioning::Branch)
    } else if flag("Versioned") {
        Some(Versioning::Traditional)
    } else {
        None
    };
    Some(Definition {
        alias: item_value("AliasName"),
        shape_column: item_value("ShapeFieldName"),
        field_aliases,
        versioning,
        archived: flag("IsArchived"),
        family: item_value("ShapeType").as_deref().and_then(family_of),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, physical: &str, path: &str, type_name: &str, definition: Option<&str>) -> ItemRow {
        ItemRow {
            name: name.into(),
            physical: physical.into(),
            path: path.into(),
            type_name: type_name.into(),
            definition: definition.map(String::from),
        }
    }

    const MAINS_DEF: &str = "<DEFeatureClassInfo><AliasName>Water Mains</AliasName><ShapeFieldName>SHAPE</ShapeFieldName>\
        <Versioned>true</Versioned><IsArchived>false</IsArchived><GPFieldInfoExs><GPFieldInfoEx><Name>DIAM</Name>\
        <AliasName>Diameter</AliasName></GPFieldInfoEx></GPFieldInfoExs></DEFeatureClassInfo>";

    fn catalog() -> Catalog {
        Catalog::from_rows(
            "sde",
            Some("4.2".into()),
            vec![
                row("utility.sde.Water", "", "\\Water", "Feature Dataset", None),
                row("utility.sde.Mains", "SDE.MAINS", "\\Water\\Mains", "Feature Class", Some(MAINS_DEF)),
                row("sde.Readings", "SDE.READINGS", "\\Readings", "Table", None),
                row("sde.Links", "SDE.LINKS", "\\Links", "Relationship Class", None),
            ],
            vec![RegistryRow { owner: "SDE".into(), table: "MAINS".into(), rowid_column: Some("OBJECTID".into()) }],
            vec![LayerRow { owner: "SDE".into(), table: "MAINS".into(), column: "SHAPE".into(), srid: Some(2) }],
            vec![Srs { srid: 2, auth_name: None, auth_srid: Some(4326), srtext: None }],
        )
    }

    fn names(items: Vec<&Item>) -> Vec<&str> {
        items.into_iter().map(|i| i.name.as_str()).collect()
    }

    fn plain(name: &str, table: &str, versioning: Versioning) -> Item {
        Item {
            name: name.into(),
            schema: "SDE".into(),
            table: table.into(),
            kind: ItemKind::FeatureClass,
            dataset: None,
            alias: None,
            shape_column: None,
            field_aliases: vec![],
            versioning,
            definition_ok: true,
            family: None,
            archived: false,
            rowid_column: Some("OBJECTID".into()),
            srid: None,
        }
    }

    #[test]
    fn items_come_with_dataset_alias_and_fields() {
        let c = catalog();
        assert_eq!(c.owner, "sde");
        assert_eq!(c.release.as_deref(), Some("4.2"));
        assert_eq!(c.items.len(), 3, "the relationship class isn't listed");
        let mains = c.find("Mains").unwrap();
        assert_eq!((mains.schema.as_str(), mains.table.as_str()), ("SDE", "MAINS"));
        assert_eq!(mains.kind, ItemKind::FeatureClass);
        assert_eq!(mains.dataset.as_deref(), Some("Water"));
        assert_eq!(mains.alias.as_deref(), Some("Water Mains"));
        assert_eq!(mains.field_aliases, vec![("DIAM".to_string(), "Diameter".to_string())]);
        assert_eq!(mains.versioning, Versioning::Traditional);
        assert!(!mains.archived);
        assert_eq!(mains.rowid_column.as_deref(), Some("OBJECTID"));
        assert_eq!(mains.srid, Some(2));
        assert_eq!(mains.shape_column.as_deref(), Some("SHAPE"));
        assert_eq!(names(c.children(Some("Water"))), ["utility.sde.Mains"]);
        assert_eq!(names(c.children(Some("utility.sde.water"))), ["utility.sde.Mains"]);
        assert_eq!(names(c.children(None)), ["sde.Readings"]);
        assert_eq!(names(c.feature_datasets()), ["utility.sde.Water"]);
        let readings = c.find("Readings").unwrap();
        assert_eq!((readings.kind, readings.dataset.as_deref(), readings.alias.as_deref()), (ItemKind::Table, None, None));
        assert_eq!(readings.versioning, Versioning::Unversioned);
        assert_eq!(c.srs(2).and_then(|s| s.auth_srid), Some(4326));
        assert!(c.srs(3).is_none());
    }

    #[test]
    fn an_alias_equal_to_the_name_is_none_and_the_shape_falls_back_to_the_layer() {
        let def = "<DETableInfo><AliasName>Mains</AliasName></DETableInfo>";
        let c = Catalog::from_rows(
            "sde",
            None,
            vec![row("sde.Mains", "SDE.MAINS", "\\Mains", "Feature Class", Some(def))],
            vec![],
            vec![LayerRow { owner: "sde".into(), table: "mains".into(), column: "SHAPE".into(), srid: None }],
            vec![],
        );
        let item = c.items.first().unwrap();
        assert_eq!((item.alias.as_deref(), item.shape_column.as_deref(), item.srid), (None, Some("SHAPE"), None));
    }

    #[test]
    fn a_database_qualified_physical_name_gives_its_owner_and_table() {
        let c = Catalog::from_rows(
            "sde",
            None,
            vec![
                row("utility.sde.Mains", "UTILITY.SDE.MAINS", "\\Mains", "Feature Class", None),
                row("Bare", "BARE", "\\Bare", "Table", None),
            ],
            vec![RegistryRow { owner: "SDE".into(), table: "MAINS".into(), rowid_column: Some("OBJECTID".into()) }],
            vec![],
            vec![],
        );
        let mains = c.find("sde.mains").unwrap();
        assert_eq!((mains.schema.as_str(), mains.table.as_str()), ("SDE", "MAINS"));
        assert_eq!(mains.rowid_column.as_deref(), Some("OBJECTID"));
        let bare = c.find("bare").unwrap();
        assert_eq!((bare.schema.as_str(), bare.table.as_str()), ("", "BARE"));
    }

    #[test]
    fn names_match_in_every_form() {
        let c = catalog();
        for q in ["mains", "sde.MAINS", "utility.sde.Mains", "SDE.MAINS", "UTILITY.SDE.MAINS"] {
            assert_eq!(c.find(q).map(|i| i.name.as_str()), Some("utility.sde.Mains"), "{q}");
        }
        assert!(c.find("water.mains").is_none());
        assert!(c.find("").is_none());
        assert_eq!(c.find("sde.readings").map(|i| i.name.as_str()), Some("sde.Readings"));
    }

    #[test]
    fn a_bad_definition_leaves_the_item_listed() {
        let big = format!("<DEFeatureClassInfo><AliasName>Big</AliasName></DEFeatureClassInfo>{}", " ".repeat(MAX_DEFINITION_BYTES));
        for def in [Some("<not xml".to_string()), Some(big), None] {
            let rows = vec![row("sde.Mains", "SDE.MAINS", "\\Mains", "Feature Class", def.as_deref())];
            let c = Catalog::from_rows("sde", None, rows, vec![], vec![], vec![]);
            assert_eq!(c.items.len(), 1);
            let item = c.items.first().unwrap();
            assert_eq!(item.alias, None);
            assert!(!item.definition_ok, "{def:?}");
        }
        let rows = vec![row("sde.Mains", "SDE.MAINS", "\\Mains", "Feature Class", Some("<DEFeatureClassInfo/>"))];
        assert!(Catalog::from_rows("sde", None, rows, vec![], vec![], vec![]).items.first().unwrap().definition_ok);
        let rows: Vec<ItemRow> = (0..=MAX_ITEMS).map(|i| row(&format!("sde.T{i}"), &format!("SDE.T{i}"), "\\T", "Table", None)).collect();
        assert_eq!(Catalog::from_rows("sde", None, rows, vec![], vec![], vec![]).items.len(), MAX_ITEMS);
    }

    #[test]
    fn a_catalog_over_its_limit_is_cut_and_says_so() {
        // The statement asks for at most `MAX_ITEMS` rows, so a full answer is a cut one.
        let rows =
            |n: usize| -> Vec<ItemRow> { (0..n).map(|i| row(&format!("sde.T{i}"), &format!("SDE.T{i}"), "\\T", "Table", None)).collect() };
        let full = Catalog::from_rows("sde", None, rows(MAX_ITEMS), vec![], vec![], vec![]);
        assert_eq!(full.items.len(), MAX_ITEMS);
        assert_eq!(full.cut_note().as_deref(), Some("the geodatabase lists 10000 items or more: only the first 10000 are shown"));
        assert_eq!(Catalog::from_rows("sde", None, rows(MAX_ITEMS - 1), vec![], vec![], vec![]).cut_note(), None);
    }

    #[test]
    fn branch_versioning_is_read_from_the_definition() {
        let versioning = |inner: &str| {
            let def = format!("<DETableInfo>{inner}</DETableInfo>");
            let rows = vec![row("sde.T", "SDE.T", "\\T", "Table", Some(&def))];
            let c = Catalog::from_rows("sde", None, rows, vec![], vec![], vec![]);
            c.items.first().map(|i| i.versioning)
        };
        assert_eq!(versioning("<VersioningType>esriVersioningTypeBranch</VersioningType>"), Some(Versioning::Branch));
        assert_eq!(versioning("<VersioningType>2</VersioningType>"), Some(Versioning::Branch));
        assert_eq!(versioning("<Versioned>false</Versioned>"), Some(Versioning::Unversioned));
        assert_eq!(versioning("<Versioned>true</Versioned><VersioningType>1</VersioningType>"), Some(Versioning::Traditional));
        assert_eq!(versioning(""), Some(Versioning::Unversioned));
    }

    #[test]
    fn shape_types_give_families() {
        let family = |shape_type: &str| {
            let def = format!("<DEFeatureClassInfo><ShapeType>{shape_type}</ShapeType></DEFeatureClassInfo>");
            let rows = vec![row("sde.T", "SDE.T", "\\T", "Feature Class", Some(&def))];
            Catalog::from_rows("sde", None, rows, vec![], vec![], vec![]).items.first().and_then(|i| i.family)
        };
        assert_eq!(family("esriGeometryPoint"), Some(Family::Points));
        assert_eq!(family("esriGeometryMultipoint"), Some(Family::Points));
        assert_eq!(family("esriGeometryPolyline"), Some(Family::Lines));
        assert_eq!(family("esriGeometryPolygon"), Some(Family::Polygons));
        assert_eq!(family("esriGeometryMultiPatch"), Some(Family::Polygons));
        assert_eq!(family(" esriGeometryPOLYLINE "), Some(Family::Lines));
        assert_eq!(family("esriGeometryNull"), None);
        assert_eq!(family(""), None);
        let rows = vec![row("sde.T", "SDE.T", "\\T", "Table", Some("<DETableInfo/>"))];
        assert_eq!(Catalog::from_rows("sde", None, rows, vec![], vec![], vec![]).items.first().and_then(|i| i.family), None);
    }

    #[test]
    fn versioning_serializes_as_none_traditional_branch() {
        let json = |v| serde_json::to_string(&v).unwrap();
        assert_eq!(json(Versioning::Unversioned), "\"none\"");
        assert_eq!(json(Versioning::Traditional), "\"traditional\"");
        assert_eq!(json(Versioning::Branch), "\"branch\"");
        assert_eq!(serde_json::to_string(&ItemKind::FeatureDataset).unwrap(), "\"featureDataset\"");
    }

    #[test]
    fn machinery_is_hidden() {
        for t in ["GDB_ITEMS", "SDE_layers", "A12", "D12", "i12"] {
            assert!(is_machinery(t), "{t}");
        }
        for t in ["Mains", "A_Roads", "Dams", "History", "A", "i", "MAINS_evw", "Sites_H"] {
            assert!(!is_machinery(t), "{t}");
        }
        for (t, of) in [("MAINS_evw", "MAINS"), ("mains_VW", "mains"), ("MAINS_H", "MAINS")] {
            assert_eq!(derived_from(t), Some(of), "{t}");
        }
        for t in ["Mains", "_H", "_evw", "Hevw", "\u{e9}_"] {
            assert_eq!(derived_from(t), None, "{t}");
        }
    }

    #[test]
    fn read_plans() {
        let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let traditional = plain("sde.Mains", "MAINS", Versioning::Traditional);
        let branch = plain("sde.Mains", "MAINS", Versioning::Branch);
        let base = plain("sde.Mains", "MAINS", Versioning::Unversioned);
        assert_eq!(read_plan(&traditional, true, &strings(&["MAINS_EVW"]), &[]), ReadPlan::VersionedView("MAINS_EVW".into()));
        assert_eq!(read_plan(&traditional, true, &strings(&["mains_vw", "mains_evw"]), &[]), ReadPlan::VersionedView("mains_evw".into()));
        assert_eq!(read_plan(&traditional, true, &strings(&["MAINS_vw"]), &[]), ReadPlan::VersionedView("MAINS_vw".into()));
        match read_plan(&traditional, true, &[], &[]) {
            ReadPlan::Refused(why) => assert!(why.contains("versioned view is missing"), "{why}"),
            other => panic!("{other:?}"),
        }
        let all = strings(&["objectid", "gdb_branch_id", "GDB_IS_DELETE", "GDB_FROM_DATE"]);
        assert_eq!(read_plan(&branch, true, &[], &all), ReadPlan::Branch("objectid".into()));
        match read_plan(&branch, true, &[], &strings(&["GDB_BRANCH_ID", "GDB_IS_DELETE"])) {
            ReadPlan::Refused(why) => {
                assert!(why.contains("GDB_FROM_DATE") && why.contains("OBJECTID") && !why.contains("GDB_IS_DELETE"), "{why}")
            }
            other => panic!("{other:?}"),
        }
        match read_plan(&Item { rowid_column: None, ..branch.clone() }, true, &[], &all) {
            ReadPlan::Refused(why) => assert!(why.contains("no ObjectID column"), "{why}"),
            other => panic!("{other:?}"),
        }
        match read_plan(&base, false, &[], &[]) {
            ReadPlan::Refused(why) => assert!(why.contains("SDEBINARY"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(read_plan(&base, true, &[], &[]), ReadPlan::Base);
    }

    #[test]
    fn an_archived_class_without_its_view_is_refused() {
        let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let base = plain("sde.Mains", "MAINS", Versioning::Unversioned);
        let archived = Item { archived: true, ..base.clone() };
        // Its base table keeps every past row, so it isn't read as the class.
        match read_plan(&archived, true, &[], &[]) {
            ReadPlan::Refused(why) => assert!(why.contains("archiv") && why.contains("sde.Mains"), "{why}"),
            other => panic!("{other:?}"),
        }
        // The table's own columns say so whatever the definition does.
        match read_plan(&base, true, &[], &strings(&["OBJECTID", "gdb_from_date", "GDB_TO_DATE"])) {
            ReadPlan::Refused(why) => assert!(why.contains("archiv"), "{why}"),
            other => panic!("{other:?}"),
        }
        // Its view shows the current rows.
        assert_eq!(read_plan(&archived, true, &strings(&["MAINS_EVW"]), &[]), ReadPlan::VersionedView("MAINS_EVW".into()));
        assert_eq!(read_plan(&base, true, &[], &strings(&["OBJECTID", "NAME"])), ReadPlan::Base);
    }

    #[test]
    fn a_cut_catalog_does_not_vouch_for_a_missing_name() {
        let mut c = catalog();
        assert_eq!(c.registered("sde.Other"), Ok(None));
        assert_eq!(c.registered("Mains").map(|i| i.map(|i| i.table.as_str())), Ok(Some("MAINS")));
        c.cut = true;
        assert_eq!(c.registered("Mains").map(|i| i.map(|i| i.table.as_str())), Ok(Some("MAINS")));
        let why = c.registered("sde.Other").unwrap_err();
        assert!(why.contains("sde.Other") && why.contains("isn't known"), "{why}");
    }

    #[test]
    fn an_unreadable_definition_is_refused() {
        let unknown = Item { definition_ok: false, ..plain("sde.Mains", "MAINS", Versioning::Unversioned) };
        match read_plan(&unknown, true, &[], &["OBJECTID".to_string()]) {
            ReadPlan::Refused(why) => {
                assert_eq!(why, "sde.Mains: its geodatabase definition couldn't be read, so how it is versioned is unknown")
            }
            other => panic!("{other:?}"),
        }
        // non-native storage is still the first thing said
        match read_plan(&unknown, false, &[], &[]) {
            ReadPlan::Refused(why) => assert!(why.contains("SDEBINARY"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_table_outranks_its_definition() {
        let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let branch_columns = strings(&["OBJECTID", "GDB_BRANCH_ID", "GDB_IS_DELETE", "GDB_FROM_DATE", "SHAPE"]);
        // A branch table is read as one whatever the definition says, even an unreadable one.
        for item in [
            plain("sde.Mains", "MAINS", Versioning::Unversioned),
            plain("sde.Mains", "MAINS", Versioning::Traditional),
            Item { definition_ok: false, ..plain("sde.Mains", "MAINS", Versioning::Unversioned) },
        ] {
            assert_eq!(read_plan(&item, true, &strings(&["MAINS_evw"]), &branch_columns), ReadPlan::Branch("OBJECTID".into()));
        }
        // A class called unversioned that has a versioned view is read through it.
        let unversioned = plain("sde.Mains", "MAINS", Versioning::Unversioned);
        let columns = strings(&["OBJECTID", "SHAPE"]);
        assert_eq!(read_plan(&unversioned, true, &strings(&["mains_evw"]), &columns), ReadPlan::VersionedView("mains_evw".into()));
        assert_eq!(read_plan(&unversioned, true, &strings(&["MAINS_vw"]), &columns), ReadPlan::VersionedView("MAINS_vw".into()));
        assert_eq!(read_plan(&unversioned, true, &strings(&["OTHER_evw"]), &columns), ReadPlan::Base);
    }
}
