# Interface themes

A theme is a JSON file that sets every colour the interface draws, plus its spacing and text
sizes. Light and Dark ship with RustGIS; users add their own to the themes folder. The map is
drawn by the renderer, which takes its light or dark defaults from the theme's `dark` flag.

## Where themes live

| Theme | File |
|---|---|
| Built in | `crates/ui-egui/themes/*.json`, compiled into the app |
| User | `<config>/themes/*.json`, read at startup, when a file changes, and by Reload Themes |

The config folder is `%APPDATA%\RustGIS` on Windows, `~/Library/Application Support/RustGIS` on
macOS and `$XDG_CONFIG_HOME/rustgis`, which is `~/.config/rustgis` when the variable is unset,
elsewhere. The default Linux themes folder is `~/.config/rustgis/themes`.
`RUSTGIS_CONFIG_DIR` overrides the config folder.

A user file whose `name` matches a built-in replaces it. A reworked Light sits in the folder as
`light.json`, and the menu lists one Light. Two files that claim the same name: the first in name
order is used, and the second is listed in red.

## Picking one

Settings › Theme lists Follow the System and every theme that loaded. Light and Dark are the
brightness choices, so there is no separate Dark Mode switch. A choice is applied at once and
saved for the next start. Interface scale, 80% to 200%, sits beside it, and it is the only
control for it: Ctrl + and Ctrl - do nothing, so the scale the app records is the scale it draws.

Follow the System follows the platform theme where the platform reports one: Windows, macOS and the
browser, through `prefers-color-scheme`. Linux desktops report no theme, so the entry is greyed out
there and the button names the brightness in force instead.

## What the browser build does

The browser has no config folder, so there is no themes folder there and **Load a Theme from File…**
is not offered. The browser offers Follow the System, Light, Dark and the interface scale: a theme in
a file is a desktop route. The two built-in themes are the same files, compiled in, so nothing else
differs.

On the desktop, to start from a theme you already have, use **Load a Theme from File…**. The app
keeps the source text, including inherited settings and exact colour strings, in the themes folder
(adding the resolved name if the file omitted it); edit that copy and the app follows it. Loading a file whose `name`
matches a theme already in the folder replaces that copy, and the version it replaces is kept beside
it as `<file>.json.bak`: one backup, replaced on each save. An interrupted save, or a save whose final syncing or cleanup fails, may leave
`<file>.json.recover`, containing the theme from before the save. Copy it somewhere safe,
compare it with the theme and backup, then remove the recovery file before saving again. A file that does not load is listed in
red at the bottom of the same menu with its reason. Unknown tokens and bad colour strings name
the token; values of the wrong JSON type give a line and column. A notice says when a file was refused.

While the themes folder is readable, the app checks file names, sizes and modification times
once a second. Without that folder, the app does not wake just to check for themes. If you create
the folder while the app is idle, interact with the app or use **Reload Themes** to discover it. A save
that changes that metadata is picked up automatically. If a save preserves both size and
modification time, use **Reload Themes** to reread it. A theme in use that stops loading leaves the window on the nearest built-in
until it loads again: the button names the theme drawn, and the choice returns with the file. A
file that is refused is listed in the menu with its reason; one deleted or moved away is not
listed, and only the fallback shows it is gone.

## A short theme

`extends` names a theme that ships, and the file then sets only what it changes:

```json
{
  "schema": 1,
  "name": "Cobalt",
  "extends": "Dark",
  "colours": {
    "accent": "#3B82F6",
    "accent_text": "#93C5FD",
    "on_accent": "#FFFFFF",
    "hover": "#22314D",
    "checked": "#1E3A8A"
  }
}
```

Colours, sizes and brightness the file leaves out come from the theme it extends. A short file is
the better start.

## The file

Without `extends` every colour is required, and a size the file leaves out keeps the standard one.
An unknown name, colour or size is refused by name. The built-in Light is the reference:

```json
{
  "schema": 1,
  "name": "Light",
  "dark": false,
  "colours": {
    "header": "#EEF1F5",
    "ribbon": "#FFFFFF",
    "tab_strip": "#EEF1F5",
    "canvas": "#E4E8EE",
    "pane": "#FBFCFD",
    "pane_header": "#F3F5F8",
    "border": "#DDE2E9",
    "border_strong": "#C3CAD4",
    "text": "#1F2630",
    "text_dim": "#5E6977",
    "text_disabled": "#A9B1BC",
    "icon": "#3A4554",
    "hover": "#E9F0FB",
    "pressed": "#D4E2F8",
    "checked": "#DCE8FA",
    "accent": "#2563EB",
    "accent_text": "#1D4EC4",
    "on_accent": "#FFFFFF",
    "input": "#FFFFFF",
    "input_border": "#C6CDD7",
    "group_label": "#707A88",
    "status_bar": "#F3F5F8",
    "selection": "#CFE0FC",
    "menu": "#FFFFFF",
    "shadow": "#0000001C",
    "green": "#169A5A",
    "red": "#D13C3C",
    "amber": "#E59E1A",
    "context": "#0E9384",
    "context_tint": "#E3F5F2",
    "control": "#DDE3EA"
  },
  "metrics": {
    "item_spacing": [6.0, 5.0],
    "button_padding": [8.0, 3.0],
    "row_height": 22.0,
    "corner_radius": 5,
    "body": 12.5,
    "button_text": 12.5,
    "small": 10.5,
    "heading": 17.0,
    "mono": 12.0
  }
}
```

An unknown token stops the file from loading, and the error names it. Colours are `#RRGGBB`, or
`#RRGGBBAA` where alpha matters, as in `shadow`. `schema` is 1; a file written for a later build is
refused by number.

`dark` is the brightness the theme reports to egui, which sets its default widget shapes and the
map's light or dark defaults. It is not inferred from the colours.

`metrics` are in points: item spacing, button padding, row height, corner radius and text sizes.
Corner radius is held between 0 and 32 and rounded to the nearest whole point; an adjustment is counted.
Sizes are held to what a window can draw, and a notice says when a value was held. A file over 64 KB
is refused.

## Tokens

Surfaces: `header`, `ribbon`, `tab_strip`, `canvas`, `pane`, `pane_header`, `menu`, `status_bar`,
`input`.

Lines and text: `border`, `border_strong`, `input_border`, `text`, `text_dim`, `text_disabled`,
`icon`, `group_label`, `on_accent`.

Interaction: `hover`, `pressed`, `checked`, `selection`, `control` (slider rails and checkbox
wells, which must stay visible on white).

Meaning: `accent`, `accent_text`, `context`, `context_tint`, `green`, `red`, `amber`, `shadow`.

## Commands

| Command | Effect |
|---|---|
| `ui.themeFile` with `name` | Uses that theme; no name follows the system again |
| `ui.themeLoad` | Opens a theme file from anywhere, keeps it in the themes folder and uses it |
| `ui.theme` with `dark` | Uses Light or Dark, the brightness asked for |
| `ui.scale` with `factor` | Interface scale, held between 80% and 200% |
| `ui.themeReload` | Reads the themes folder again |

## What a theme cannot change

The map is drawn by the renderer, so a theme's colours stop at the map view; `canvas` is the
surface behind it. Icon shapes stay as drawn, taking their ink and accent from the theme. Plugin
panels use the host's widgets and follow it.
