# rustgis-mobile

RustGIS on Android and iOS ([#1902](https://github.com/opengeos/RustGIS/issues/1902)). It runs the
desktop front end (`rustgis-ui-egui`) in a `NativeActivity` on Android and in a UIKit window on iOS
(see [iOS](#ios)), with the system's file picker for opening and saving, and the touch interface (`rustgis-ui-touch`) around its map, which can capture points,
lines and polygons with their attributes, edit their shapes, snap, and show where the device is.
It averages positions, records tracks (also out of sight), attaches photos to features, and takes a
map with its offline basemap to the field and its edits back (see Field packages).

## Build

Once:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --locked
# A JDK (for keytool and apksigner), and an Android SDK in ~/Android/Sdk (or ANDROID_HOME), with:
sdkmanager "ndk;27.2.12479018" "build-tools;36.0.0" "platforms;android-35" "platform-tools"
```

Then:

```sh
apps/rustgis-mobile/build-apk.sh             # arm64-v8a and x86_64, release
apps/rustgis-mobile/build-apk.sh x86_64      # the emulator's only
adb install -r target/android/rustgis-mobile.apk
adb shell am start -n org.rustgis.mobile/.MainActivity
adb logcat -s rustgis                        # the app's log
```

Open, Add Data and Save use Android's own file picker (see below). For testing, files can also be
pushed to the app's external files folder before starting the app: every file or folder there is
opened once the window is up, as if dropped on the desktop's window.

```sh
adb push parcels.gpkg /sdcard/Android/data/org.rustgis.mobile/files/
```

An emulator for testing without a phone:

```sh
sdkmanager "emulator" "system-images;android-35;default;x86_64"
avdmanager create avd -n rustgis-spike -k "system-images;android-35;default;x86_64" -d pixel_6
emulator -avd rustgis-spike -no-window -no-audio -gpu swiftshader_indirect
adb exec-out screencap -p > shot.png
```

The APK is signed with a debug key made in `target/android`. There is no Gradle project: `aapt2`
links the manifest, `javac` and `d8` compile the one Java class, and the Rust library is the rest
of the app.

## The phone, from Rust

`rustgis_engine::platform::Platform` is what a front end asks a phone for: files from the system's
picker, a place to save, the device's position, positions kept coming out of sight, and the
camera. `src/phone.rs` implements it for Android by calling the activity's Java half,
`java/org/rustgis/mobile/MainActivity.java`, which is the only code that can show the picker (the
Storage Access Framework) and receive its answer.

Rust calls Java and never the other way: each frame the app collects what happened
(`takeEvents`), so the library exports nothing but `android_main`.

- **Opening.** A picked document is copied into the app's storage (`files/picked/…`), because the
  picker gives a document, not a path. Data and projects then open by path, so a GeoPackage is
  edited in place, in its copy: the original is not changed. Every other picker's file (a picture,
  a plugin, a delimited text file) goes to the dialog that asked, as in the browser.
- **Saving.** Save, Save As and the exports offer their file through the picker, as the browser
  offers a download. A project saved this way names its picked data by the copies' paths in the
  app's storage.
- **Not yet.** Choosing a folder (Add Folder, a Zarr store) says it isn't available. The copies of
  data and projects in `files/picked` are never removed (a dialog's file is removed once read). Bringing edits back to the original file is phase 3.

## The touch interface

A phone starts in the touch interface: the map, a bar of four large buttons under it (Layers, Add
Data, Capture, More) and sheets that rise over the map. The Layers sheet shows and hides layers,
zooms to one and makes one the active layer; the More sheet has Full Extent, the project, undo
and redo, and **Use the Desktop Interface**, which brings back the ribbon and panes until the app
is started again.

**Capture** adds a feature to one of the map's point, line or polygon layers. A cross marks the
middle of the map: the map is moved under it, and the sheet's fields are filled in (one for each
of the layer's attributes). For a point layer, **Add Point Here** puts the point at the cross.
For a line or polygon layer, **Add Vertex Here** places a vertex at the cross, the shape so far
is drawn over the map with a thin line on to the cross, **Undo Vertex** and **Start Over** take
vertices back, and **Finish Line** or **Finish Polygon** adds the feature once it has enough of
them (two for a line, three for a polygon). The sheet stays up, emptied, for the next feature.
Editing is turned on for the map when it was off, and the sheet says so.

The form follows the layer's data design. A field with a list of values (a coded-value domain,
the layer's subtypes, or yes and no) shows a chip for each: one is tapped to choose it, and
tapped again to leave the field empty. A field with a range says its limits. When the layer has
feature templates, a **Template** row chooses one, and its defaults fill what the form leaves
empty; the template stays chosen for the next feature. A field a constraint rule requires (the
rule asks `IS NOT NULL` of it, alone or joined to other clauses by `AND`) is marked with `*`, and a
feature with it empty, and no default for it from the template or subtype chosen, isn't added: the
sheet names the fields to fill in. A field in a contingent value set offers only the values that
fit what the set's other fields hold, as chips (a value chosen before the others changed stays,
so the conflict shows). A date or date and time field has a calendar button: a month of days a
finger tall each, with **Today** (in UTC; the phone's time zone isn't asked) and, for a date and
time, **Now**.

While a field is typed in, the sheet takes most of the window and stays above the soft keyboard:
the activity's Java half counts the keyboard in the window's bottom inset, the app looks again
for it while a field wants the keyboard (Android doesn't wake the app when it comes or goes), the
field is scrolled into view, and closing the keyboard ends the typing (Android 11 and later, which say when it goes; before, the field keeps the focus until the next tap, and the keyboard's room isn't counted).

**Show My Position**, in the More sheet, asks Android for the device's position (and the user,
the first time, for leave to use it). The position is drawn on the map as a dot in a ring as wide
as it is good to, and follows the device about once a second. **Go to My Position**, in the More
sheet and at the top of the Capture sheet, moves the map so the capture cross is on it: a point
or a vertex added then is at the device's position. **Hide My Position** stops asking, which
spares the battery.

**Average Position**, beside Go to My Position in the Capture sheet, averages the positions that
come while it runs: each weighted by how exact Android says it is (one over its accuracy squared),
and the sheet says how many and how far they lie from their average. **Use Average** puts the
cross there, for a point or a vertex more exact than one position.

**Record a Track**, in the More sheet with the position shown, draws where the device goes as it
goes, with a bar over the buttons that counts its positions and has **Stop**. Stopping (or Hide My
Position) adds the track as one line to the map's **Tracks** layer, made when the map has none,
named for when it started, with its count and length in minutes. A position less exact than 50 m
is left out, and the notice says how many were. Every position Android reports reaches the track,
not only the last one each frame (`RustGisApp::fixes`). The track is kept in memory until it is
stopped: an app the system ends loses it.

A track goes on while the app is out of sight (the Home button, the screen off). Recording one
starts `TrackService`, a foreground service of the location type, with the notice Android requires:
"RustGIS is recording a track", which brings the app back when tapped. The activity's location
listener then keeps receiving positions after the activity stops, and they wait in its events
(at most 200,000; past that the oldest go, and the app says how many) until the app is back in
sight and takes them all into the track. The first track asks for leave to show notifications
(Android 13 and later); refused, the service still runs, its notice unseen. Stopping the track
stops the service. Ending the app (swiping it away), or Android ending its activity, ends the track with it: the session goes with the process (see below).

**Edit**, beside **Add** at the top of the Capture sheet, changes the shapes already on the
map, the same way: the map is moved under the cross. **Pick Feature at Cross** picks the layer's
feature drawn on top under it (within half a finger) and draws its vertices. For a line or polygon,
**Pick Vertex at Cross** picks the vertex nearest the cross, and then **Move Here** moves it to
where the cross is next, **Add After** puts a new vertex there after it, and **Delete Vertex**
removes it (a line keeps two, a ring three). For a point, **Move Point Here** moves it. **Delete
Feature** deletes it (only it: the map's other selections are kept), and **Done** lets
it go. Each change is one edit, undone by Undo in the More sheet.

**Take Photo**, over the Capture sheet's fields, asks the device's camera app for a photo
(`Platform::take_photo`). The camera writes it to the app's photos folder (`files/photos`) through
a content address the activity's `PhotoProvider` offers, so RustGIS needs no camera permission. Each
photo taken shows as a chip (a tap takes it off again; the file stays), and the next feature added
names them in its layer's photos field: a text field `photos` (or `photo`) listing each photo's
path, separated by `;`, as a report's attachments field does. A layer without one gets one when its
first photo is attached, and the sheet says so; the form doesn't offer that field for typing. In Edit
mode, **Add Photo** attaches a photo to the feature picked, after the ones it has (one edit, which
undoes; a layer's first photo also adds its photos field, an edit of its own). A photos field the
layer already has stays in the form: the photos go after what is typed in it. A field named
`photos` that isn't text can't list them, and the sheet says so.

**Snap** turns the map's snapping on and off (its settings are the desktop's: Edit › Snapping).
With it on, the cross's place snaps to a vertex, a line's end or an edge in reach, for adding and
editing alike: a green ring marks where, and the sheet says what it snapped to. A vertex being
moved doesn't snap to itself or its own edges.

What it doesn't do yet: editing an existing feature's attributes, and a new layer to capture into.

The interface keeps out of the room the system's bars take: the activity's Java half reports the
insets, and the bars' icons are drawn dark over a light theme's ground and light over a dark
one's.

## Field packages

A map goes to the phone as a field package (`.rgisfield`), made on the desktop by **Data ›
Offline › Package for the Field** (or `offline.package`). It holds a project of one map, the
features in view as an offline area of the desktop project (as Download Map makes one, so the
area is in that project to bring the edits back to), and the MBTiles and PMTiles archives its tile
layers draw from, under `basemaps/`, so the basemap shows with no network. Tile layers that draw
from the web stay in it and show only online; the package says which. Raster layers are left
out, and said.

Opening a field package (Open Project, the picker, or a file pushed to the app's folder) unpacks it
into a folder of its own in the app's folder (`field/<name>`, numbered when one of that name is
there: a copy unpacked before is never written over) and opens its project there, unsaved, as a
project package opens. Its project's paths are trusted as a project file's are. Packaging and
opening run on the window's thread, as Package Project and opening a project do, with no Cancel:
a large basemap archive holds the window while it is read or written. Adding an `.mbtiles` or `.pmtiles` file as data adds it as a tile layer too, an
offline basemap with no package.

The edits come back the same way. With a field package's map open, **Send Field Package** in the
More sheet saves the map as a field package again (through Android's picker), with the photos its
features name, under `photos/`, each feature naming its photos there. Only picture files go in
(`.jpg`, `.jpeg`, `.png`, `.heic`, `.heif`, `.webp`), under a plain file name, listed in `photos/index.tsv` with the
path their feature named them by; other files a photos field names are said, not put in; a package
going out to the field carries none. A photos field lists paths separated by `;`: a path that
holds a `;` is read as one where, joined again, it names a file here, and a photo's name in a
package never holds one. On the computer, in the
project the package was made from (saved after packaging, so it holds the offline area), **Data ›
Offline › Bring Back** (`offline.bringBack`) reads it: the photos go in a `<area> photos` folder
beside the project and the features name them there (a photo whose listed path holds the same
bytes here, sent from this computer, keeps that path; one whose name is taken there by other
bytes, from an earlier package, gets a name of its own, and the summary says how many); a field the phone added to a layer (its
photos field) is added to the layer at home; and the edits are copied back by feature, as Sync
does for an offline area: changed and new features update the map, deleted ones are removed from
it, and a feature changed at home too takes the field's version, said by name (Undo brings the
home versions back). A package for an area the project doesn't have is refused, by name, and so
is one whose layers aren't the area's (an area of the same name in another project).

One field package goes to one phone at a time: Bring Back compares what comes back with the area
as it was last brought back, so a second phone's edits to the same package, brought back after the
first's, would undo the first's where they differ. Send a new package after each Bring Back. A
feature added in the field keeps its id at home when the id is free; one that takes a new id (its
own was used outside the area) is said, as a later package from the same phone replaces it.

## What differs from the desktop

| | On Android |
|---|---|
| File dialogs | Android's picker, through `Services::open_async` and `download` as in the browser. `rfd` has no Android backend, so what still asks it (choosing a folder) is cancelled and says so (`crates/ui-egui/src/filedialog.rs`) |
| HTTPS trust | The bundled Mozilla roots, not the device's certificates; the session says so once (`rustgis_data::http`) |
| Folders | Settings, app data and the cache are in the app's internal storage, set once at start-up (`rustgis_data::dirs::set_root`) |
| GeoLibre tools, GDAL formats, DuckDB | Not available: a phone can't run their programs. `rustgis_data::RUNS_PROGRAMS` is false on Android and iOS, so no program is looked for, and each place they would appear says so: a file read through GDAL or a DuckDB database is refused with the reason (convert it on a computer), the SQL pane says why it can't query, and the Geoprocessing pane shows a card in place of the GeoLibre tools. Add Data's Database… stays: its PostGIS, MySQL and SQL Server connections need no program. The app is built without the engine's `geolibre` feature |
| Preferences, the keychain, autosave | Not wired up yet |
| Soft keyboard | It opens for a text field, and the touch interface keeps the field above it. `NativeActivity`'s text input is still poor (no suggestions, no composing): a real app moves to `GameActivity` |

## What phase 0 found

On the Android 15 emulator (x86_64, software GPU), from this crate as it is:

| Checked | Result |
|---|---|
| Builds and links for `aarch64-linux-android` and `x86_64-linux-android` | Yes. The whole workspace below the apps compiles unchanged apart from `rfd`. The bundled SQLite and the Zstandard decoder build with the NDK's clang; the library needs only Android's own system libraries |
| Launches and draws the sample project | Yes, through wgpu on Vulkan, in under a second |
| Touch | Taps, menus and one-finger map panning work. Pinch zoom was not tested (the emulator can't be sent two fingers by `adb`) |
| Opens a GeoPackage | Yes |
| HTTPS basemap tiles | Yes, with the bundled roots |
| Rotation | Yes, without a restart |
| Suspend and resume | Yes: the surface is dropped and made again, the session is kept |
| The activity destroyed by the system | The session is lost. winit's event loop can't be made twice in a process, so the app ends its process and the next launch starts afresh; without that, the next launch showed nothing |
| Typing | Key events reach text fields. The soft keyboard itself needs a phone to judge |
| Size | 78 MB for arm64 without symbols, 89 MB in the APK for x86_64 |

What it showed is wrong or missing:

- (Fixed since.) The window drew under the status bar and the navigation bar.
- The desktop ribbon and panes are unusable at a phone's width, as expected. Phase 2 is a new front end.
- The map draws on the CPU here (the emulator has no GPU). Frame rates need a phone.
- Not tried on a phone: arm64, a real GPU, pinch zoom, the soft keyboard, memory limits.

## iOS

The same app on iPhones and iPads: `src/shell.rs` is what both systems share (the window, the
picked files, the events), `src/android.rs` and `src/phone.rs` are Android's half, and `src/ios.rs`
is iOS's, which calls UIKit and Core Location through `objc2`. The executable is `src/main.rs`.

### Build

On a Mac with Xcode, once:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
```

Then:

```sh
apps/rustgis-mobile/build-ios.sh                 # the iOS Simulator, release: target/ios/RustGIS.app
apps/rustgis-mobile/build-ios.sh --debug         # the same, quicker to build
xcrun simctl boot "iPhone 17" && open -a Simulator
xcrun simctl install booted target/ios/RustGIS.app
xcrun simctl launch --console booted org.rustgis.mobile    # the app's log on the terminal
```

`build-ios.sh device` builds for iPhones and iPads; with `IOS_SIGN_IDENTITY` (an Apple
Development or Distribution identity in the keychain) and `IOS_PROFILE` (a provisioning profile for
`org.rustgis.mobile`) it signs the app and packs `target/ios/RustGIS.ipa`, for a device or for
TestFlight. There is no Xcode project: cargo builds the executable and the script makes the bundle
around it, with `ios/Info.plist`.

Files put in the app's Documents folder are opened once the window is up, as the pushed files are
on Android:

```sh
cp parcels.gpkg "$(xcrun simctl get_app_container booted org.rustgis.mobile data)/Documents/"
```

### The phone, from Rust

| | On iOS |
|---|---|
| Opening | UIKit's document picker, any file, several at once (a Shapefile's parts). It gives the original documents, wherever they are (iCloud Drive, another app's folder): each is read inside its security scope, through a file coordinator (a document in iCloud Drive is downloaded first), and copied into `Library/Application Support/picked/…` off the main thread, a folder for each folder the documents came from, and opened from there as on Android. The original is not changed |
| Saving | The bytes are written to the app's `tmp` folder under the name offered, off the main thread, and then the document picker exports a copy where the user chooses; then the file in `tmp` is removed. Cancelled, the app says the file wasn't saved |
| Position | Core Location, best accuracy. Asked the first time (`NSLocationWhenInUseUsageDescription`); positions start once the user allows them. Refused, now or later in Settings, the app says so and stops asking |
| Out of sight | Recording a track turns on background location updates (the app's `location` background mode in `Info.plist`), with iOS's indicator that an app is using the location. Positions found out of sight wait, at most 200,000, as on Android |
| Camera | `UIImagePickerController`: the photo is written as a JPEG to `Library/Application Support/photos`. A device without a camera (the Simulator) says so |
| Soft keyboard | winit's own (`UIKeyInput`): letters and Backspace reach the form. The room the keyboard takes comes from UIKit's keyboard notices and keeps the field above it. Return types nothing, as winit 0.30 hands it on as a character: typing ends with a tap outside the field |
| Folders | Settings, app data and the cache are in `Library/Application Support` |
| Picked copies | Kept in `picked/`, as on Android: data and projects open from their copies and are edited there. The folder is left out of the device's backups (`NSURLIsExcludedFromBackupKey`), as the documents are the user's elsewhere. Nothing removes old copies yet |
| Plain HTTP | App Transport Security is off for every host (`NSAllowsArbitraryLoads`): a project's tile layers and services may be any `http://` server, not known when the app is built, so a list of exceptions can't name them. The App Review notes say so |

`src/ios.rs` is the one module of the app with `unsafe` code beyond Android's `no_mangle`, an
exception to the workspace's rule allowed by `#![expect(unsafe_code)]` on the module: UIKit's
delegates are Objective-C classes made with `objc2`'s `define_class!`, which needs `unsafe impl`,
and a few of UIKit's calls are unsafe in `objc2`. Each block says why it holds.

Two things winit 0.30 doesn't do on iOS 27 are done in `src/ios.rs`:

- **Scenes.** iOS 27 refuses an app without a scene. `Info.plist` names a scene delegate of the
  app's, which puts winit's window in the scene once both are there (winit makes its window
  before scenes, after the scene is connected), or iOS shows nothing.
- **The safe area.** eframe 0.37 makes its GPU surface the size of winit's safe area, while egui
  lays out over the whole screen: on a phone with a notch or a home indicator the picture was
  stretched and its bottom cut off. The window is given a safe area of none (a subclass of
  winit's, made at start-up), and the shell keeps the interface out of the real one itself.

The Simulator's Metal falls short of wgpu's default limits, so the app asks for egui's, no better
than the GPU's own.

Tested on the iOS 27.2 Simulator (iPhone 17 and iPhone 18 Pro). Not yet on a device: the camera,
GPS out of doors, memory limits and the frame rate on a phone's GPU need one.
