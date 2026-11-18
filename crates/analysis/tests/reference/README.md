# Generated polygon overlays

The fixed corpus in this directory checks known results. The generated overlay check also runs
Clip, Intersect, Union and Erase on seeded polygon pairs and layers with several features. It
varies concave outlines, holes, thin overlaps, shared edges, collinear overlaps, nearly touching
corners and multipart features.

The reference test sends the same Web Mercator input coordinates that RustGIS uses to Shapely's
GEOS. It compares each input ID's output (both IDs for Intersect and Union) by the area of the
symmetric difference. Ring order, orientation and redundant vertices do not affect that area.
Boundary-only GEOS results and empty polygons count as absent. The comparison checks output
validity and uses the fixed corpus's 0.1 ppm tolerance, scaled by the combined input area
because rounding depends on the inputs even when the answer is tiny or empty. It counts
polygon parts above that same area limit.

From the repository root, build the reference test and pass the executable printed by Cargo:

```sh
cargo test --locked -p rustgis-analysis --test reference --no-run
uv run crates/analysis/tests/reference/generate.py --overlay "target/debug/deps/reference-<hash>" --findings ./overlay-findings
```

CI runs six fixed seeds with 16 cases each. To replay a nightly run, set
`PROPERTY_SEED=<seed> PROPERTY_CASES=256` before the `uv run` command. To replay just a
minimized case, save the finding from the issue or run artifact as JSON and add
`--case path/to/finding.json`. Use a fresh `--findings` path each time.

The reducer keeps removing features, parts, holes and vertices until no single deletion
preserves the same kind of difference for the tool on valid inputs. The nightly run keeps these
findings as an artifact and uses the tool, difference kind and normalized reduced input to skip
issues it has already reported; it only trusts markers in issues the workflow opened itself. It
groups Clip splits that differ only by a gap a few overlay grid steps wide between masks, with each
reduced variant in the artifact. That group is reported again once its issue is closed. An
overlay run that fails without a finding opens its own issue, which closes when the check passes.

The default run also checks the inputs in `overlay-regressions/`. The Union fixture checks
that a polygon touching the zone boundary leaves the correct number of pieces. The Clip
fixture records a known split where individually rounded masks should meet. The default
run permits that specific difference, and `--case` reports it as a failure.
