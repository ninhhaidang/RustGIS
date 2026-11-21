# Field Work

RustGIS runs on Android phones and tablets and on iPhones and iPads, for collecting and editing
data away from a desk. A map is prepared on the computer, carried to the phone as a field package
with its basemap for working offline, edited there with the device's position and camera, and
brought back into the project it came from.

!!! note "Getting the app"
    Get RustGIS from [Google Play](https://play.google.com/store/apps/details?id=org.rustgis.mobile) on Android phones and tablets, or
    from the [App Store](https://apps.apple.com/app/rustgis/id6834987896) on iPhones and iPads. Each
    [release](https://github.com/opengeos/RustGIS/releases/latest) from v0.7.0 on also carries
    signed Android APKs (`RustGIS-<version>-android-arm64.apk` for phones and tablets). To build it yourself, see
    [`apps/rustgis-mobile`](https://github.com/opengeos/RustGIS/tree/main/apps/rustgis-mobile).

## The touch interface

A phone opens in the touch interface: the map, with a bar of four large buttons under it.

| Button | What it does |
| --- | --- |
| **Layers** | Shows and hides layers, zooms to one, and makes one the active layer |
| **Add Data** | Opens files with the system's picker (Android's Files, iOS's document picker) |
| **Capture** | Adds and edits features at the cross in the middle of the map |
| **More** | Full Extent, the project, Undo and Redo, the position, tracks and the rest |

Each button opens a sheet that rises over the map. **Use the Desktop Interface**, in the More
sheet, brings back the ribbon and panes; **Touch Interface**, on the View tab (or Use the Touch
Interface in Command Search), returns to the phone's interface.

## Opening data

**Add Data** and **Open Project** use the system's own picker. Several files can be picked at
once, which a Shapefile needs (its `.shp`, `.dbf`, `.shx` and `.prj`). On an iPhone, documents in
iCloud Drive are downloaded first.

The app copies each file picked into its own storage and works on the copy: edits are saved to the
copy, not to the original, and a project saved on the phone names its data by the copies. Copies
are never removed by themselves. **Remove Unused Copies**, in the More sheet, finds the copies the
project in the window doesn't use, says how many there are and how much room they take, and
removes them once you agree. A copy still being made is kept.

!!! warning
    Remove Unused Copies looks only at the project in the window. A copy that another project
    saved on the phone uses counts as unused, and removing it breaks that project. Open the project
    whose data you want to keep before removing copies, or keep the copies.

Formats read through GDAL, DuckDB databases and the GeoLibre tools need programs a phone can't
run: the app says so where they would appear. Convert such data on a computer first. Database
connections (PostGIS, MySQL, SQL Server) work on a phone too, as they need no program.

## Capturing features

**Capture** adds features to one of the map's point, line or polygon layers. A cross marks the
middle of the map: move the map under it, then fill in the sheet's fields.

- For a point layer, **Add Point Here** puts the point at the cross.
- For a line or polygon layer, **Add Vertex Here** places a vertex at the cross. The shape so far
  is drawn over the map. **Undo Vertex** and **Start Over** take vertices back, and **Finish Line**
  or **Finish Polygon** adds the feature once it has two or three vertices.

The sheet stays open, emptied, for the next feature. Editing is turned on for the map if it was
off.

### The form

The form has a field for each of the layer's attributes, and follows the layer's data design
(**Data › Data Design** on the computer):

- A field with a list of values (a coded-value domain, subtypes, or yes and no) shows a chip for
  each value. Tap one to choose it, and tap it again to leave the field empty.
- A field with a range shows its limits.
- When the layer has feature templates, a **Template** row chooses one, and its defaults fill
  what the form leaves empty.
- A field a constraint rule requires is marked with `*`. A feature with it empty is not added, and
  the sheet names the fields to fill in.
- A field in a contingent value set offers only the values that fit the set's other fields.
- A date field has a calendar button, with **Today** and, for a date and time, **Now**. Both are
  in UTC.

While you type in a field, the sheet stays above the soft keyboard.

### Photos

**Take Photo**, over the form, opens the camera. Each photo taken shows as a chip (tap it to take
it off), and the next feature added lists them in its layer's `photos` text field, separated by
`;`. A layer without one gets one when its first photo is attached, and the sheet says so. To add
a photo to a feature already on the map, pick it in **Edit** mode and tap **Add Photo**.

### Snapping

**Snap** turns the map's snapping on and off (its settings are the computer's: **Edit ›
Snapping**). With it on, the cross snaps to a vertex, a line's end or an edge within reach,
marked with a green ring, when adding and editing alike.

## Editing features

**Edit**, beside **Add** at the top of the Capture sheet, changes shapes already on the map, again
at the cross:

- **Pick Feature at Cross** picks the feature under the cross and draws its vertices.
- For a line or polygon, **Pick Vertex at Cross** picks the nearest vertex. Then **Move Here**
  moves it to the cross, **Add After** adds a vertex there after it, and **Delete Vertex** removes
  it.
- For a point, **Move Point Here** moves it.
- **Delete Feature** deletes the picked feature, and **Done** lets it go.

Each change is one edit, which **Undo** in the More sheet takes back. Changing an existing
feature's attributes on the phone is not supported yet.

## The device's position

**Show My Position**, in the More sheet, shows where the device is: a dot in a ring as wide as the
position's accuracy, following the device about once a second. The system asks once for leave to
use the location. **Go to My Position** (in the More sheet, and at the top of the Capture sheet)
moves the map so the cross is on the position, so a point or vertex added next is where you
stand. **Hide My Position** stops it and spares the battery.

**Average Position**, in the Capture sheet, averages the positions that come in while it runs,
giving the more accurate ones more weight, and says how many it has and how far they spread.
**Use Average** puts the cross on the average, which can place a point more precisely than a
single position.

### Tracks

**Record a Track**, in the More sheet while the position is shown, draws where the device goes,
with a bar that counts its positions and a **Stop** button. Stopping adds the track as one line to
the map's **Tracks** layer (made if the map has none), named for when it started, with its count
of positions and its length in minutes. Positions less exact than 50 m are left out, and the
notice says how many.

Recording goes on while the app is out of sight or the screen is off. Android shows a notice,
"RustGIS is recording a track"; iOS shows its indicator that an app is using the location. The
track is kept in memory until it is stopped: if the app is closed or the system ends it, the track
is lost.

## Offline basemaps

A phone in the field often has no network. Basemaps from MBTiles and PMTiles archives draw with
none: adding an `.mbtiles` or `.pmtiles` file with **Add Data** adds it as a tile layer, and a
field package carries the archives its map draws from. Tile layers from the web show only online.

## Field packages

A field package (`.rgisfield`) carries a map to the phone and its edits back.

1. **On the computer**, with the map in view, **Data › Offline › Package for the Field** makes
   the package: a project of that map, the features in view, and the MBTiles and PMTiles
   archives its tile layers use. The package records the area it was made from in your project,
   so save the project after packaging. A layer read in place from a supported local file is
   copied within the area while it fits the package budget. The copy is for reference, and
   Bring Back skips edits to it. The sender gets a notice about anything left behind, including
   rasters and streamed layers that could not be copied.
2. **On the phone**, open the package with **Open Project** (or **Add Data**). It is unpacked into
   a folder of its own and its map opens. Any notices about what stayed behind appear as it opens.
3. **In the field**, capture and edit as above.
4. **Back from the field**, **Send Field Package**, in the More sheet, saves the map as a field
   package again, with the photos its features name, wherever you choose (a cloud drive, email,
   a USB connection to the computer).
5. **On the computer**, in the project the package was made from, **Data › Offline › Bring Back**
   reads it. New and changed features update the map, deleted ones are removed, and the photos go
   in a folder beside the project. A feature also changed on the computer takes the field's
   version, and the summary names it; **Undo** brings the computer's versions back.

Give one package to one phone, and make a new package after each Bring Back or Sync of its area:
from the area's offline map, **Package for the Field** again. A package comes back once, and only
while its area is as it was when the package was made. A second phone's copy of the same package, or
the same phone's second return, would undo or repeat what the first brought home, so Bring Back
refuses it and changes nothing; redo that phone's edits on a new package. A package made before
packages carried this check comes back as before, with a notice. A package for an area the project
doesn't have is refused.

### Origin identities and older field copies

Download Map gives each new offline area and its home project stable identities. New field
packages preserve those identities, including after Save As and reopening. Bring Back checks
both before applying edits or writing photos. Packaging an older area can also add identities
to its home project: save that project so the return can be matched to it. Undo can remove the
newly created area and identities too.

Saving a project with these identities declares the `field-origin` capability, even when the
offline area has never been sent as a field package. Earlier RustGIS releases that do not
support this capability refuse the saved project rather than discard its return association.
A project whose area has been packaged also declares `field-generation`, for the return check
described above, so a release without it can't save the project and then accept any copy.

A field copy made before these identities were introduced still opens, and Send Field Package
preserves its edits and photos as an unverified legacy return. Ordinary Bring Back refuses to
guess its home from names. After verifying which original home offline area it belongs to, a
user can explicitly recover it from the native command line:

```sh
rustgis-cli run --project home.rgis \
  --cmd 'offline.bringBack={"path":"legacy-return.rgisfield","legacyArea":"Site"}' \
  --save recovered-home.rgis
```

`legacyArea` accepts entirely ID-less returns and direct User dispatch. Assistant, MCP, plugin
and imported-item calls cannot select this recovery path, and it cannot override present,
malformed or incomplete identities. The result reports the unverified origin; normal conflict,
photo and Undo handling still apply. Make a fresh field package for further edits and save its
home project.

If the older field copy was saved as a plain `.rgis` project, send its existing edits and photos
first with the explicit legacy-return option:

```sh
rustgis-cli run --project edited-field.rgis \
  --cmd 'offline.package={"path":"legacy-return.rgisfield","legacyReturn":true}'
```

Use the project and photo files on the device where those edits were saved, or copy its
referenced photos along with it. This preserves the old copy's data without inventing an origin
identity. A newly identified package whose home cannot be found requires the home project saved
after packaging; legacy recovery cannot change that association.

## What differs on a phone

- **Plugins** can't be installed on an iPhone or iPad: App Store rules don't allow an app to
  download code that adds to what it does.
- **Choosing a folder** is not possible with the phone's picker; the app says so where one is
  asked for.
- **Recovery** keeps a copy of completed edits in the app's private storage. On the next launch,
  the app offers to recover unsaved work if a complete copy is available. The first edit submits
  a copy immediately. Edits made within two seconds of a submission are grouped into another copy
  due at the end of that interval. When the exit hook runs, it submits any waiting edit and gives
  the writer up to two seconds to finish. The system can stop the app without running that hook.
  If a copy is still waiting or being written when the process ends, recovery uses the most recent
  completed copy, if one exists. Recovered field packages can still be sent back with their photos.
- **Preferences and saved passwords** are not available on a phone yet.
- **HTTPS** on Android is checked against a bundled list of trusted roots rather than the
  device's own, and the app says so once. An iPhone or iPad uses the device's own.
