"""RustGIS from Python: the RustGIS engine in-process, headless.

Every action in RustGIS is a command with an id and JSON parameters, the same ids the ribbon,
the command line and the AI Assistant use. ``Session.run`` runs any of them; the other methods
are shortcuts for the common ones, and ``Layer`` wraps one layer of the map.

>>> import rustgis
>>> s = rustgis.Session(sample=True)
>>> buffers = s.run_tool("buffer", input=s["Cities"], distance=100)
>>> s["Countries"].style("graduated", field="pop_est")
>>> s.to_png("map.png")
>>> s.save("atlas.rgis")

In Jupyter, ``rustgis.Map`` shows the RustGIS app in the cell, in step with its session
(``pip install "rustgis[jupyter]"``); in a Qt application, ``rustgis.qt.MapWidget`` does
(``pip install "rustgis[qt]"``).
"""

from __future__ import annotations

import json
import os
from typing import Any, Dict, Iterable, Iterator, List, Optional, Tuple, Union

from ._rustgis import Engine as _Engine
from ._rustgis import RustGISError, __version__
from ._rustgis import load_plugins as _load_plugins

__all__ = ["Layer", "Map", "RustGISError", "Session", "__version__", "load_plugins"]

PathLike = Union[str, "os.PathLike[str]"]


def __getattr__(name: str) -> Any:
    """Imports ``Map`` on first use, so the package works without the notebook widget's
    dependencies.

    Args:
        name: The attribute asked for.

    Raises:
        ImportError: When ``Map`` is asked for and anywidget isn't installed.
        AttributeError: For any other name.
    """
    if name == "Map":
        try:
            from .widget import Map
        except ImportError as e:
            raise ImportError('rustgis.Map needs anywidget: pip install "rustgis[jupyter]"') from e
        return Map
    raise AttributeError(f"module 'rustgis' has no attribute {name!r}")


def load_plugins() -> List[str]:
    """Loads the plugins installed for the desktop app, so their commands and tools run here too.

    Returns:
        The problems found, one message per plugin that couldn't be loaded.
    """
    return list(_load_plugins())


class Layer:
    """A layer of a session's active map.

    A layer is kept by its id, so it is the same layer after a rename. Its properties read the
    session each time: they show what was done since, in Python or in the app.

    Attributes:
        session: The session (or :class:`Map`) the layer belongs to.
        id: The layer's id, unique in its map.
    """

    def __init__(self, session: "_Api", layer: Union[str, int]) -> None:
        """Refers to a layer of ``session``.

        Args:
            session: The session (or :class:`Map`) the layer belongs to.
            layer: The layer's name, or its id.

        Raises:
            RustGISError: When the active map has no such layer.
        """
        self.session = session
        self.id: int = session._find_layer(layer)["id"]

    @classmethod
    def _of(cls, session: "_Api", layer_id: int) -> "Layer":
        """A handle on a layer whose id is known to exist, without looking it up again.

        Args:
            session: The session (or :class:`Map`) the layer belongs to.
            layer_id: The layer's id.
        """
        layer = cls.__new__(cls)
        layer.session = session
        layer.id = layer_id
        return layer

    @property
    def info(self) -> Dict[str, Any]:
        """The layer's summary: id, name, type, feature count, fields, extent, renderer,
        selection and visibility.

        Raises:
            RustGISError: When the layer was removed.
        """
        return self.session._find_layer(self.id)

    @property
    def name(self) -> str:
        """The layer's name; :meth:`rename` changes it."""
        return str(self.info.get("name", ""))

    @property
    def type(self) -> str:
        """The layer's kind: ``"Point"``, ``"Line"``, ``"Polygon"``, ``"Raster"`` and so on."""
        return str(self.info.get("type", ""))

    @property
    def count(self) -> int:
        """How many features the layer holds."""
        return int(self.info.get("features") or 0)

    @property
    def bounds(self) -> Optional[Tuple[float, float, float, float]]:
        """The layer's extent as ``(west, south, east, north)`` in degrees, or None when it is
        empty."""
        extent = self.info.get("extent")
        return tuple(extent) if extent else None  # type: ignore[return-value]

    @property
    def fields(self) -> List[str]:
        """The names of the layer's attribute fields."""
        return [field["name"] for field in self.session._query("layer.fields", layer=self.id)]

    @property
    def selected(self) -> int:
        """How many of the layer's features are selected."""
        return int(self.info.get("selected") or 0)

    @property
    def visible(self) -> bool:
        """Whether the layer draws; set it to show or hide the layer."""
        return bool(self.info.get("visible"))

    @visible.setter
    def visible(self, value: bool) -> None:
        self.session.run("layer.visibility", layer=self.id, visible=bool(value))

    # Changing the layer --------------------------------------------------------------------

    def style(self, type: str = "single", field: Optional[str] = None, **params: Any) -> "Layer":
        """Sets how the layer draws (``layer.symbology``).

        Args:
            type: ``"single"`` (one symbol), ``"unique"`` (a colour per value), ``"graduated"``
                (colours by class) or ``"proportional"`` (symbol sizes by value).
            field: The field the colours or sizes come from (not for ``"single"``).
            **params: More ``layer.symbology`` parameters: ``classes`` (1 to 12), ``method``
                (``"naturalBreaks"``, ``"equalInterval"``, ``"quantile"``,
                ``"geometricInterval"``, ``"standardDeviation"``, ``"arithmeticProgression"``,
                ``"headTailBreaks"``, ``"manual"``),
                ``breaks`` (manual: the classes' upper bounds), ``ramp``.

        Returns:
            The layer, so calls can be chained.
        """
        if field is not None:
            params["field"] = field
        self.session.run("layer.symbology", layer=self.id, type=type, **params)
        return self

    def set_transparency(self, value: float) -> "Layer":
        """Sets the layer's transparency.

        Args:
            value: 0 (opaque) to 100 (invisible).

        Returns:
            The layer.
        """
        self.session.run("layer.transparency", layer=self.id, value=value)
        return self

    def filter(self, where: Optional[str]) -> "Layer":
        """Shows only the features matching a SQL where clause (a definition query).

        Args:
            where: The clause, e.g. ``"pop_max > 1000000"``; None or ``""`` shows them all.

        Returns:
            The layer.
        """
        self.session.run("layer.definitionQuery", layer=self.id, where=where or "")
        return self

    def rename(self, name: str) -> "Layer":
        """Renames the layer.

        Args:
            name: The new name.

        Returns:
            The layer.
        """
        self.session.run("layer.rename", layer=self.id, name=name)
        return self

    def remove(self) -> None:
        """Removes the layer from the map (``undo`` brings it back)."""
        self.session.run("layer.remove", layer=self.id)

    def zoom_to(self) -> None:
        """Zooms the map to the layer's features."""
        self.session.run("layer.zoomTo", layer=self.id)

    # Selection -----------------------------------------------------------------------------

    def select(self, where: str, mode: str = "new") -> int:
        """Selects the features matching a SQL where clause.

        Args:
            where: The clause, e.g. ``"pop_max > 10000000"``.
            mode: ``"new"`` (replace the selection), ``"add"``, ``"remove"`` or ``"subset"``
                (select among the selected).

        Returns:
            How many features are selected now.
        """
        result = self.session.run("select.byAttributes", layer=self.id, where=where, mode=mode)
        return int(result.get("selected", 0)) if isinstance(result, dict) else self.selected

    def clear_selection(self) -> None:
        """Clears the selection, in this layer and every other."""
        self.session.run("select.clear")

    # Reading and writing -------------------------------------------------------------------

    def to_geojson(self, selected: bool = False) -> Dict[str, Any]:
        """The layer's features as a GeoJSON FeatureCollection, in longitude and latitude.

        A large file read in place holds the features of the last view; run
        ``session.run("layer.loadAll", layer=name)`` first to get all of them.

        Args:
            selected: Only the selected features.

        Returns:
            The FeatureCollection as a dict.
        """
        return json.loads(self.session._layer_geojson(self.id, selected))

    def to_geopandas(self, selected: bool = False) -> Any:
        """The layer as a ``geopandas.GeoDataFrame`` in EPSG:4326.

        Args:
            selected: Only the selected features.

        Returns:
            A GeoDataFrame with the layer's features and attributes.

        Raises:
            ImportError: When geopandas isn't installed (``pip install rustgis[geopandas]``).
        """
        import geopandas as gpd

        collection = self.to_geojson(selected)
        return gpd.GeoDataFrame.from_features(collection["features"], crs="EPSG:4326")

    def to_file(
        self,
        path: PathLike,
        crs: Optional[str] = None,
        *,
        mode: Optional[str] = None,
        table_name: Optional[str] = None,
    ) -> Any:
        """Writes the layer's features to a file (``share.exportLayer``).

        Args:
            path: The file; its extension picks the format: ``.geojson``, ``.csv``, ``.fgb``,
                ``.gpkg``, ``.parquet``, ``.shp``, ``.zip``, ``.kml``, ``.kmz`` or ``.gpx``.
            crs: The coordinate system of a GeoPackage, Shapefile or FlatGeobuf, e.g.
                ``"EPSG:3857"`` (the one the layer was read in when left out).
            mode: For a GeoPackage: ``"newFile"`` (the default; an existing file is refused),
                ``"addTable"`` (one more table in an existing GeoPackage, or a new file when
                there is none) or ``"replaceTable"`` (replacing its table of that name, which
                must be there).
            table_name: The GeoPackage table (the layer's name when left out).
        """
        params: Dict[str, Any] = {"layer": self.id, "path": os.fspath(path)}
        if crs is not None:
            params["crs"] = crs
        if mode is not None:
            params["mode"] = mode
        if table_name is not None:
            params["tableName"] = table_name
        return self.session.run("share.exportLayer", params)

    def __eq__(self, other: object) -> bool:
        return (
            isinstance(other, Layer)
            and other.session._root() is self.session._root()
            and other.id == self.id
        )

    def __hash__(self) -> int:
        return hash((id(self.session._root()), self.id))

    def __repr__(self) -> str:
        try:
            return f"Layer({self.name!r}, id={self.id})"
        except RustGISError:
            return f"Layer(id={self.id}, removed)"


class _Api:
    """What :class:`Session` and :class:`Map` share: shortcuts for the common commands, all
    made of :meth:`run`."""

    def run(self, command: str, params: Optional[Dict[str, Any]] = None, /, **kwargs: Any) -> Any:
        """Runs a RustGIS command by id (see :meth:`Session.run`)."""
        raise NotImplementedError

    def _query(self, command: str, /, **params: Any) -> Any:
        """Runs a command that only reads the project.

        Args:
            command: The command id.
            **params: Its parameters.
        """
        return self.run(command, **params)

    def _layer_geojson(self, layer: int, selected: bool) -> str:
        """A layer as GeoJSON text.

        Args:
            layer: The layer's id.
            selected: Only its selected features.
        """
        raise NotImplementedError

    def _root(self) -> "Session":
        """The session that holds the project: this one, or the one a :class:`Map` shows."""
        raise NotImplementedError

    def _data_command(self, path: PathLike) -> Tuple[str, Dict[str, Any]]:
        """The command adding a file, and its parameters.

        Args:
            path: The file.
        """
        return "layer.addData", {"path": os.fspath(path)}

    def _outputs(self, result: Any) -> Any:
        """A command's result as layers: the :class:`Layer` it added, a list when it added
        several, else the result as it is (when it added no layer, only tables, say).

        A file of several layers (the tables of a GeoPackage, a GDAL file split by geometry
        type) reports the first as ``"layer"`` and every one as ``"layers"``, so ``"layers"``
        decides. Standalone tables added with the layers are in :meth:`info`, not returned.

        Args:
            result: The command's result.
        """
        if isinstance(result, dict):
            many = result.get("layers")
            ids = [i for i in many if isinstance(i, int) and not isinstance(i, bool)] if isinstance(many, list) else []
            if len(ids) > 1:
                return [Layer._of(self, i) for i in ids]
            added = result.get("layer")
            if isinstance(added, int) and not isinstance(added, bool):
                return Layer._of(self, added)
            if ids:
                return Layer._of(self, ids[0])
        return result

    # Commands ------------------------------------------------------------------------------

    def commands(self) -> List[Dict[str, Any]]:
        """Every command: its id, label, ribbon location and parameters.

        Returns:
            One dict per command.
        """
        result = self._query("info.commands")
        return result.get("commands", result) if isinstance(result, dict) else result

    def tools(self) -> List[Dict[str, Any]]:
        """The geoprocessing tools, with their parameters.

        Returns:
            One dict per tool: its id, name, toolbox, summary and parameters.
        """
        return self._query("gp.tools")

    def describe(self, name: str) -> Dict[str, Any]:
        """A command's or a geoprocessing tool's description, with its parameters.

        Args:
            name: A command id (``"layer.symbology"``) or a tool id (``"buffer"``).

        Returns:
            The command or tool, as :meth:`commands` or :meth:`tools` lists it.

        Raises:
            KeyError: When there is no command or tool of that id.
        """
        for item in self.commands():
            if item.get("id") == name:
                return item
        for item in self.tools():
            if item.get("id") == name:
                return item
        raise KeyError(f"no command or tool named {name!r}")

    def run_tool(self, tool: str, /, **params: Any) -> Any:
        """Runs a geoprocessing tool (``gp.run``).

        Layers can be given as :class:`Layer` objects or by name; tools take layers by name,
        so of two layers with the same name, the upper one is used. :meth:`describe` lists a
        tool's parameters.

        Args:
            tool: The tool's id, e.g. ``"buffer"``.
            **params: The tool's parameters.

        Returns:
            The :class:`Layer` the tool made, a list of them when it made several, or the
            tool's result as it is (``{"selected": n}`` for a selection, ``{"table": name}``).

        Raises:
            RustGISError: When the tool fails.

        Example:
            >>> buffers = s.run_tool("buffer", input=s["Cities"], distance=100)
            >>> buffers.style("single")
        """
        return self._outputs(self.run("gp.run", tool=tool, params=params))

    def undo(self) -> Any:
        """Undoes the last change."""
        return self.run("edit.undo")

    def redo(self) -> Any:
        """Redoes the last undone change."""
        return self.run("edit.redo")

    # Projects ------------------------------------------------------------------------------

    def load_project(self, path: PathLike) -> Any:
        """Opens a project file (``.rgis``), replacing the current project.

        Args:
            path: The project file.
        """
        return self.run("project.open", path=os.fspath(path))

    def save(self, path: PathLike) -> Any:
        """Saves the project to ``path`` (``.rgis``); the app opens it as it is.

        Args:
            path: Where to save.
        """
        return self.run("project.saveAs", path=os.fspath(path))

    def info(self) -> Dict[str, Any]:
        """The project's summary: the active map, basemap, view and layers."""
        return self._query("info.project")

    # Layers --------------------------------------------------------------------------------

    def layers(self) -> List[Dict[str, Any]]:
        """The active map's layers, top first, each with its id, name, type, feature count,
        fields, renderer and visibility."""
        return list(self.info().get("layers", []))

    def layer_names(self) -> List[str]:
        """The names of the active map's layers, top first."""
        return [str(layer.get("name")) for layer in self.layers()]

    def layer(self, name: Union[str, int]) -> Layer:
        """A layer of the active map (``session["Cities"]`` does the same).

        Args:
            name: The layer's name, or its id.

        Raises:
            RustGISError: When the map has no such layer.
        """
        return Layer(self, name)

    def _find_layer(self, layer: Union[str, int, Layer]) -> Dict[str, Any]:
        """A layer's summary, found by name (exactly, else ignoring case) or id.

        Args:
            layer: The layer's name, id, or the layer.

        Raises:
            RustGISError: When the map has no such layer.
        """
        if isinstance(layer, Layer):
            if layer.session._root() is not self._root():
                raise RustGISError(f"{layer!r} belongs to another session")
            layer = layer.id
        if isinstance(layer, bool) or not isinstance(layer, (int, str)):
            raise RustGISError(f"a layer is a name or an id, not {layer!r}")
        layers = self.layers()
        if isinstance(layer, int):
            found = [entry for entry in layers if entry.get("id") == layer]
        else:
            found = [entry for entry in layers if entry.get("name") == layer]
            folded = layer.lower()
            found = found or [entry for entry in layers if str(entry.get("name", "")).lower() == folded]
            found = found or [entry for entry in layers if str(entry.get("id")) == layer.strip()]
        if not found:
            what = f"with id {layer}" if isinstance(layer, int) else f"named '{layer}'"
            raise RustGISError(f"no layer {what}")
        return found[0]

    def __getitem__(self, name: Union[str, int]) -> Layer:
        """A layer of the active map, by name or id.

        Raises:
            KeyError: When the map has no such layer.
        """
        try:
            return Layer(self, name)
        except RustGISError as e:
            raise KeyError(name) from e

    def __contains__(self, name: object) -> bool:
        """Whether the active map has a layer of this name or id (or this :class:`Layer`)."""
        if isinstance(name, bool) or not isinstance(name, (str, int, Layer)):
            return False
        try:
            self._find_layer(name)
        except RustGISError:
            return False
        return True

    def __iter__(self) -> Iterator[Layer]:
        """The active map's layers, top first."""
        return iter([Layer._of(self, int(entry["id"])) for entry in self.layers()])

    def add_data(self, path: PathLike, name: Optional[str] = None, **params: Any) -> Any:
        """Adds a file as a layer, or as a standalone table when it has no geometry.

        Args:
            path: The file: GeoJSON, CSV, Shapefile (or a zip of them), GeoPackage, GeoParquet,
                FlatGeobuf, KML/KMZ, GPX, GeoTIFF, ASCII grid, and the formats GDAL or DuckDB
                read when they are installed.
            name: The layer's name (the file's name when left out).
            **params: More ``layer.addData`` parameters, e.g. ``layer=`` for one table of a
                GeoPackage, ``x=`` and ``y=`` for a CSV's coordinate columns.

        Returns:
            The new :class:`Layer`, a list of them when the file adds several (the tables
            of a GeoPackage, or a GDAL file's layers), or the command's result for a table.
        """
        command, args = self._data_command(path)
        if name is not None:
            params["name"] = name
        return self._outputs(self.run(command, {**args, **params}))

    def add_geojson(self, data: Union[Dict[str, Any], str, PathLike], name: str = "GeoJSON") -> Any:
        """Adds GeoJSON as a layer.

        Args:
            data: A GeoJSON dict (or any object with ``__geo_interface__``), GeoJSON text, or
                the path of a GeoJSON file.
            name: The layer's name.

        Returns:
            The new :class:`Layer`, or the command's result when the GeoJSON had no geometry
            (it is added as a table).
        """
        if hasattr(data, "__geo_interface__"):
            data = data.__geo_interface__  # type: ignore[union-attr]
        if isinstance(data, dict):
            text = json.dumps(data, default=_jsonable, allow_nan=False)
        elif isinstance(data, str) and data.lstrip().startswith("{"):
            text = data
        else:
            return self.add_data(data, name=name)
        return self._outputs(self.run("layer.addDataText", fileName=f"{name}.geojson", text=text, name=name))

    def add_geopandas(self, gdf: Any, name: str = "GeoDataFrame") -> Any:
        """Adds a ``geopandas.GeoDataFrame`` as a layer (reprojected to EPSG:4326 if needed).

        Args:
            gdf: The GeoDataFrame.
            name: The layer's name.

        Returns:
            The new :class:`Layer`.
        """
        if getattr(gdf, "crs", None) is not None and not gdf.crs.equals("EPSG:4326"):
            gdf = gdf.to_crs("EPSG:4326")
        return self.add_geojson(gdf.to_json(), name=name)

    def add_tiles(self, url: str, name: Optional[str] = None, **params: Any) -> Any:
        """Adds a web tile layer: an XYZ template, a WMS, vector tiles or a MapLibre style.

        Args:
            url: The address, e.g. ``"https://tile.openstreetmap.org/{z}/{x}/{y}.png"``.
            name: The layer's name.
            **params: More ``map.addTiles`` parameters (``kind``, ``attribution``, ...).

        Returns:
            The tile layer's position, kind and name. An archive (PMTiles or MBTiles)
            added without an ``id`` also reports ``zoomed``, whether its bounds framed
            the map by the time the call returned (``False`` while its metadata is
            still loading, which can frame the map later).
        """
        if name is not None:
            params["name"] = name
        return self.run("map.addTiles", url=url, **params)

    def add_pmtiles(
        self,
        url: PathLike,
        name: Optional[str] = None,
        source_layers: Optional[List[str]] = None,
        style: Optional[Union[List[Dict[str, Any]], str]] = None,
        **params: Any,
    ) -> Any:
        """Adds a raster or vector PMTiles archive through ``map.addTiles``.

        Args:
            url: A web address or local file path. Local archives are not embedded in the
                notebook app; use a web address accessible to the browser there.
            name: The tile layer's name (the archive's file name when left out).
            source_layers: Vector source-layer names for the default look; omitted for all.
            style: An array of MapLibre style layers, or its JSON text. Each style layer's
                source is the archive itself, and its ``source-layer`` chooses what it draws
                instead of ``source_layers``. Omit for the default look.
            **params: More ``map.addTiles`` parameters, e.g. ``opacity`` or ``attribution``.

        Returns:
            The tile layer's position, kind and name, as for :meth:`add_tiles`. Without
            an ``id``, also ``zoomed``, indicating whether its bounds framed the map immediately.
        """
        params["kind"] = "pmtiles"
        if source_layers is not None:
            params["layers"] = source_layers
        if style is not None:
            params["styleLayers"] = style
        return self.add_tiles(os.fspath(url), name=name, **params)

    def remove_layer(self, layer: Union[str, int, Layer]) -> None:
        """Removes a layer from the map (``undo`` brings it back).

        Args:
            layer: The layer, its name or its id.
        """
        self.run("layer.remove", layer=self._find_layer(layer)["id"])

    # The view ------------------------------------------------------------------------------

    @property
    def bounds(self) -> Tuple[float, float, float, float]:
        """What the map shows, as ``(west, south, east, north)`` in degrees."""
        return tuple(self.info()["extent"])  # type: ignore[return-value]

    def set_basemap(self, name: str) -> Any:
        """Sets the map's basemap.

        Args:
            name: ``"paper"``, ``"midnight"``, ``"ocean"``, ``"atlas"`` or ``"blank"``.
        """
        return self.run("map.basemap", name=name)

    def set_center(
        self,
        lon: float,
        lat: float,
        scale: Optional[float] = None,
        *,
        bearing: Optional[float] = None,
        pitch: Optional[float] = None,
    ) -> Any:
        """Centres the map on a longitude and latitude.

        Args:
            lon: Longitude in degrees.
            lat: Latitude in degrees.
            scale: The map scale's denominator (``2_000_000`` for 1:2,000,000).
            bearing: The map's rotation, in degrees clockwise from north.
            pitch: The map's tilt, in degrees (0 to 85).
        """
        params: Dict[str, Any] = {"lon": lon, "lat": lat}
        for key, value in (("scale", scale), ("bearing", bearing), ("pitch", pitch)):
            if value is not None:
                params[key] = value
        return self.run("map.goToXY", params)

    def zoom_to(self, target: Union[str, int, Layer, Iterable[Any]]) -> Any:
        """Zooms the map to a layer's features, to several layers', or to a bounding box.

        Args:
            target: A layer (or its name or id), a list of layers (as :meth:`run_tool` returns
                them), or ``(west, south, east, north)`` in degrees.

        Raises:
            RustGISError: When a layer isn't on the map, or the layers are all empty.
            TypeError: When ``target`` is none of these.
        """
        if isinstance(target, (str, int, Layer)) and not isinstance(target, bool):
            return self.run("layer.zoomTo", layer=self._find_layer(target)["id"])
        try:
            items = list(target)  # type: ignore[arg-type]
        except TypeError:
            items = []
        if items and all(isinstance(item, Layer) for item in items):
            extents = (self._find_layer(item).get("extent") for item in items)
            boxes = [box for box in extents if box]
            if not boxes:
                raise RustGISError("the layers have no features to zoom to")
            extent = [
                min(b[0] for b in boxes),
                min(b[1] for b in boxes),
                max(b[2] for b in boxes),
                max(b[3] for b in boxes),
            ]
            return self.run("map.zoomToExtent", extent=extent)
        try:
            box = [float(v) for v in items if not isinstance(v, (bool, str, Layer))]
        except (TypeError, ValueError):
            box = []
        if len(box) != 4 or len(items) != 4:
            raise TypeError("zoom_to takes a layer, a list of layers, or (west, south, east, north)")
        return self.run("map.zoomToExtent", extent=box)


class Session(_Api):
    """A RustGIS project open in memory: its maps, layers, symbology, layouts and undo history.

    Args:
        project: A project file (``.rgis``) to open.
        sample: Open the World Atlas sample project.
        data: Files to add as layers (GeoJSON, CSV, Shapefile, GeoPackage, GeoParquet,
            FlatGeobuf, KML, GPX, GeoTIFF and more).
        plugins: Load the plugins installed for the desktop app first.
        use_saved_secrets: Use the connections the desktop app saved, with the passwords and
            keys it keeps in the system keychain, and its remembered Earthdata Login token
            (read only when a command needs one).
            Parameters given to a command win. Off by default: nothing reads the keychain,
            except for the Earthdata Login token once any session of the process has opted in
            (the token and its store serve every session of the process).
            When the keychain can't be used the session goes on without the saved secret and
            logs one ``WARNING`` to the ``rustgis`` logger; that diagnostic never raises or emits
            a Python warning (a command that needed the secret still raises its own error).

    Example:
        >>> s = rustgis.Session(data=["parcels.gpkg"])
        >>> parcels = s["parcels"]
        >>> parcels.count
    """

    def __init__(
        self,
        project: Optional[PathLike] = None,
        *,
        sample: bool = False,
        data: Optional[Union[PathLike, Iterable[PathLike]]] = None,
        plugins: bool = False,
        use_saved_secrets: bool = False,
    ) -> None:
        """Starts a session, optionally opening a project, the sample and data files."""
        if plugins:
            load_plugins()
        self._engine = _Engine(use_saved_secrets)
        if sample:
            self.run("project.sample")
        if project is not None:
            self.open(project)
        if data is not None:
            for path in [data] if isinstance(data, (str, os.PathLike)) else data:
                self.add_data(path)

    def run(self, command: str, params: Optional[Dict[str, Any]] = None, /, **kwargs: Any) -> Any:
        """Runs a RustGIS command by id.

        Parameters can be given as a dict, as keyword arguments, or both (keyword arguments win).
        ``Session.commands()`` lists every command with its parameters, and
        ``Session.describe(id)`` one of them. The dict is positional only, so a command's own
        ``params`` parameter (``gp.run`` has one) can be a keyword:
        ``s.run("gp.run", tool="buffer", params={"input": "Cities", "distance": 100})``.
        A :class:`Layer` given as a parameter is sent as its name (a layer of the same name
        above it is taken instead; layer commands also take ``layer=`` an id).

        Args:
            command: The command id, e.g. ``"layer.symbology"`` or ``"gp.run"``.
            params: The command's parameters, as a dict (positional only).
            **kwargs: Parameters as keyword arguments.

        Returns:
            The command's result, decoded from JSON (``None`` for commands that return nothing).

        Raises:
            RustGISError: When the command fails; the message says why.
        """
        merged: Dict[str, Any] = dict(params or {})
        merged.update(kwargs)
        try:
            encoded = json.dumps(merged, default=_jsonable, allow_nan=False) if merged else ""
        except ValueError as e:
            raise ValueError(f"{command}: parameters must be finite numbers, text, lists and dicts ({e})") from e
        text = self._engine.run(command, encoded)
        return json.loads(text)

    def notices(self) -> List[Tuple[str, bool]]:
        """What commands have reported since the last call: warnings about a file, a tool's
        summary, and so on.

        Returns:
            ``(text, is_error)`` pairs, oldest first.
        """
        return list(self._engine.take_notices())

    def _layer_geojson(self, layer: int, selected: bool) -> str:
        """A layer as GeoJSON text.

        Args:
            layer: The layer's id.
            selected: Only its selected features.
        """
        return self._engine.layer_geojson(layer, selected)

    def _root(self) -> "Session":
        """The session that holds the project: this one."""
        return self

    # Projects ------------------------------------------------------------------------------

    def open(self, path: PathLike) -> Any:
        """Opens a project file (``.rgis``), replacing the current project.

        Args:
            path: The project file.
        """
        return self.load_project(path)

    def project_json(self) -> str:
        """The project as ``.rgis`` JSON text, as :meth:`save` writes it.

        Returns:
            The JSON text; ``run("project.openText", text=...)`` opens it.
        """
        return self._engine.project_json()

    # Maps ----------------------------------------------------------------------------------

    def to_png(
        self,
        path: Optional[PathLike] = None,
        *,
        width: Optional[int] = None,
        height: Optional[int] = None,
        dpi: Optional[float] = None,
    ) -> bytes:
        """Renders the active map, with its labels, as a PNG.

        Args:
            path: Where to write the PNG; when left out only the bytes are returned.
            width: Width in pixels (the view's own when left out).
            height: Height in pixels.
            dpi: Resolution (96 when left out).

        Returns:
            The PNG bytes.
        """
        png = bytes(
            self._engine.render_png(
                None if width is None else float(width),
                None if height is None else float(height),
                None if dpi is None else float(dpi),
            )
        )
        if path is not None:
            with open(path, "wb") as f:
                f.write(png)
        return png

    def _repr_png_(self) -> bytes:
        """The map as a PNG, for Jupyter."""
        return self.to_png(width=960, height=540)

    def __repr__(self) -> str:
        try:
            names = self.layer_names()
        except RustGISError:
            names = []
        return f"Session(layers={names!r})"


def _jsonable(value: Any) -> Any:
    """Turns values JSON can't encode (paths, layers, numpy numbers) into ones it can."""
    if isinstance(value, Layer):
        return value.name
    if isinstance(value, os.PathLike):
        return os.fspath(value)
    if hasattr(value, "item"):
        return value.item()
    if hasattr(value, "tolist"):
        return value.tolist()
    raise TypeError(f"{type(value).__name__} isn't JSON serialisable")
