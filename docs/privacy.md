# Privacy

The RustGIS desktop app and command line collect no analytics, telemetry or usage data.
Local files and projects are processed on your device. Features that use remote services can
send searches, coordinates, data or assistant context to those services, as described below.

## Local troubleshooting history

**Diagnostics** keeps a bounded, in-memory history of messages and failed tile, service and
data requests for the current session, even while its pane is closed (a map tile the server
doesn't have is routine: noted once per server, not listed). It is not telemetry:
nothing is uploaded automatically, written to preferences, or saved in a project. **Copy
diagnostics** explicitly copies the filtered entries; **Clear** removes the history.

Credentials are removed before diagnostic text is retained. URL user information, all query
strings (including signed URLs), fragments, credential fields, and structured payloads are
omitted conservatively. A credential field in the middle of a line is removed when its value is
one word or quoted (`… password: hunter2`), but a value of several words reads as prose and is
kept (`… password: correct horse battery` keeps all three), so check such text before sharing.
After `Basic` (without a colon) at the start of a line, the rest of the text is removed only
when the next word looks like base64: it has a digit or punctuation (a sentence-ending mark
`.,!?;:)` at its end doesn't count), has 16 characters or more, or has 8 or more with a capital
after the first letter. So a very short Basic credential such as `YTpi` is kept.

Notices and request errors redact a known address before attaching its failure reason, so a
token or signature does not hide the HTTP status or other explanation. The complete query and
fragment are omitted, including values with unescaped spaces. For an unknown URL embedded in
free text, a secret-named query or fragment still omits the rest of that text: those later
words might be part of the credential, not an explanation. The same rule applies to copied
history errors, saved run messages and notebook cell output. Ambiguous slash-containing userinfo
is omitted rather than guessed to be a host, port and path. A root address is shown with `/`
before an omission marker or reason, so repeated redaction keeps its host.

Copied run parameters are sanitized as data, without blanket-omitting JSON/XML syntax.
Typed addresses lose their keys and credential fields are emptied, but opaque text and nested
values a tool doesn't describe are kept for replay and flagged with a notice: they can still
hold credentials, so review copied code before sharing it.

Request and response bodies are not captured by network diagnostics.
Background requests carry their originating session's weak capture token; failures are not
broadcast to other embedded or Python sessions. Layer names, file paths and URL paths can
still identify private data, so review copied diagnostics before sharing them.

## Online services

- **Locate › By Place or Address** and place search send your query to OpenStreetMap's Nominatim by default.
  **By Place or Address** and **Geocode Addresses** can also use Komoot's Photon, PDOK's Locatieserver (the
  Netherlands) or a server you specify;
  geocoding sends the address values you choose from your table or layer.
- **Directions** sends the route's start, stops and destination to FOSSGIS's public Valhalla
  server by default. The Directions pane asks for consent before sending places and remembers
  that choice. The `map.route` command can also use a server you specify.
- Web basemaps and tile layers request tiles for the area you view; vector styles can also
  request their tile sources and sprites. These requests reveal the requested map area to the
  servers even when your local layers are not uploaded.
- The identify pop-up can show a picture from a web address in the clicked feature's data (an
  `image_url`, `photo` or similar field, or an address ending in `.jpg`, `.png` or `.webp`: see
  [pop-ups](user-guide/editing.md)). Fetching it tells the server the address names, which the
  data chooses, your IP address and that the picture was viewed, so it could be used to see who
  opens a file. The pop-up fetches a picture as it opens only when the address is
  `https://assets.rustgis.app/…`: exactly that host (in any case), over `https://`, with no port,
  user name or subdomain. On the desktop a redirect that server answers with is followed, as for
  any download; the web app refuses it, as it does every download's. Any other picture is fetched only when you click
  **Show Picture** in the pop-up, which names the server first. Plain `http://` pictures and
  pictures on this computer or the local network are never fetched; on the desktop that includes
  a name, or a redirect, that resolves to one. A picture kept in the data itself (a
  `data:image/…` value) needs no request. A fetched picture is kept in memory for the session, or
  until a sign-out or a login that ends clears the cached web tiles; the web app also uses the
  browser's own cache. Links in the pop-up open only when you click them.
- Web data, remote rasters, cloud storage, database connections and online catalogs contact
  their configured servers. Searches may send keywords, coordinates, an area of interest,
  dates or filters; authenticated connections also send the credentials or tokens required by
  that service. See [Web data](user-guide/web-data.md).

Servers receive ordinary connection information, including your IP address and request headers.
Their own privacy and retention policies apply. Working with local files does not itself
upload their contents to RustGIS.

## AI Assistant and connected agents

When you use the [AI Assistant](user-guide/assistant.md), requests go to the provider and
endpoint you configure, which may be a local model server. They include your messages,
conversation history, map context (including the map centre, layer names, counts and field
names), tool results and any skills the assistant reads. Tools can read feature attributes
and geometry, so those results can become part of the conversation sent to the provider.

Signing in with ChatGPT contacts OpenAI's authentication services and Responses API. Local
agents you connect or let the assistant delegate to use their own model services and tools,
under their own configuration and policies. Approval prompts for file, network and other
restricted actions do not stop ordinary assistant context and tool results from being sent to
the configured model.

## Plugins and downloads

Browsing the Plugin Manager fetches a registry and its blocklist; installing or updating a
plugin downloads its package. The default registry is `plugins.rustgis.app`. The web app also
checks the registry's blocklist at startup and may download packages saved by URL in an earlier
visit. Preparing a Python process plugin can download uv from PyPI, Python and the plugin's
dependencies.

WebAssembly plugins need the `network` permission to make web requests through RustGIS. A
plugin with that permission can send information it can read to addresses it chooses.
Desktop process plugins are not sandboxed and can access files and the network as ordinary
programs. Review the permissions and publisher before installing; see [Plugins](plugins.md).

On the desktop, **Help › Check for Updates** contacts GitHub's latest-release API only when
you ask. It does not upload your project. On Windows, **Download and Install** then downloads
the release's installer and its `SHA256SUMS` from GitHub's release downloads, only when you
choose it. The web app checks its own site for service-worker
updates when it loads and returns to view, and fetches app icons from `assets.rustgis.app`.
It downloads language fonts from that asset server when needed. The web GeoLibre tools
download their runner from jsDelivr when needed; the tool list is cached between visits.

## Website analytics

The websites we host, the documentation at [rustgis.app](https://rustgis.app) and the web app at
[web.rustgis.app](https://web.rustgis.app), use Google Analytics 4 to count page visits and see
which pages are used. Google receives your IP address, the page you are on, the referring page and
general device and browser information, and sets its own cookies in your browser. It is governed
by [Google's privacy policy](https://policies.google.com/privacy).

RustGIS does not send layer contents, files or projects to Google Analytics. The web app's
page-view event reports its address trimmed to the site and path (`https://web.rustgis.app/`),
without the query string or map hash; its referring address is trimmed the same way. The
documentation reports the standard page address, which can include a query string, such as the
words of a site search.

You can block analytics with a content blocker or your browser's tracking protection; the sites
work normally without them. Analytics run only on the sites we host: the measurement ID is added
when those sites are deployed, so the desktop app, the command line and any copy you build
yourself never load them.
