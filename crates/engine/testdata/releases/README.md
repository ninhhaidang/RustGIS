# Files written by RustGIS releases

These are the original bytes written by the published Linux x86-64 binaries on
2026-11-14, downloaded with `gh release download` from
[opengeos/RustGIS](https://github.com/opengeos/RustGIS/releases).
Never regenerate, reformat or resave them. Add a project and preferences file for
each new release, and extend `released_projects_and_preferences_still_open`.
All five releases below support both file types; none was skipped.

| Writer | Release archive | Archive SHA-256 |
|---|---|---|
| v0.2.0 | `RustGIS-0.2.0-linux-x86_64.tar.gz` | `1cd13d8c5d84777537d211314adc745ccf9341707b546555e24a294f990b384a` |
| v0.3.0 | `RustGIS-0.3.0-linux-x86_64.tar.gz` | `98c7c2a3c2e646fb0affb6c812db6735f83b316d35c4ef89fd3bb658cebb08f2` |
| v0.4.0 | `RustGIS-0.4.0-linux-x86_64.tar.gz` | `6ccd39f1c35ff824e2ce9d875922394a00ab2468057ad183080205d17f1e07a1` |
| v0.5.0 | `RustGIS-0.5.0-linux-x86_64.tar.gz` | `a14e6bed30059ab8fb1c026c71bdb46891b2b4195e0d0ea39cc1f903b64fa9bf` |
| v0.6.0 | `RustGIS-0.6.0-linux-x86_64.tar.gz` | `633bea1712c3369ce43aa0d5ffcebe9928ff2dd1068280e9a6a8c31a2c94a972` |

Each `v*.rgis` was saved by that release's `rustgis-cli run --no-plugins`
with commands followed by `--save v<version>.rgis`. Starting with `project.new`,
`layer.addDataText` added two made-up GeoJSON datasets, with no external data:

- Places: points (1, 2), (3, 4), with `name` = Alpha, Béta and `pop` = 10, 20.
  `layer.symbology` set unique values on `name`; `labels.enable`, `labels.field`
  and `labels.style` set bold, haloed labels on `name`, size 14, color `#123456`.
  `layer.definitionQuery` set `pop >= 20`.
- Routes: one line from (1, 2) to (3, 4), with `name` = Route.
  `layer.color` set `#336699`, `layer.outline` set `#112233`, width 2.5,
  and `layer.transparency` set 25 percent.
- `map.camera` set longitude 2, latitude 3, scale 250000.

The same commands saved Places before Routes in v0.2.0–v0.4.0, and Routes before
Places in v0.5.0–v0.6.0, which inserted new layers first. The test pins both orders.

Each `v*.prefs.json` was written by that release's desktop binary, started with
`--sample` and an empty, isolated `XDG_CONFIG_HOME`. Ctrl+F1 collapsed the ribbon;
the resulting `rustgis/prefs.json` was copied unchanged. Releases from v0.4.0 also
write an empty connections collection on startup. No personal settings were used.
