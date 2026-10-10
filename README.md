# curlyfries ➿🍟

A command-line HTTP client whose request collection *is* the filesystem. Each
request is a JSON file, environments are JSON files next to them, and captured
response values are written back into a session file so the next invocation can
reuse them without logging in again.

- **Requests as files** — `requests/ships/create.json` is the request `ships/create`.
  They live in git, review like code and nest as deep as you like.
- **Templates, not scripting** — `${token}` anywhere in a path, header or body
  picks the value up from the session store or the environment, and
  `${random.string.8}` / `${random.int.1.500}` / `${random.bool}` mint fresh
  random values when nothing defines them.
- **Captures** — `outputs` pull values out of a response body, a header or the
  status line and keep them for later requests (in this run or a later one).
  Mark a value secret — an environment entry or an `outputs` entry written
  `{ "secret": true, "value": … }` — and the UI shows `••••••` instead of it
  while the real value is still substituted, sent and persisted.
- **Loud failures** — an undefined variable, a missing output path or a broken
  `${` names the file, the JSON pointer and the value that caused it.

## Install

```bash
cargo install --path .        # ~/.cargo/bin/curlyfries
```

## Layout

```
<project root>/                  # directory with curlyfries.json
  curlyfries.json                # manifest (marks the project root)
  requests/**/*.json             # one file per request, any nesting depth
  environments/<name>.json       # optional named environments
  .curlyfries/session.json       # captured variables (add to .gitignore)
```

That is the layout `curlyfries init` writes into an empty directory (as does
`curlyfries import`, from an API document): the manifest, requests, environments
and a `.gitignore` covering the capture store.

## Quickstart against the Pirate API

The `sample-api/` directory in this repository holds a small pirate-themed REST
API used for the examples (it is not part of the crate).

```bash
node sample-api/src/server.js &        # http://127.0.0.1:4000
cd examples/pirate
curlyfries                             # opens the terminal UI
```

The UI opens on `dev` (the manifest's `defaultEnvironment`) with the request
tree on the left. Select `auth/login` and press `Enter`: the response lands in
the pane below and its `outputs` are captured under the `dev` scope, so `token`
and `role` now resolve everywhere. Run `auth/me` next — its
`Authorization: Bearer ${token}` header comes from that capture — then
`ships/detail`, `ships/list`, `ships/create` and `ships/delete` the same way.

Captures persist to `.curlyfries/session.json`, so a later launch needs no
second login. Press `e` to pick the environment (`(none)` plus every
`environments/*.json`) and `?` for the full key list.

## Terminal UI

Running `curlyfries` opens a full-screen TUI: the request
tree on the left, the resolved (or definition) request above the response, the
active scope's captures below, and a key legend at the bottom. The resolved
request labels each part — `url`, `headers` and `body`. Whenever a response
arrives its status line is shown, even if an `outputs` expression then fails;
that failure appears in its own wrapped **Output error** section below the tabs.

```
╭Requests──────────────────────────────╮╭Request — resolved ───────────────────╮
│auth/                                 ││url                                   │
│  POST auth/login — …                 ││POST http://127.0.0.1:4000/auth/login │
│ships/                                ││headers                               │
│  GET ships/list — …                  ││Content-Type: application/json        │
│                                      ││body                                  │
│                                      ││"••••••"                              │
│                                      │╰──────────────────────────────────────╯
│                                      │╭Response — http://…/auth/login────────╮
│                                      ││← 200 OK  1ms                         │
│                                      ││ 1 Body │ 2 Headers │ 3 Raw           │
│                                      ││{ "access_token": "…" }               │
╰──────────────────────────────────────╯╰──────────────────────────────────────╯
╭Captures — dev─────────────────────────────────────────────╮
│token = ••••••                                             │
╰───────────────────────────────────────────────────────────╯
↑↓ select · Enter run · e env · x clear · Tab body/headers/raw · v definition/resolved · / filter · ? help · q quit
```

| key | effect |
|---|---|
| `↑`/`k`, `↓`/`j` | move the selection (folders are skipped) |
| `g`, `G` | first / last request |
| `Enter`, `r` | run the selected request; the status line, body and captures update in place |
| `e` | choose the environment (`(none)` plus every `environments/*.json`) |
| `x`, `X` | clear the active scope / every scope, each behind a `y/n` confirmation |
| `/` | filter the tree; `Esc` clears, `Enter` keeps the filter |
| `Tab`, `Shift-Tab` | next / previous response tab |
| `1`, `2`, `3` | Body / Headers / Raw |
| `v` | resolved / definition request view |
| `J`, `K`, `PageUp`/`PageDown`, `Home`/`End` | scroll the response |
| `?` | help overlay (any key closes it) |
| `q`, `Esc`, `Ctrl-C` | quit |

Runs happen on a worker thread, so the interface stays responsive; captures are
persisted to `.curlyfries/session.json` after every request that captured
something. curlyfries needs a terminal on stdin and stdout and exits `1` with
`error: curlyfries needs a terminal` otherwise. Colours are dropped when
`NO_COLOR` is set.

## CLI reference

```
curlyfries [-C <dir>]     # open the terminal UI
curlyfries init           # write a starter project into the current directory
curlyfries import <SPEC>  # write a project from an OpenAPI 3.x or Swagger 2.0 document
```

| flag | meaning |
|---|---|
| `-C, --project <dir>` | project root; defaults to the current directory |

`init` writes `curlyfries.json`, a sample `requests/ping.json`
(`GET ${baseUrl}/ping`), `environments/dev.json` (with
`"baseUrl": "http://127.0.0.1:4000"`) and a `.gitignore` holding `.curlyfries/`,
printing each created file. It changes nothing when any of those four files
already exists (exit `1`, `error: cannot initialize a curlyfries project in
<dir>`), and `-C` cannot be combined with `init` (usage error, exit `2`).

`import <SPEC>` reads a local OpenAPI 3.x or Swagger 2.0 document (JSON or
YAML) and writes the same layout: one request file per operation under
`requests/<first tag>/<operationId>.json`, falling back to `default/` and
`<method>-<path>` when the operation names neither, plus the manifest, a
`.gitignore` and a single `environments/default.json` holding the document's
`baseUrl`. Operation parameters become `${name}` placeholders for you to fill
in, while request bodies are rendered from the document's schemas — so an
operation without parameters runs as soon as it is imported. Like `init`, it
changes nothing when any target file already exists (exit `1`, `error: cannot
import into <dir>: these files already exist`), and `-C` cannot be combined with
`import` (usage error, exit `2`). The document must be a local file: nothing is
fetched, not even external `$ref`s, and `trace` operations, non-JSON request
bodies and `cookie` parameters are skipped. An unreadable, unparsable or
unsupported document fails with `error: invalid API document <path>` (exit `3`).

curlyfries otherwise always opens the terminal UI; `--help` and `--version` are
the only things it prints without one. Environments, the per-request timeout and
the capture store are configured in files (see
[`docs/format.md`](docs/format.md)) and driven from inside the UI.

## Redirects, symlinks and the timeout

Redirects and symlinks are off by default, so neither a hostile server nor a
hostile checkout can steer the client on its own. Set them in `curlyfries.json`:

```json
{ "followRedirects": true, "followSymlinks": true, "timeout": 30 }
```

- `followRedirects` follows `3xx` redirects that stay on the **same origin** —
  identical scheme, host and port, with default ports normalised — for at most
  10 hops. Anything else (another host, another port, an https→http downgrade)
  fails with `error: redirect error in <url>`, so request headers never reach
  another origin.
- With redirects off, a `3xx` is reported as the response and stderr carries
  `warning: response is a redirect to <location>, but following redirects is
  off; …` — the `Location` is never fetched.
- `followSymlinks` restores following symlinks inside `requestsDir` and
  `environmentsDir`; by default symlinked entries are skipped, so the walk
  cannot read files outside the project.
- `timeout` is the per-request timeout in seconds, `30` by default; `0` disables
  timeouts. A request that exceeds it fails with
  `error: request timed out after <n>s`.
- Plain http keeps working either way: the policy compares scheme, host and
  port rather than requiring TLS.

## Exit codes

| code | meaning |
|---|---|
| `0` | the UI exited cleanly |
| `1` | filesystem or environment: project not found, missing requests directory, IO, transport, timeout, refused redirect, not a terminal, unreadable session file, `init` or `import` finding existing files |
| `2` | CLI usage error (from the argument parser) |
| `3` | project or request definition: invalid JSON, schema violation, template error, undefined variable, output evaluation, unknown request id, invalid API document |

## What failures look like

```
error: undefined variable `token` in requests/auth/me.json
  at:    /headers/Authorization
  value: "Bearer ${token}"
  known: environment `dev` → admiral, baseUrl, limit, pirate
         session           → (none)
```

```
error: output `shipId` failed in requests/ships/create.json
  expression: response.body.id
  reason: no key `id` in object (available: class, crewCapacity, homePort, id…)
```

```
error: invalid request file requests/ships/create.json
  at:    /ouputs
  reason: unknown key `ouputs` (allowed: body, headers, method, name, outputs, path)
```

```
error: unterminated `${` in requests/ships/create.json
  at:    /path
  value: "/ships/${"
  hint:  write `$${` for a literal `$` followed by `{`, or close the placeholder with `}`
```

```
error: no base URL for request `ships/list`
  path: "/ships"
  hint: add "baseUrl" to environments/dev.json, or use an absolute URL
```

```
error: no curlyfries.json found in /tmp
  hint: run inside a project, or pass -C <dir>
```

```
error: unknown request `ships/crete`
  available: auth/login, auth/login-admiral, auth/me, debug/echo, debug/slow, ships/create, ships/delete, ships/detail, ships/list, ships/missing
```

```
error: transport error: connection refused
  url: http://127.0.0.1:9/nope
```

```
error: request timed out after 1s
  url: http://127.0.0.1:4000/_debug/slow?ms=3000
```

```
error: unreadable session file .curlyfries/session.json
  reason: unsupported version 2 (expected 1)
  hint:  delete the file, or press x in the terminal UI to clear the captures
```

## Tests

```bash
cargo test
```

Unit tests cover templating, expressions, schema validation, project discovery,
the session store and the TUI state machine; `tests/execute.rs`, `tests/cli.rs`,
`tests/tui.rs` and `tests/runner.rs` run against a dependency-free stub HTTP
server (`tests/support/mod.rs`) on an ephemeral port, so no test touches the
network. `tests/tui.rs` drives the interface through `ratatui`'s `TestBackend`,
and `tests/cli.rs` covers the launcher surface (help, version, project errors,
`init` and `import`) only.

## More

`docs/format.md` documents the manifest, request, environment and session file
formats, the templating rules and the output-expression grammar.
