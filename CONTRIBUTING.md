# Contributing to RustGIS

Contributions are welcome. This page covers what to work on and how to propose it. For build,
test, docs and release mechanics see [docs/contributing.md](docs/contributing.md); for code
conventions see [AGENTS.md](AGENTS.md).

## Most roadmap items have no issue

[ROADMAP.md](ROADMAP.md) lists where RustGIS is heading. Only a few of its items link to an
issue; **most features have no issue yet**. Don't wait for one to appear before starting. If
something on the roadmap (or not on it) interests you, propose it and open a pull request.

## How to contribute a feature

1. **Check for an existing issue.** Search the
   [issues](https://github.com/opengeos/RustGIS/issues) and the roadmap. If there is an issue,
   comment on it to say you're taking it.
2. **Write a short proposal** if there is no issue. It covers:
   - the problem or workflow it serves;
   - what you plan to build: the commands it adds, where it sits in the ribbon (following the
     [ribbon guidelines](docs/contributing.md#ribbon-guidelines)), which formats or tools are involved;
   - which crates it touches and whether it adds a dependency;
   - for work that grows with the data, how it runs off the UI thread with progress and Cancel
     (see [Never hold the window](AGENTS.md#never-hold-the-window));
   - how you will test it.

   Put it in an issue with the
   [feature request](https://github.com/opengeos/RustGIS/issues/new?template=feature_request.yml)
   form, or in the description of a draft pull request. For changes that need coordination (see
   below) use an issue and wait for a maintainer's reply; otherwise either works.
3. **Open a pull request.** Draft pull requests are welcome early, to get feedback on the
   approach before the work is finished. Link the issue (`Closes #123`) and fill in the
   [pull request template](.github/pull_request_template.md). If the change shows on screen, add
   screenshots to the description (before and after for a change to existing UI, after only for
   new UI); see [attachments](docs/contributing.md#documentation). Before it is ready for review,
   it must meet the rules and quality gates in [AGENTS.md](AGENTS.md).

## Pick freely or coordinate first

- **Pick freely**: roadmap items marked _(open)_ are self-contained. Open a pull request directly,
  with the proposal in its description; an issue is optional.
- **Coordinate first**: open a proposal issue and wait for a maintainer's reply before writing much
  code if your change
  - changes the data model (`Feature`, `Coord`, `Renderer`);
  - adds a dependency or a crate;
  - changes the `.rgis` project format, or an existing command's id or parameters (the CLI,
    Python, MCP and saved scripts call commands by id);
  - changes the plugin interface;
  - falls under _Exploratory_ in the roadmap.

[Maintaining RustGIS](docs/maintaining.md) says who approves each of these and what happens when a
proposal gets no reply.

Roadmap themes are not releases or an order; an item moves to Done when it lands. A
pull request that lands a roadmap item, or an item of [GEOLIBRE_PARITY.md](GEOLIBRE_PARITY.md),
updates that file in the same change.

## AI-assisted contributions

Code written with AI tools is welcome, from anyone, whatever their programming experience
([#203](https://github.com/opengeos/RustGIS/discussions/203)). A contribution is judged by its
quality, not by who or what wrote it.

Whoever opens a pull request is responsible for it, however it was written:

- you have read the change and can explain what it does and why;
- you ran the checks in [AGENTS.md](AGENTS.md) and looked at what the change does (the
  screenshot, the output file, the numbers) rather than trusting a tool's summary of it;
- the pull request description and replies to review comments say what is true of the change,
  whether or not a tool drafted them.

## Where code comes from

RustGIS is licensed under Apache-2.0, so everything in it must be your own work or come from a
source whose licence allows that.

- Don't copy code from GPL, LGPL, AGPL or other copyleft projects (such as GRASS, QGIS or GEOS),
  and don't ask an AI tool to translate it into Rust: a translation is still a copy.
- Implementing an algorithm from a paper, a standard or a specification is fine. Cite it in a
  code comment or the pull request.
- If you followed another project's code while writing yours, name it in the pull request so a
  reviewer can check.
- Running another program only to produce expected test results is fine, as the
  [analysis reference corpus](ATTRIBUTION.md) does with GEOS and PROJ.

Report security problems privately, as [SECURITY.md](SECURITY.md) describes, not in an issue or
pull request.
