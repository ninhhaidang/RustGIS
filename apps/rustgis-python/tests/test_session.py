"""Tests for the rustgis Python package (run after `maturin develop`)."""

import json
import shutil
import subprocess

import pytest

import rustgis


POINTS = {"type": "FeatureCollection", "features": [
    {"type": "Feature", "geometry": {"type": "Point", "coordinates": [114.17, 22.3]}, "properties": {"name": "Hong Kong"}},
    {"type": "Feature", "geometry": {"type": "Point", "coordinates": [113.26, 23.13]}, "properties": {"name": "Guangzhou"}},
]}


@pytest.fixture
def sample():
    """A session with the World Atlas sample open."""
    return rustgis.Session(sample=True)


def test_version_matches_the_engine():
    assert rustgis.__version__.count(".") >= 2


def test_use_saved_secrets_loads_the_desktop_connections(tmp_path, monkeypatch):
    monkeypatch.setenv("RUSTGIS_CONFIG_DIR", str(tmp_path))
    connections = json.dumps({"next": 2, "connections": [{
        "id": "c1", "kind": "postgres", "name": "Work",
        "address": "postgres:host=127.0.0.1 port=1 user=u dbname=gis",
        "remembered": True,
    }]})
    (tmp_path / "prefs.json").write_text(json.dumps({"connections": connections}))
    assert rustgis.Session(use_saved_secrets=True).run("connection.list")["connections"][0]["name"] == "Work"
    assert rustgis.Session().run("connection.list")["connections"] == []


def test_sample_layers(sample):
    names = [layer["name"] for layer in sample.layers()]
    assert {"Cities", "Countries"} <= set(names)
    assert sample.layer("Cities").info["features"] > 200


def test_commands_and_tools_are_listed(sample):
    ids = {c["id"] for c in sample.commands()}
    assert {"layer.addData", "gp.run", "project.saveAs"} <= ids
    assert "buffer" in json.dumps(sample.tools())


def test_run_takes_keywords_including_a_params_keyword(sample):
    result = sample.run("gp.run", tool="buffer", params={"input": "Cities", "distance": 100})
    assert "layer" in result
    assert any("Buffer" in text for text, _ in sample.notices())
    assert sample.notices() == []
    # A dict works too, and keywords win over it.
    sample.run("layer.transparency", {"layer": "Countries", "value": 10}, value=40)
    assert any(l["name"] == "Countries" for l in sample.layers())


def test_errors_raise_rustgis_error(sample):
    with pytest.raises(rustgis.RustGISError, match="unknown command"):
        sample.run("no.such.command")
    with pytest.raises(rustgis.RustGISError):
        sample.layer("No such layer").to_geojson()


def test_parameters_must_be_json(sample):
    with pytest.raises(ValueError, match="finite"):
        sample.run("map.setScale", scale=float("nan"))


def test_geojson_round_trip(sample):
    collection = sample.layer("Cities").to_geojson()
    assert collection["type"] == "FeatureCollection" and len(collection["features"]) > 200
    big = {"type": "FeatureCollection", "features": [f for f in collection["features"] if f["properties"]["pop_max"] > 10_000_000]}
    sample.add_geojson(big, name="Megacities")
    assert sample.layer("Megacities").info["features"] == len(big["features"])
    sample.undo()
    assert "Megacities" not in [l["name"] for l in sample.layers()]


def test_png(sample, tmp_path):
    out = tmp_path / "map.png"
    png = sample.to_png(out, width=400, height=200)
    assert png[:4] == b"\x89PNG" and out.read_bytes() == png
    assert sample._repr_png_()[:4] == b"\x89PNG"


def test_projects_save_and_open(sample, tmp_path):
    path = tmp_path / "atlas.rgis"
    sample.run("gp.run", tool="buffer", params={"input": "Cities", "distance": 50})
    sample.save(path)
    reopened = rustgis.Session(path)
    assert [l["name"] for l in reopened.layers()] == [l["name"] for l in sample.layers()]


def test_data_files(tmp_path):
    path = tmp_path / "points.geojson"
    path.write_text(json.dumps({"type": "FeatureCollection", "features": [
        {"type": "Feature", "geometry": {"type": "Point", "coordinates": [114.17, 22.3]}, "properties": {"name": "Hong Kong"}},
        {"type": "Feature", "geometry": {"type": "Point", "coordinates": [113.26, 23.13]}, "properties": {"name": "Guangzhou"}},
    ]}))
    s = rustgis.Session(data=path)
    assert s.layers()[0]["features"] == 2


@pytest.mark.parametrize("as_text", [False, True])
def test_pmtiles_options_survive_a_project_round_trip(as_text):
    """The Python names reach the engine, including JSON-text styles and extensionless URLs."""
    style = [{"id": "water", "type": "fill", "source-layer": "water", "paint": {"fill-color": "#336699"}}]
    s = rustgis.Session()
    result = s.add_pmtiles(
        "http://127.0.0.1:9/archive", name="Water", source_layers=["water"],
        style=json.dumps(style) if as_text else style, opacity=0.5,
    )
    assert result == {"name": "Water", "kind": "pmtiles", "index": 0, "zoomed": False}
    other = rustgis.Session()
    other.run("project.openText", text=s.project_json())
    tile = json.loads(other.project_json())["maps"][0]["tiles"][0]
    assert tile["url"] == "http://127.0.0.1:9/archive"
    assert tile["layers"] == ["water"] and tile["opacity"] == 0.5
    assert json.loads(tile["styleLayers"]) == style


def test_pmtiles_local_path_defaults_and_undo(tmp_path):
    """A generated, tile-less archive works as a Path without a server or committed fixture."""
    import struct

    path = tmp_path / "Iceland.pmtiles"
    header = b"PMTiles\x03" + struct.pack("<11Q", 127, 1, 128, 0, 128, 0, 128, 0, 0, 0, 0)
    header += bytes([1, 1, 1, 1, 0, 6])
    header += struct.pack("<4i", -245000000, 633000000, -135000000, 666000000)
    path.write_bytes(header + bytes(10))
    s = rustgis.Session()
    assert s.add_pmtiles(path) == {"name": "Iceland", "kind": "pmtiles", "index": 0, "zoomed": True}
    tile = json.loads(s.project_json())["maps"][0]["tiles"][0]
    assert tile["url"] == str(path) and "layers" not in tile and "styleLayers" not in tile
    assert s.run("map.zoomToTiles", name="Iceland")["bounds"] == [-24.5, 63.3, -13.5, 66.6]
    s.undo()
    assert s.run("map.tiles") == []


def test_static_render_applies_a_pending_archive_view(tmp_path):
    """Rendering applies ready metadata without needing an intervening engine command."""
    import sqlite3
    import subprocess
    import sys
    import time

    path = tmp_path / "pending.mbtiles"
    with sqlite3.connect(path) as database:
        database.executescript(
            "CREATE TABLE metadata (name TEXT, value TEXT);"
            "CREATE TABLE tiles (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_data BLOB);"
            "INSERT INTO metadata VALUES ('format','png'),('minzoom','0'),('maxzoom','0'),"
            "('bounds','-24.5,63.3,-13.5,66.6');"
        )
    # Hold the lock in another process: Python and the native extension use separate SQLite libraries.
    locker = subprocess.Popen(
        [sys.executable, "-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); "
         "c.execute('BEGIN EXCLUSIVE'); print('locked', flush=True); sys.stdin.readline(); c.rollback()", str(path)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
    )
    try:
        assert locker.stdout.readline().strip() == "locked"
        s = rustgis.Session()
        assert s.add_tiles(str(path), kind="mbtiles")["zoomed"] is False
        before = json.loads(s.project_json())["maps"][0]["camera"]
    finally:
        locker.communicate("\n", timeout=5)
    assert locker.returncode == 0
    deadline = time.monotonic() + 5
    while True:
        assert s.to_png(width=64, height=64).startswith(b"\x89PNG")
        # project_json serializes state directly, without running a command that could frame it.
        after = json.loads(s.project_json())["maps"][0]["camera"]
        if after != before:
            break
        assert time.monotonic() < deadline, "static renders never applied the ready archive view"
        time.sleep(0.01)


@pytest.mark.parametrize("style", [{"not": "a style array"}, "not JSON"])
def test_pmtiles_keeps_the_engines_style_validation(style):
    """Bad styles raise the usual command error and leave the project unchanged."""
    s = rustgis.Session()
    with pytest.raises(rustgis.RustGISError, match="style"):
        s.add_pmtiles("http://127.0.0.1:9/archive", style=style)
    assert s.run("map.tiles") == []


def test_geopandas_round_trip(sample):
    gpd = pytest.importorskip("geopandas")
    gdf = sample.layer("Cities").to_geopandas()
    assert isinstance(gdf, gpd.GeoDataFrame) and gdf.crs.to_epsg() == 4326
    sample.add_geopandas(gdf.head(5), name="Five")
    assert sample.layer("Five").info["features"] == 5


def test_a_session_is_usable_from_another_thread():
    """Jupyter delivers widget messages on a thread of its own."""
    import threading

    s = rustgis.Session(sample=True)
    seen = []
    t = threading.Thread(target=lambda: seen.append(len(s.layers())))
    t.start()
    t.join()
    assert seen == [len(s.layers())]


def test_project_json_reopens_as_the_same_project():
    s = rustgis.Session(sample=True)
    other = rustgis.Session()
    other.run("project.openText", text=s.project_json())
    assert [layer["name"] for layer in other.layers()] == [layer["name"] for layer in s.layers()]


def test_layers_by_name_and_id(sample):
    cities = sample["Cities"]
    assert cities == sample.layer(cities.id) == sample["cities"]
    assert cities.type == "Point" and cities.count > 200 and "pop_max" in cities.fields
    west, south, east, north = cities.bounds
    assert west < east and south < north
    assert "Cities" in sample and cities in sample and "No such layer" not in sample
    assert [layer.name for layer in sample] == sample.layer_names()
    with pytest.raises(KeyError):
        sample["No such layer"]


def test_a_layer_is_the_same_after_a_rename(sample):
    cities = sample["Cities"]
    cities.rename("Towns")
    assert cities.name == "Towns" and "Cities" not in sample
    cities.visible = False
    assert not cities.visible and not sample["Towns"].info["visible"]
    cities.remove()
    assert "Towns" not in sample
    with pytest.raises(rustgis.RustGISError):
        cities.name
    assert "removed" in repr(cities)


def test_styling_filtering_and_selection(sample):
    countries = sample["Countries"].style("graduated", field="pop_est", classes=5)
    assert "Graduated" in countries.info["renderer"]
    cities = sample["Cities"]
    n = cities.select("pop_max > 10000000")
    assert n == cities.selected > 0
    selected = cities.to_geojson(selected=True)["features"]
    assert len(selected) == n and all(f["properties"]["pop_max"] > 10_000_000 for f in selected)
    cities.clear_selection()
    assert cities.selected == 0 and cities.to_geojson(selected=True)["features"] == []
    cities.filter("pop_max > 10000000")
    assert cities.info["definitionQuery"]
    cities.filter(None)
    assert not cities.info["definitionQuery"]


def test_run_tool_returns_its_layer(sample):
    buffers = sample.run_tool("buffer", input=sample["Cities"], distance=100)
    assert isinstance(buffers, rustgis.Layer) and buffers.type == "Polygon"
    assert buffers.count == sample["Cities"].count
    assert sample.describe("buffer")["name"] == "Buffer"
    assert "params" in sample.describe("layer.symbology")
    with pytest.raises(KeyError):
        sample.describe("no.such.thing")


def test_adding_data_returns_the_layer(sample, tmp_path):
    sites = sample.add_geojson(POINTS, name="Sites")
    assert isinstance(sites, rustgis.Layer) and sites.name == "Sites" and sites.count == 2
    out = tmp_path / "sites.gpkg"
    sites.to_file(out)
    again = sample.add_data(out, name="Again")
    assert again.count == 2 and again.name == "Again"
    sample.remove_layer("Again")
    assert "Again" not in sample


def test_adding_a_file_of_several_layers_returns_them_all(sample, tmp_path):
    sites = sample.add_geojson(POINTS, name="Sites")
    more = sample.add_geojson({"type": "FeatureCollection", "features": POINTS["features"][:1]}, name="More")
    out = tmp_path / "two.gpkg"
    sites.to_file(out)
    more.to_file(out, mode="addTable", table_name="more_sites")
    added = sample.add_data(out)
    assert isinstance(added, list)
    assert len(added) == 2
    assert all(isinstance(layer, rustgis.Layer) for layer in added)
    assert len({layer.id for layer in added}) == 2
    assert sorted(layer.count for layer in added) == [1, 2]
    # One table of it is a single layer.
    one = sample.add_data(out, layer="more_sites")
    assert isinstance(one, rustgis.Layer)
    assert one.count == 1


def test_layers_added_with_tables_are_all_returned(sample):
    # Standalone tables read with the layers (or a count of tables left out) don't hide them.
    ids = [layer["id"] for layer in sample.layers()][:2]
    for tables in (["notes"], 40):
        added = sample._outputs({"layer": ids[0], "layers": ids, "tables": tables})
        assert [layer.id for layer in added] == ids
    one = sample._outputs({"layers": ids[:1], "tables": []})
    assert isinstance(one, rustgis.Layer)
    assert one.id == ids[0]
    assert sample._outputs({"tables": ["notes"]}) == {"tables": ["notes"]}
    # Entries that aren't layer ids are left out before counting.
    one = sample._outputs({"layers": [ids[0], "x", True, None]})
    assert isinstance(one, rustgis.Layer)
    assert one.id == ids[0]
    one = sample._outputs({"layer": ids[1], "layers": ["x", True]})
    assert isinstance(one, rustgis.Layer)
    assert one.id == ids[1]


def _gdal_reads(driver):
    """Whether the GDAL programs the engine runs are on the PATH and read ``driver``."""
    ogrinfo = shutil.which("ogrinfo")
    if ogrinfo is None or shutil.which("ogr2ogr") is None:
        return False
    try:
        formats = subprocess.run([ogrinfo, "--formats"], capture_output=True, text=True, timeout=60).stdout
    except (OSError, subprocess.SubprocessError):
        return False
    return any(line.strip().startswith(f"{driver} ") for line in formats.splitlines())


@pytest.mark.skipif(not _gdal_reads("GeoJSONSeq"), reason="needs GDAL with GeoJSONSeq")
def test_adding_a_gdal_file_of_several_layers_returns_them_all(sample, tmp_path):
    # GeoJSON Sequence is read through GDAL; points and a line in one file are two layers.
    line = {"type": "Feature", "geometry": {"type": "LineString", "coordinates": [[0, 0], [1, 1]]}, "properties": {}}
    path = tmp_path / "mixed.geojsonl"
    path.write_text("\n".join(json.dumps(f) for f in [*POINTS["features"], line]) + "\n")
    added = sample.add_data(path)
    assert isinstance(added, list)
    assert len({layer.id for layer in added}) == 2
    assert sorted(layer.count for layer in added) == [1, 2]


def test_the_view(sample):
    sample.zoom_to(sample["Cities"])
    # Two layers that each reach past the other (Countries reaches the poles, past Web Mercator).
    cities, rivers = sample["Cities"], sample["Rivers"]
    assert cities.bounds[1] < rivers.bounds[1] and rivers.bounds[3] > cities.bounds[3]
    sample.zoom_to([cities, rivers])
    west, south, east, north = sample.bounds
    for w, s, e, n in (cities.bounds, rivers.bounds):
        assert west <= w and south <= s and east >= e and north >= n
    with pytest.raises(TypeError):
        sample.zoom_to(True)
    with pytest.raises(TypeError):
        sample.zoom_to((0, 0))
    sample.zoom_to(v for v in (0, 0, 10, 10))
    with pytest.raises(TypeError):
        sample.zoom_to((0, 0, 10, "north"))
    sample.zoom_to((0, 0, 10, 10))
    west, south, east, north = sample.bounds
    assert west <= 0 and east >= 10 and south <= 0 and north >= 10
    sample.set_center(114.17, 22.3, scale=1_000_000)
    assert abs(sample.info()["center"]["lon"] - 114.17) < 0.01
    sample.set_basemap("paper")
    assert sample.info()["basemap"] == "paper"



def test_a_layer_named_like_another_layers_id_reads_its_own_features(sample):
    cities = sample["Cities"]
    decoy = sample.add_geojson(POINTS, name=str(cities.id))
    assert len(cities.to_geojson()["features"]) == cities.count != decoy.count
    assert decoy.to_geojson()["features"][0]["properties"]["name"] == "Hong Kong"
    assert True not in sample


def test_a_layer_read_in_place_exports_its_whole_source(tmp_path):
    session = rustgis.Session()
    far = {"type": "Feature", "geometry": {"type": "Point", "coordinates": [-70.0, -30.0]}, "properties": {"name": "Far"}}
    path = tmp_path / "streamed.geojson"
    path.write_text(json.dumps({"type": "FeatureCollection", "features": POINTS["features"] + [far]}))
    layer = session.add_data(path, stream=True)
    # The view holds Hong Kong and Guangzhou, not Far.
    session.zoom_to((113, 22, 115, 24))
    session.to_png(width=64, height=64)
    assert {f["properties"]["name"] for f in layer.to_geojson()["features"]} == {"Hong Kong", "Guangzhou", "Far"}
    try:
        import geopandas  # noqa: F401
    except ImportError:
        pass
    else:
        assert len(layer.to_geopandas()) == 3
    assert not any("couldn't be read" in text for text, _ in session.notices())
    # A row that can't be read is left out and said.
    bad = {"type": "Feature", "geometry": {"type": "Point", "coordinates": "x"}, "properties": {"name": "Bad"}}
    path = tmp_path / "bad.geojson"
    path.write_text(json.dumps({"type": "FeatureCollection", "features": POINTS["features"] + [bad]}))
    layer = session.add_data(path, stream=True)
    session.notices()
    assert len(layer.to_geojson()["features"]) == 2
    assert [text for text, _ in session.notices() if "1 row couldn't be read and was left out" in text]


def test_a_layer_of_another_session_is_refused(sample):
    other = rustgis.Session(sample=True)
    stranger = other["Cities"]
    assert stranger not in sample
    with pytest.raises(rustgis.RustGISError, match="another session"):
        sample.remove_layer(stranger)
    assert "Cities" in sample


def test_threads_set_the_count_for_the_process():
    try:
        s = rustgis.Session(threads=2)
        t = s.run("gp.threads")
        assert t["threads"] == 2
        assert t["max"] == min(2, t["cores"])
        # Every session of the process shares it.
        assert rustgis.Session().run("gp.threads")["threads"] == 2
        with pytest.raises(rustgis.RustGISError, match="1 to 1024"):
            rustgis.Session(threads=0)
        # A session that fails to start leaves the setting as it was.
        with pytest.raises(rustgis.RustGISError):
            rustgis.Session("/nonexistent/project.rgis", threads=3)
        assert rustgis.Session().run("gp.threads")["threads"] == 2
    finally:
        rustgis.Session().run("gp.threads", threads=None)
    assert rustgis.Session().run("gp.threads")["threads"] is None
