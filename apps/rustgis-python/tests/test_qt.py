"""The Qt widget: its project stays in step with the session both ways, through the host page."""

import json
import os

import pytest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
if os.environ.get("RUSTGIS_TEST_QT") != "1":
    # Skipped where Qt WebEngine isn't installed; CI's Qt job sets RUSTGIS_TEST_QT to require it.
    pytest.importorskip("PySide6.QtWebEngineWidgets")

from rustgis.qt import HOST_URL, MapWidget, _host_page  # noqa: E402  (before the QApplication)
from PySide6.QtCore import QCoreApplication, QEvent  # noqa: E402

from PySide6.QtWidgets import QApplication  # noqa: E402

import rustgis  # noqa: E402

POINT = {
    "type": "FeatureCollection",
    "features": [{"type": "Feature", "properties": {"name": "A"}, "geometry": {"type": "Point", "coordinates": [10, 50]}}],
}

# Nothing listens there: the tests talk to the widget as its host page would.
NOWHERE = "http://127.0.0.1:9/"


@pytest.fixture(scope="session")
def app():
    """The application the widgets need, kept alive until every test's widgets are destroyed.

    WebEngine runs without the GPU. With it, WebEngine's GPU thread sets up EGL (Mesa reads the
    environment with getenv there) while the tests go on, and a test or library that adds an
    environment variable at that moment can crash the process: glibc's setenv may move and free
    the environment array that getenv is reading (#1113). The tests never draw the page.
    """
    return QApplication.instance() or QApplication(["rustgis-tests", "--webEngineArgs", "--disable-gpu"])


@pytest.fixture
def make_widget(app):
    """Destroy native widgets on the GUI thread, without relying on cyclic garbage collection."""
    widgets = []

    def make(*args, **kwargs):
        w = MapWidget(*args, **kwargs)
        widgets.append(w)
        return w

    try:
        yield make
    finally:
        for w in widgets:
            w.close()
            # The bridge has no Qt parent, so deleting the widget does not delete it.
            w._bridge.deleteLater()
            w.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.Type.DeferredDelete)


@pytest.fixture
def widget(make_widget):
    """A widget whose scripts for the page are recorded rather than run."""
    w = make_widget(url=NOWHERE)
    w.scripts = []
    w._js = w.scripts.append
    return w


def from_page(w, message):
    """Delivers a message as the host page would.

    Args:
        w: The widget.
        message: The message, a dict.
    """
    w._on_page(json.dumps(message))


def test_the_host_page_runs_the_widget_script_against_the_app():
    page = _host_page(NOWHERE + "?embed&lang=de")
    assert "new QWebChannel(" in page and "function render(" in page
    assert json.dumps(NOWHERE + "?embed&lang=de") in page
    # The widget script is a string in a <script> element: nothing in it may end the element.
    assert page.count("</script>") == 2
    assert HOST_URL.startswith("https://")


def test_python_changes_wait_for_the_page_then_reach_it(widget):
    widget.add_geojson(POINT, name="Sites")
    widget.run_in_app("ui.language", code="de")
    assert widget.scripts == []
    from_page(widget, {"type": "host"})
    project, run = widget.scripts
    assert project.startswith("window.rustgisHost.project(") and "Sites" in project
    assert run.startswith("window.rustgisHost.custom(") and '"ui.language"' in run
    widget.set_center(10, 50, scale=1_000_000)
    assert len(widget.scripts) == 3 and widget.scripts[2].startswith("window.rustgisHost.project(")


def test_app_changes_reach_the_session(widget):
    from_page(widget, {"type": "host"})
    other = rustgis.Session()
    other.add_geojson(POINT, name="Drawn in the app")
    text = other.project_json()
    from_page(widget, {"type": "project", "text": text})
    assert widget.layer_names() == ["Drawn in the app"]
    # The app's own text is kept, so it isn't sent back to it.
    assert widget._project == text
    assert widget.notices() == []


def test_bad_messages_are_notices_not_crashes(widget):
    widget.add_geojson(POINT, name="Sites")
    from_page(widget, {"type": "project", "text": "not a project"})
    widget._on_page("not json")
    widget._on_page('"junk"')
    from_page(widget, {"type": "result", "ok": False, "error": "unknown command"})
    assert widget.layer_names() == ["Sites"]
    notices = widget.notices()
    assert notices[-1] == ("unknown command", True) and all(error for _, error in notices)


def test_options_and_the_session(make_widget, tmp_path):
    path = tmp_path / "sites.geojson"
    path.write_text(json.dumps(POINT))
    s = rustgis.Session()
    w = make_widget(s, data=[path], lang="de", url=NOWHERE)
    assert w._src == NOWHERE + "?embed&lang=de"
    assert w.session is s and w["sites"] in s
    # A small file travels with the project: the app can't read the path.
    assert str(path) not in w._project
    assert repr(w) == "MapWidget(layers=['sites'])"


def test_a_reload_holds_messages_until_the_page_starts_again(widget):
    from_page(widget, {"type": "host"})
    widget.scripts.clear()
    widget._unready()  # what loadStarted calls
    widget.run_in_app("ui.theme", dark=True)
    widget.add_geojson(POINT, name="Sites")
    assert widget.scripts == []
    from_page(widget, {"type": "host"})
    project, run = widget.scripts
    assert "Sites" in project and '"ui.theme"' in run
