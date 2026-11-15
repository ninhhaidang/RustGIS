//! Stable leaf locations for a synchronous output transaction. Resolving a location neither
//! discovers slots nor changes structure; addresses and local paths share a single location.

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SlotPos {
    Group { group: usize, child: Option<usize> },
    Tile { tile: usize, role: TileRole },
    Terrain,
    Layer { layer: usize, role: LayerRole },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayerRole {
    Dataset,
    Streamed,
    Raster,
    ZarrStore,
    GdalPath,
    DuckDbDatabase,
    DuckDbSql,
    GpkgPath,
    PostgisEditDatabase,
    PostgisDatabase,
    MysqlDatabase,
    SqlServerDatabase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileRole {
    Url,
    TimeUrl,
    TimeAt(usize),
    Legend(usize),
}

pub struct Slot<'a> {
    pub at: SlotPos,
    pub value: &'a str,
    pub address: bool,
    pub local_path: bool,
}
