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

/// A slot's identity across a bridge exchange. Positions are deliberately absent, so reordering
/// layers preserves identity while changing their owner, role or id does not.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SlotAt {
    Layer { id: u32, role: LayerRole },
    Tile { id: String, role: TileRole },
    Group { id: u32, child: Option<u32> },
    Terrain,
}

pub struct Slot<'a> {
    pub at: SlotPos,
    pub value: &'a str,
    pub address: bool,
    pub local_path: bool,
}
