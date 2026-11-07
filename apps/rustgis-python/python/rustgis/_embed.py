"""What the app's hosts share: the Jupyter widget (:mod:`rustgis.widget`) and the Qt widget
(:mod:`rustgis.qt`) both frame the RustGIS web app (``?embed``) and keep it in step with a
:class:`~rustgis.Session` through the project.

A host's page runs ``_widget.js``, which passes the project between the app and Python. Python
sends its project whenever it changes and ``{"type": "run", ...}`` for the app's own commands; the
page sends ``{"type": "project", "text": ...}`` after a change in the app and
``{"type": "result", ...}`` for a command's outcome. The protocol between the page and the app is
described in apps/rustgis-web/src/embed.rs.
"""

from __future__ import annotations

import base64
import os
import pathlib
import threading
from typing import Any, Dict, Iterable, List, Optional, Tuple, Union
from urllib.parse import quote

from . import PathLike, RustGISError, Session, _Api

#: The web app the widgets show; ``RUSTGIS_WEB_URL`` points them at another build (a local
#: ``trunk serve``, a self-hosted copy).
DEFAULT_URL = "https://web.rustgis.app/"

#: Files up to this size travel with the project, so the app in the browser shows them too. The
#: whole project, base64 bytes and all (a third larger), crosses the notebook's websocket after
#: each change, and some setups cap a message near 10 MB, so this stays small.
EMBED_LIMIT = 2 << 20

# The extensions `layer.addDataBytes` reads from a file's contents.
_EMBEDDABLE = {
    ".tif", ".tiff", ".asc", ".geojson", ".json", ".csv", ".tsv", ".txt", ".zip", ".kml", ".kmz",
    ".gpx", ".parquet", ".geoparquet", ".fgb", ".gpkg", ".mbtiles", ".xlsx", ".xls", ".ods",
}

#: The page script that frames the app: an anywidget module, also run by the Qt host page.
WIDGET_JS = pathlib.Path(__file__).parent / "_widget.js"


class _Embedded(_Api):
    """A session shown in the web app: commands run in the session, and its project is sent to
    the app after each; the app's project, after a change there, replaces the session's.

    A host calls :meth:`_start` once, passes the page's messages to :meth:`_from_app`, and
    implements :meth:`_send_project` and :meth:`_send_run`.
    """

    #: The project the app has: the session's last one sent, or the app's own text.
    _project: str = ""

    def _start(
        self,
        session: Optional[Session],
        sample: bool,
        data: Optional[Union[PathLike, Iterable[PathLike]]],
    ) -> None:
        """Takes the session to show, opens the sample and the data files, and sends the project.

        Args:
            session: The session to show; a new one when None.
            sample: Open the World Atlas sample project.
            data: Files to add as layers.
        """
        self._session = session if session is not None else Session()
        # Notices waiting to be read. The app's messages may arrive on another thread (Jupyter's).
        self._held: List[Tuple[str, bool]] = []
        self._held_lock = threading.Lock()
        self._sent_keys = 0
        self._sent_notices: List[str] = []
        if sample:
            self._session.run("project.sample")
        if data is not None:
            for path in [data] if isinstance(data, (str, os.PathLike)) else data:
                command, args = _data_args(path)
                self._session.run(command, args)
        self._push()

    def _send_project(self, text: str) -> None:
        """Sends the project to the page, which passes it to the app.

        Args:
            text: The project as JSON text.
        """
        raise NotImplementedError

    def _send_run(self, message: Dict[str, Any]) -> None:
        """Sends ``{"type": "run", "command", "params"}`` to the page.

        Args:
            message: The message.
        """
        raise NotImplementedError

    @property
    def session(self) -> Session:
        """The session the app shows."""
        return self._session

    # Commands ------------------------------------------------------------------------------

    def run(self, command: str, params: Optional[Dict[str, Any]] = None, /, **kwargs: Any) -> Any:
        """Runs a RustGIS command by id, then shows the result in the app.

        Args:
            command: The command id, e.g. ``"layer.symbology"``.
            params: The command's parameters, as a dict (positional only).
            **kwargs: Parameters as keyword arguments.

        Returns:
            The command's result, decoded from JSON.

        Raises:
            RustGISError: When the command fails.
        """
        try:
            return self._session.run(command, params, **kwargs)
        finally:
            self._push()

    def run_in_app(self, command: str, params: Optional[Dict[str, Any]] = None, /, **kwargs: Any) -> None:
        """Runs a command in the app shown, rather than in the session: the commands only the
        app has (``ui.*``: panes, the attribute table, the interface language).

        The command runs once the app has started; if it fails, the error is among
        :meth:`notices`.

        Args:
            command: The command id, e.g. ``"ui.language"``.
            params: The command's parameters, as a dict (positional only).
            **kwargs: Parameters as keyword arguments.
        """
        merged: Dict[str, Any] = dict(params or {})
        merged.update(kwargs)
        self._send_run({"type": "run", "command": command, "params": merged})

    def notices(self) -> List[Tuple[str, bool]]:
        """What the session and the app have reported since the last call.

        Returns:
            ``(text, is_error)`` pairs, oldest first.
        """
        with self._held_lock:
            held, self._held = self._held, []
        return held + self._session.notices()

    def _query(self, command: str, /, **params: Any) -> Any:
        """Runs a command that only reads the project, without showing anything in the app.

        Args:
            command: The command id.
            **params: Its parameters.
        """
        return self._session.run(command, **params)

    def _layer_geojson(self, layer: int, selected: bool) -> str:
        """A layer as GeoJSON text.

        Args:
            layer: The layer's id.
            selected: Only its selected features.
        """
        return self._session._layer_geojson(layer, selected)

    def _root(self) -> Session:
        """The session that holds the project: the one the app shows."""
        return self._session

    def _data_command(self, path: PathLike) -> Tuple[str, Dict[str, Any]]:
        """The command adding a file: a file of up to 2 MB travels with the project, so the app
        shows it too; a larger one is read from its path, which only Python can see: it draws in
        :meth:`to_png` but not in the app.

        Args:
            path: The file.
        """
        return _data_args(path)

    def to_png(
        self,
        path: Optional[PathLike] = None,
        *,
        width: Optional[int] = None,
        height: Optional[int] = None,
        dpi: Optional[float] = None,
    ) -> bytes:
        """Renders the map as a PNG, as the session holds it.

        Args:
            path: Where to write the PNG; when left out only the bytes are returned.
            width: Width in pixels.
            height: Height in pixels.
            dpi: Resolution (96 when left out).

        Returns:
            The PNG bytes.
        """
        return self._session.to_png(path, width=width, height=height, dpi=dpi)

    # Keeping the app and the session in step -----------------------------------------------

    def _push(self) -> None:
        """Sends the session's project to the app, when it changed."""
        try:
            text, keys, notices = self._session._engine._peer_text()
            # A new loss may leave the public project text unchanged.
            if text != self._project or notices != self._sent_notices:
                self._hold([(notice, False) for notice in notices])
            self._sent_notices = notices
            if keys and (text != self._project or keys > self._sent_keys):
                self._hold([(f"{keys} layer address(es) went to the map widget without their keys, tokens, passwords or URL fragments; restore missing values if needed", False)])
            self._sent_keys = keys
        except RustGISError as e:
            self._hold([(f"couldn't show the project in the app: {e}", True)])
            return
        if text != self._project:
            self._project = text
            self._send_project(text)

    def _hold(self, notices: List[Tuple[str, bool]]) -> None:
        """Keeps notices for :meth:`notices`.

        Args:
            notices: ``(text, is_error)`` pairs.
        """
        with self._held_lock:
            self._held.extend(notices)

    def _from_app(self, content: Any) -> None:
        """Takes a message from the page: the app's project after a change there, a command's
        failure.

        Args:
            content: The message.
        """
        if not isinstance(content, dict):
            return
        kind = content.get("type")
        if kind == "project":
            text = content.get("text")
            if not isinstance(text, str) or text == self._project:
                return
            # Peer input is an import: keep its containment notices and invalidate old trust.
            self._hold(self._session.notices())
            try:
                self._session._engine._open_peer_text(text)
            except RustGISError as e:
                self._hold([(f"the app's project couldn't be read: {e}", True)])
                return
            finally:
                self._hold(self._session.notices())
            # The app's own text, not the session's re-serialised copy: the app then sees its
            # project and doesn't open it again.
            self._project = text
        elif kind == "result" and content.get("ok") is False:
            self._hold([(str(content.get("error", "the command failed")), True)])

    def __repr__(self) -> str:
        return f"{type(self).__name__}(layers={self.layer_names()!r})"


def _app_src(url: Optional[str], lang: Optional[str]) -> str:
    """The address of the app a widget frames: ``url`` with ``?embed`` (and ``lang``).

    Args:
        url: The web app's address, or None for ``RUSTGIS_WEB_URL`` or the default.
        lang: The interface language code, or None.
    """
    base = url or os.environ.get("RUSTGIS_WEB_URL") or DEFAULT_URL
    query = "embed" + (f"&lang={quote(lang)}" if lang else "")
    return base + ("&" if "?" in base else "?") + query


def _data_args(path: PathLike) -> Tuple[str, Dict[str, Any]]:
    """The command adding ``path``: its contents when they can travel with the project, else
    its path.

    Args:
        path: The file to add.

    Returns:
        The command id and its parameters.
    """
    p = pathlib.Path(os.fspath(path))
    if p.suffix.lower() in _EMBEDDABLE:
        try:
            if p.is_file() and p.stat().st_size <= EMBED_LIMIT:
                data = base64.b64encode(p.read_bytes()).decode("ascii")
                return "layer.addDataBytes", {"fileName": p.name, "data": data}
        except OSError:
            pass
    return "layer.addData", {"path": os.fspath(path)}
