# curlyfries ➿🍟

A command-line HTTP client whose request collection *is* the filesystem. Each
request is a JSON file, environments are JSON files next to them, and captured
response values are written back into a session file so the next invocation can
reuse them without logging in again.

- **Requests as files** — `requests/ships/create.json` is the request `ships/create`.
  They live in git, review like code and nest as deep as you like.
- **Templates, not scripting** — `${token}` anywhere in a path, header or body
  picks the value up from `--var`, the session store or the environment, and
  `${random.string.8}` / `${random.int.1.500}` / `${random.bool}` mint fresh
  random values when nothing defines them.
- **Captures** — `outputs` pull values out of a response body, a header or the
  status line and keep them for later requests (in this run or a later one).
- **Loud failures** — an undefined variable, a missing output path or a broken
  `${` names the file, the JSON pointer and the value that caused it.

## Install

```bash
cargo install --path .        # ~/.cargo/bin/curlyfries
```

## Layout

```
<project root>/                  # nearest ancestor directory with curlyfries.json
  curlyfries.json                # manifest (marks the project root)
  requests/**/*.json             # one file per request, any nesting depth
  environments/<name>.json       # optional named environments
  .curlyfries/session.json       # captured variables (add to .gitignore)
```

## Quickstart against the Pirate API

The `sample-api/` directory in this repository holds a small pirate-themed REST
API used for the examples (it is not part of the crate).

```bash
node sample-api/src/server.js &                       # http://127.0.0.1:4000
cd examples/pirate
curlyfries run auth/login ships/detail ships/list
```

```
→ POST http://127.0.0.1:4000/auth/login
← 200 OK  4ms
content-type: application/json; charset=utf-8
…

captured:
  token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJwaXJhdGUtYXB…"
  role = "pirate"
→ GET http://127.0.0.1:4000/ships/1?expand=pirates,treasures
← 200 OK  1ms
…

captured:
  firstPirateName = "Mad Meg Hawkins"
```

Capturing and chaining in one invocation:

```bash
rm -rf examples/pirate/.curlyfries
curlyfries -C examples/pirate run auth/login auth/login-admiral ships/create ships/delete --json
```

Each `--json` line is one request result; `ships/create` reports
`"status":201` with `"captured":{"shipId":6,"shipName":"Curlyfries 4fQ2m8Za"}`
(the name carries a random suffix, so create can be run again) and
`ships/delete` — which uses `${shipId}` from that capture and `${admiralToken}`
from `auth/login-admiral` — reports `"status":204`.

Because captures persist in `.curlyfries/session.json`, a later invocation needs
no login:

```bash
cd examples/pirate
curlyfries run auth/me        # 200 OK: the Bearer token came from the session
curlyfries --no-session run auth/me   # exit 3: undefined variable `token`
```

## Terminal UI

Running `curlyfries` with no subcommand opens a full-screen TUI: the request
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
│                                      ││{ "username": "silver", … }           │
│                                      │╰──────────────────────────────────────╯
│                                      │╭Response — http://…/auth/login────────╮
│                                      ││← 200 OK  1ms                         │
│                                      ││ 1 Body │ 2 Headers │ 3 Raw           │
│                                      ││{ "access_token": "…" }               │
╰──────────────────────────────────────╯╰──────────────────────────────────────╯
╭Captures — dev─────────────────────────────────────────────╮
│token = "eyJhbGciOiJIUzI1NiIs…"                            │
╰───────────────────────────────────────────────────────────╯
↑↓ select · Enter run · e env · Tab body/headers/raw · v definition/resolved · / filter · ? help · q quit
```

| key | effect |
|---|---|
| `↑`/`k`, `↓`/`j` | move the selection (folders are skipped) |
| `g`, `G` | first / last request |
| `Enter`, `r` | run the selected request; the status line, body and captures update in place |
| `e` | choose the environment (`(none)` plus every `environments/*.json`) |
| `/` | filter the tree; `Esc` clears, `Enter` keeps the filter |
| `Tab`, `Shift-Tab` | next / previous response tab |
| `1`, `2`, `3` | Body / Headers / Raw |
| `v` | resolved / definition request view |
| `J`, `K`, `PageUp`/`PageDown`, `Home`/`End` | scroll the response |
| `?` | help overlay (any key closes it) |
| `q`, `Esc`, `Ctrl-C` | quit |

Runs happen on a worker thread, so the interface stays responsive; captures are
persisted to `.curlyfries/session.json` exactly as `run` does. The TUI needs a
terminal on stdin and stdout and exits `1` with
`error: interactive mode requires a terminal; use `curlyfries run <id>``
otherwise. Colours are dropped when `NO_COLOR` is set.

## CLI reference

```
curlyfries [-C <dir>] [-e <env>] [--var NAME=VALUE]... [--no-session] [--timeout <secs>]
curlyfries run <id>... [--json] [--verbose] [--fail-on-error]
curlyfries list [--json]
curlyfries session show [--json]
curlyfries session clear [--all]
```

| flag | meaning |
|---|---|
| `-C, --project <dir>` | project root, overriding upward discovery |
| `-e, --env <name>` | environment file to load (default: the manifest's `defaultEnvironment`, else the only environment file) |
| `--var NAME=VALUE` | override a variable; `VALUE` is parsed as JSON when possible, otherwise taken as a string |
| `--no-session` | ignore `.curlyfries/session.json` completely (never read, never written) |
| `--timeout <secs>` | per-request timeout in seconds, default `30`; `0` disables timeouts |

| command | behaviour |
|---|---|
| *(none)* | full-screen terminal UI |
| `run <id>...` | runs each id in order, sharing captures; ids may keep their `.json` suffix |
| `list` | loads and validates every request, printing `METHOD id` plus ` — name` when the request declares a name (or `[{"id","method","name","path"}]` with `--json`); doubles as project validation |
| `session show` | prints the active scope's variables |
| `session clear` | deletes the active scope, or every scope with `--all` |

`run --verbose` additionally prints the resolved request headers and body
(secrets included, since that is the point). `run --json` prints one JSON object
per request — NDJSON — with `request`, `name`, `method`, `url`, `status`,
`reason`, `durationMs`, `headers` (wire order), `body` (parsed JSON or `null`),
`bodyText` (raw text) and `captured`.

Human output colours the status line green/yellow/red for 2xx/4xx/5xx when
stdout is a terminal and `NO_COLOR` is unset; captured strings longer than 60
characters are truncated with `…`.

## Exit codes

| code | meaning |
|---|---|
| `0` | a response was received (or interactive mode exited cleanly) |
| `1` | filesystem or environment: project not found, missing requests directory, IO, transport, timeout, not a terminal, unreadable session file |
| `2` | CLI usage error (from the argument parser) |
| `3` | project or request definition: invalid JSON, schema violation, template error, undefined variable, output evaluation, unknown request id |
| `4` | a status of `400` or above with `--fail-on-error` |

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
  hint: add "baseUrl" to environments/dev.json, or pass --var baseUrl=http://host:port, or use an absolute URL
```

```
error: no curlyfries.json found in /tmp or any parent directory
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
  hint:  delete the file or run `curlyfries session clear --all`
```

## Tests

```bash
cargo test
```

Unit tests cover templating, expressions, schema validation, project discovery,
the session store and the TUI state machine; `tests/execute.rs`, `tests/cli.rs`,
`tests/tui.rs` and `tests/runner.rs` run against a dependency-free stub HTTP
server (`tests/support/mod.rs`) on an ephemeral port, so no test touches the
network. `tests/tui.rs` drives the interface through `ratatui`'s `TestBackend`.

## More

`docs/format.md` documents the manifest, request, environment and session file
formats, the templating rules and the output-expression grammar.
