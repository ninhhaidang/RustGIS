"""The Jupyter widget: its project stays in step with the session both ways."""

import json

import pytest

pytest.importorskip("anywidget")

import rustgis  # noqa: E402

POINT = {
    "type": "FeatureCollection",
    "features": [{"type": "Feature", "properties": {"name": "A"}, "geometry": {"type": "Point", "coordinates": [10, 50]}}],
}


def from_app(m, text):
    """Delivers a message as the app's frontend would.

    Args:
        m: The widget.
        text: The project text the app sends.
    """
    m._on_app(m, {"type": "project", "text": text}, [])


def test_python_changes_reach_the_app():
    m = rustgis.Map()
    assert m._src.endswith("?embed")
    m.add_geojson(POINT, name="Sites")
    project = json.loads(m._project)
    assert [layer["name"] for layer in project["maps"][project["active_map"]]["layers"]] == ["Sites"]
    before = m._project
    m.set_center(10, 50, scale=1_000_000)
    assert m._project != before


def test_pmtiles_changes_reach_the_app():
    """The shared helper sends the archive and source layers to the notebook app."""
    m = rustgis.Map()
    result = m.add_pmtiles("http://127.0.0.1:9/archive", name="Water", source_layers=["water"])
    project = json.loads(m._project)
    tile = project["maps"][project["active_map"]]["tiles"][0]
    assert result["kind"] == tile["kind"] == "pmtiles"
    assert tile["name"] == "Water" and tile["layers"] == ["water"]


def test_app_changes_reach_the_session():
    m = rustgis.Map()
    other = rustgis.Session()
    other.add_geojson(POINT, name="Drawn in the app")
    text = other.project_json()
    from_app(m, text)
    assert [layer["name"] for layer in m.layers()] == ["Drawn in the app"]
    # The app's own text is kept, so it isn't sent back to it.
    assert m._project == text
    # Opening it isn't reported.
    assert m.notices() == []


def test_a_bad_project_from_the_app_is_a_notice_not_a_crash():
    m = rustgis.Map()
    m.add_geojson(POINT, name="Sites")
    from_app(m, "not a project")
    assert [layer["name"] for layer in m.layers()] == ["Sites"]
    assert any(error for _, error in m.notices())
    m._on_app(m, {"type": "result", "ok": False, "error": "unknown command"}, [])
    m._on_app(m, "junk", [])
    assert m.notices() == [("unknown command", True)]


def test_options_and_local_files(tmp_path):
    path = tmp_path / "sites.geojson"
    path.write_text(json.dumps(POINT))
    m = rustgis.Map(data=[path], lang="de", url="http://127.0.0.1:8772/", height="400px")
    assert m._src == "http://127.0.0.1:8772/?embed&lang=de"
    assert m.height == "400px"
    # A small file travels with the project: the app in the browser can't read the path.
    assert str(path) not in m._project
    assert m.layers()[0]["features"] == 1


def test_add_data_parameters_can_override_the_file_name(tmp_path):
    path = tmp_path / "sites.geojson"
    path.write_text(json.dumps(POINT))
    m = rustgis.Map()
    m.add_data(path, name="Renamed", fileName="other.geojson")
    assert [layer["name"] for layer in m.layers()] == ["Renamed"]


def test_layers_of_a_map_change_the_app():
    m = rustgis.Map()
    sites = m.add_geojson(POINT, name="Sites")
    assert sites.session is m and m["Sites"] == sites
    # The Map and its session hold one project: a layer of either is good for both.
    assert sites in m.session and m.session["Sites"] in m and m.session["Sites"] == sites
    before = m._project
    sites.rename("Places")
    assert m._project != before and "Places" in m._project
    assert m.layer_names() == ["Places"]


def test_widget_exchange_never_sends_keys_and_checks_the_current_slot():
    """The real receiving method keeps only keys still present on the last pushed slot."""
    secret = "RGSYNTH_widget_20261103"
    s = rustgis.Session()
    s.add_geojson(POINT, name="Sites")
    project = json.loads(s.project_json())
    project["maps"][0]["layers"][0]["dataset"]["source"] = f"https://h.invalid/a?token={secret}"
    s.run("project.openText", text=json.dumps(project))
    saved = s.project_json()
    assert secret not in saved
    other = rustgis.Session()
    other.run("project.openText", text=saved)
    assert other._engine._peer_text()[1] == 1
    m = rustgis.Map(session=s)
    assert secret not in m._project
    assert any("without their keys" in notice for notice, _ in m.notices())
    before = m._project
    m.run("info.project")
    assert m._project == before
    assert m.notices() == []
    # A different protected source still has one omitted address and needs a fresh warning.
    project = json.loads(m._project)
    project["maps"][0]["layers"][0]["dataset"]["source"] = f"https://h.invalid/b?token={secret}"
    s.run("project.openText", text=json.dumps(project))
    m.run("info.project")
    assert any("without their keys" in notice for notice, _ in m.notices())
    m.run("info.project")
    assert m.notices() == []
    incoming = json.loads(m._project)
    incoming["maps"][0]["layers"][0]["name"] = "Styled by app"
    from_app(m, json.dumps(incoming))
    assert m.session._engine._peer_text()[1] == 1
    assert not any("lost their keys" in notice for notice, _ in m.notices())
    # Direct session changes do not call the widget's push. Its stale record cannot resurrect
    # the removed source when the app sends the old slot again.
    m.session.run("layer.remove", layer="Styled by app")
    incoming["maps"][0]["layers"][0]["name"] = "Stale app slot"
    from_app(m, json.dumps(incoming))
    assert m.session._engine._peer_text()[1] == 0

    for command, params, kind in [
        ("connection.login", {"apiKey": "demo"}, "executable step(s)"),
        ("plugin.unknown", {"nested": {"token": "demo"}}, "opaque parameters"),
    ]:
        s = rustgis.Session()
        project = json.loads(s.project_json())
        clean = {"apiKey": ""} if command == "connection.login" else {"nested": {}}
        project["tasks"] = [{"name": "Notice fixture", "steps": [{"title": "Stored only", "command": command, "params": clean}]}]
        s.run("project.openText", text=json.dumps(project))
        s.notices()
        m = rustgis.Map(session=s)
        assert m.notices() == [], "clean bridge output needs no loss notice"
        before = m._project
        project["tasks"][0]["steps"][0]["params"] = params
        s.run("project.openText", text=json.dumps(project))
        s.notices()
        m.run("info.project")
        if command == "connection.login":
            assert m._project == before, "a newly removed credential need not change public project text"
        notices = m.notices()
        assert len(notices) == 1 and kind in notices[0][0] and not notices[0][1]
        before = m._project
        m.run("info.project")
        m.run("info.project")
        assert m._project == before
        assert m.notices() == [], "unchanged internal pushes must not repeat bridge notices"
        m.run("layer.template", template="points", name="Changed output")
        assert m._project != before
        assert m.notices() == notices, "changed outgoing content reports its losses again"
        m.run("info.project")
        assert m.notices() == []


def test_widget_repeated_table_id_cannot_gain_a_retained_key():
    """The receiving census sees duplicate table ids before project normalization renumbers them."""
    s = rustgis.Session()
    project = json.loads(s.project_json())
    project["tables"] = [{"id":7, "name":"Private table", "geometry_type":"None", "fields":[], "features":[], "source":"https://h.invalid/table?token=RGSYNTH_table_20261103"}]
    s.run("project.openText", text=json.dumps(project))
    m = rustgis.Map(session=s)
    incoming = json.loads(m._project)
    incoming["tables"].append(dict(incoming["tables"][0]))
    from_app(m, json.dumps(incoming))
    assert s._engine._peer_text()[1] == 0
    assert any("lost their keys" in notice for notice, _ in m.notices())
