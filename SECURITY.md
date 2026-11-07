# Security policy

## Reporting a vulnerability

Report security problems privately through
[GitHub's private vulnerability reporting](https://github.com/opengeos/RustGIS/security/advisories/new),
not in a public issue, pull request or discussion. Include what you did, what happened, the
RustGIS version and platform (desktop, browser, command line or Python), and a file, plugin or
script that shows it if you have one. Leave out real passwords, keys and private data; a
made-up example is enough.

A maintainer will reply to the report. We aim to fix confirmed problems in the next release and
publish a GitHub security advisory with it, crediting you unless you'd rather we didn't. Please
give us a chance to release a fix before you disclose the problem publicly.

## Supported versions

RustGIS is released often and fixes go into the next release; older releases don't get
patches. Check that the problem still happens with the
[latest release](https://github.com/opengeos/RustGIS/releases/latest) or `main`.

## Sharing projects and generated items

Saving removes credentials from supported address fields and recognized executable parameters,
and reports counted removals. Feature data, user notes, foreign-language notebook code and opaque
plugin content can still contain sensitive information. Data-valued command parameters, such as
imported feature text and expressions, are preserved too. Supplied service documents are kept
opaque, with an unchecked-content notice when recognizable credential material is found.
An unchecked-content notice means that content was preserved, not verified safe. Review it
before sharing; saving is not a general secret scanner.

For an older file or log that exposed a key, revoke or rotate it with its service. Saving again
cleans only recognized fields and cannot recall shared copies. For a Jupyter notebook, also clear
saved widget state and save again; printed cell output and user data require your own review.

## Active-login output redaction

While a service login is active, generated output masks its known secrets throughout the
process. A very short password can therefore also mask ordinary words, names or addresses,
including output from another session. This changes displayed output, not the stored data,
requests or permission checks. RustGIS does not retain revoked secrets just to recognize them
in later output; review that output before sharing it.

## Embedding-page command boundary

The execution policy covers engine commands admitted through named assistant, MCP, plugin and
imported-item scopes. The authority of `ui.*` RUN messages from embedding pages remains a
separate, unresolved boundary; engine approval checks do not establish authorization for those
UI messages.

## What counts as a security problem

RustGIS makes promises about what untrusted plugins, files, agents and web pages can do. A way
around one of them is a security problem:

- **Plugins**: a WebAssembly plugin reaching files, the network or the project beyond the
  permissions it was granted, or escaping its sandbox; a plugin installing or running despite the
  registry blocklist or a hash mismatch. See [Plugins › Safety](docs/plugins.md#safety).
- **Agents**: anything reaching the MCP relay or MCP over HTTP without its token, or from another
  host or a web page; the AI Assistant running a file, network or plugin command without the
  user's approval. See [MCP](docs/mcp.md) and the [AI Assistant](docs/user-guide/assistant.md).
- **Credentials**: passwords, keys or tokens written to the audit log, other logs, project files
  or exports, or stored outside the OS keychain where the docs say they're kept there.
- **Files and links**: opening a data file, project, style, package or URL that runs code,
  writes or reads files the user didn't choose, or sends local data somewhere.
- **The web app**: script injection or reading another site's data at `web.rustgis.app` or the
  other `rustgis.app` sites.
- **Releases**: anything that could put untrusted code into a published build or package.

## What doesn't

Report these as ordinary [issues](https://github.com/opengeos/RustGIS/issues):

- A crash, hang or memory blow-up from a malformed file or parameters, unless it leads to more
  than that. These are bugs we fix, and the regression tests keep them fixed.
- Anything a plugin marked *Not sandboxed* (a process plugin: a Python script or a native
  program) does: it runs with your rights, and the install prompt says so.
- Anything a holder of the MCP token does, after the user approves it, or with approval turned off
  (**Approve agents' actions without asking**, or the MCP server's `--yes`): whoever holds the
  token or approves the action can do what an agent can.
- Data sent to a service you configured (an AI provider, a geocoder, a tile or database server),
  as described in [Privacy](docs/privacy.md).
