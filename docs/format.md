# curlyfries on-disk formats

Everything curlyfries knows lives in files you can read, diff and review.

## Project root and manifest

The project root is the nearest ancestor directory (starting from the current
directory, or from `-C <dir>`) that contains `curlyfries.json`. The manifest is
optional in spirit — an empty `{}` marks the root — but every key it may hold is
listed here:

| key | type | default | meaning |
|---|---|---|---|
| `requestsDir` | string | `"requests"` | directory scanned for request files, relative to the root |
| `environmentsDir` | string | `"environments"` | directory holding environment files, relative to the root |
| `defaultEnvironment` | string | none | environment used when `--env` is absent |

Unknown keys, wrong types and non-object documents are errors
(`error: invalid request file <path>` / `at: <pointer>` / `reason: …`).

## Request files

Every `*.json` file under `requestsDir` is one request. Directories whose name
starts with `.` are skipped, as are files that do not end in `.json`. The
request **id** is the path relative to `requestsDir` with the `.json` suffix
removed, using `/` separators: `requests/ships/create.json` is `ships/create`.
Ids are listed in sorted order, and `run` accepts an id with or without its
`.json` suffix.

```json
{
  "name": "Create ship",
  "method": "POST",
  "path": "/ships",
  "headers": { "Authorization": "Bearer ${token}", "X-Trace": "curlyfries" },
  "body": {
    "name": "Curlyfries ${random.string.8}",
    "class": "frigate",
    "crewCapacity": "${random.int.1.500}"
  },
  "outputs": [
    { "shipId": "response.body.id" },
    { "shipName": "response.body.name" }
  ]
}
```

| key | type | required | notes |
|---|---|---|---|
| `name` | string | no | display name; defaults to the id. A literal empty string is rejected when the file is loaded, and a templated name must not render empty either. |
| `method` | string | yes | case-insensitive, normalised to upper case. One of `GET POST PUT PATCH DELETE HEAD OPTIONS`; anything else is an error listing all seven. |
| `path` | string | yes | may be templated. If, after templating, it starts with `http://` or `https://` it is used verbatim, otherwise it is joined to the `baseUrl` variable. |
| `headers` | object of string → string | no | names and values may be templated. Order is preserved. |
| `body` | any JSON | no | templated recursively, object keys included, then serialised compactly. `"body": null` means *no body*. A body without a `Content-Type` header gains `Content-Type: application/json`. |
| `outputs` | array of one-entry objects | no | each element is `{ "<name>": "<expression>" }`; order is preserved and duplicate names are an error. |

Unknown keys are rejected with the allowed list, e.g.

```
error: invalid request file requests/ships/create.json
  at:    /ouputs
  reason: unknown key `ouputs` (allowed: body, headers, method, name, outputs, path)
```

## Templating

Delimiter: `${name}`. Variable names match `[A-Za-z_][A-Za-z0-9_.-]*`
(`${shipName}`, `${access_token}`, `${order.total}`); names starting with
`random.` are built-ins rather than variables (see
[Random values](#random-values)). Scanning is left to right:

| input | result |
|---|---|
| `${name}` | the value of `name` |
| `$${` | the literal text `${` (the `{` is consumed by the escape) |
| `$` not followed by `{` or `${` | a literal `$`, so `$5`, `$$`, `$$x` and `$$` pass through unchanged |
| `$$${x}` | `$` followed by the literal text `${x}` |
| `}` on its own | a literal `}` |
| `${x}}` | the value of `x`, then a literal `}` |

Parse failures are reported when a request file is loaded — `list` finds them
without sending anything:

| situation | message |
|---|---|
| `${` with no later `}` | `error: unterminated `${` in <file>` |
| `${}` or `${ }` | `error: empty variable name in <file>` |
| `${1x}`, `${a b}`, `${a/b}` | `error: invalid variable name in <file>` |

Each failure also prints the `at:` JSON pointer, the `value:` string and a
`hint:`:

```
error: unterminated `${` in requests/ships/create.json
  at:    /path
  value: "/ships/${"
  hint:  write `$${` for a literal `$` followed by `{`, or close the placeholder with `}`
```

### Whole placeholders and string form

If a string's trimmed form is exactly one placeholder and nothing else, the
placeholder **is** the value, with its JSON type intact, and the surrounding
whitespace is dropped. `"body": "${pirate}"` sends the `pirate` object, and
`"limit": "${limit}"` with `limit = 2` sends the number `2` — the `/_debug/echo`
example request in `examples/pirate` relies on exactly this.

Slots that can only hold text — `path`, `name`, header names, header values and
object keys inside `body` — always use the *string form* of a value instead:

| value | string form |
|---|---|
| string | itself |
| number | JSON literal, e.g. `42` |
| boolean | JSON literal, e.g. `true` |
| array / object | compact JSON, e.g. `{"limit":2}` |
| `null` | **error** `variable `<name>` is null` — a null never becomes an empty URL segment, header or key |

Any other string is interpolated: `"Bearer ${token}"` and
`"/ships/${shipId}"` concatenate literal text with string forms. An object key
or header name that renders to `""` is an error.

### Random values

A placeholder whose name starts with `random.` is a built-in, not a variable.
The built-ins are consulted only when `--var`, the session store and the
environment all miss, so any of those can shadow one to pin a value:

| placeholder | value |
|---|---|
| `${random.string}` | 16 random ASCII letters or digits |
| `${random.string.<n>}` | `n` random letters or digits, `1 <= n <= 1024` |
| `${random.int}` | a random integer in `0..=4294967295` |
| `${random.int.<max>}` | a random integer in `0..=max` |
| `${random.int.<min>.<max>}` | a random integer in `min..=max` |
| `${random.bool}` | `true` or `false` |

A whole placeholder keeps its JSON type, so `"crewCapacity": "${random.int.1.500}"`
sends a number and `"flag": "${random.bool}"` a boolean; inside a longer string
they interpolate like any other value, as in `"Curlyfries ${random.string.8}"`.

A name in the reserved namespace that is not one of the above fails the run with
a message naming the placeholder — for example
`` invalid random value `random.int.9.2`: minimum 9 is greater than maximum 2 ``.
A name outside the namespace is an ordinary variable and still fails as
`undefined variable` when nothing defines it.

```bash
curlyfries run ships/create                                    # a fresh ship every run
curlyfries run ships/create --var random.string.8=fixed --var random.int.1.500=42  # reproducible
```

Template syntax is validated when a request is loaded, but variables are looked
up when the request runs, so `${token}` is legal in a file that no environment
can satisfy — until you run it. Random names parse like any other (they match
`[A-Za-z_][A-Za-z0-9_.-]*`) but are generated when the request runs.

## Outputs (expressions)

```
expr  := "response" "." ( "status" | "body" path* | "headers" hkey )
path  := "." ident | "[" integer "]" | '["' chars '"]'
hkey  := "." ident | '["' chars '"]'
ident := [A-Za-z0-9_-]+
```

The expression is trimmed before parsing. A bare header name may not contain `.`
or `[`; use the quoted form for unusual names (`response.headers["X-Total-Count"]`
works, `.X-Total-Count` also works).

| expression | value |
|---|---|
| `response.status` | the status code, as a number |
| `response.body` | the parsed body |
| `response.body.pirates[0].name` | a nested value |
| `response.body["odd.key"]` | a key that is not an identifier |
| `response.headers.X-Total-Count` | the first value of a header, matched case-insensitively |

Evaluation rules and their failures:

- A body that is not JSON fails with
  `response body is not JSON (content-type: <type>)`.
- A missing object key fails with the sorted available keys, at most ten:
  `no key `id` in object (available: class, crewCapacity, homePort, id…)`.
- An index out of range fails with `index [9] is out of range (length 2)`.
- `.key` on a non-object or `[i]` on non-array fails with the actual type, e.g.
  `cannot index a string with [0] (expected an array)`.

Expression *syntax* is validated at load time; *evaluation* happens after the
response arrives. A failed output means the whole capture is refused: no
variable from that request is stored, and the run exits `3`.

## Environments

Environment files live in `environmentsDir` and are named `<name>.json`. The
name must match `^[A-Za-z0-9][A-Za-z0-9_-]*$`; any other stem is a schema error
naming the file. Files that are not `*.json` and dot-files are ignored.

```json
{
  "baseUrl": "http://127.0.0.1:4000",
  "limit": 2,
  "pirate": { "username": "silver", "password": "piecesof8" }
}
```

The document must be a JSON object; every entry becomes a variable and may hold
any JSON type. Values are **literal**: environment files are never templated, so
resolution stays one pass with no cycles. A derived value belongs in a request,
in `--var`, or written out by hand.

Which environment is used:

1. `--env <name>` if given (unknown name → error listing the available ones);
2. otherwise `defaultEnvironment` from the manifest;
3. otherwise the only environment file, if exactly one exists;
4. otherwise none (`environmentsDir` missing or empty);
5. otherwise — several candidates and no explicit choice — an error listing the
   available names.

## Variable lookup

| priority | scope | written by |
|---|---|---|
| 1 | `--var NAME=VALUE` overrides | the command line (explicit input always wins) |
| 2 | session store: persisted captures plus captures from this invocation | `outputs` |
| 3 | selected environment | `environments/<name>.json` |

A miss in every scope is `error: undefined variable `<name>` in <file>` with the
`at:` pointer, the `value:` string and the known names per scope:

```
error: undefined variable `token` in requests/auth/me.json
  at:    /headers/Authorization
  value: "Bearer ${token}"
  known: environment `dev` → admiral, baseUrl, limit, pirate
         session           → (none)
```

`--var` values are parsed as JSON when they parse (`--var limit=7` becomes the
number `7`) and taken as strings otherwise (`--var q=seven`).

## Session file

Captured variables are persisted in `<root>/.curlyfries/session.json`:

```json
{
  "version": 1,
  "updatedAtUnix": 1759276800,
  "varScopes": {
    "dev": { "token": "eyJ…", "shipId": 6 }
  }
}
```

- Scope keys are environment names, or `(none)` when no environment is selected.
  `(none)` cannot collide with an environment because parentheses are not valid
  in an environment name.
- `version` must be `1`, `updatedAtUnix` is a Unix epoch second and every
  `varScopes` value must be an object. Unknown keys, an unknown version and
  malformed values are errors — never silent discards — that suggest
  `curlyfries session clear --all`:

```
error: unreadable session file .curlyfries/session.json
  reason: unsupported version 2 (expected 1)
  hint:  delete the file or run `curlyfries session clear --all`
```

- On start, the active scope's persisted variables seed the session store. After
  each successful request whose captures are non-empty, the captures are merged
  into the scope and the file is rewritten atomically (temp file + rename).
- A write failure prints `warning: could not write session file <path>: <reason>`
  on stderr and leaves the exit code alone.
- `--no-session` neither reads nor writes the file.
- `session show` prints the active scope; `session clear` removes it and
  `session clear --all` removes every scope. A missing file is an empty store,
  not an error.

## Response rendering details

- **Human output** prints `→ METHOD URL`, then `← <status> <reason>  <ms>`, then
  the response headers in wire order, then the body (pretty-printed when it is
  JSON, raw otherwise). The `captured:` block is printed only when something was
  captured, with each value as compact JSON and strings longer than 60
  characters truncated with `…`.
- **Header names** are lower-cased by the HTTP stack; values and their order are
  exactly as received. JSON output therefore contains names such as
  `content-type`, and `x-total-count`.
- **`run --json`** prints NDJSON: one object per request with `request`, `name`,
  `method`, `url`, `status`, `reason`, `durationMs`, `headers`
  (`[{"name","value"}]`), `body` (parsed JSON or `null`), `bodyText` and
  `captured`.
- **Body size**: responses are read through the HTTP library's default limit;
  curlyfries does not raise it.

## Exit codes

| code | meaning |
|---|---|
| `0` | a response was received (or interactive mode exited cleanly) |
| `1` | filesystem or environment: project not found, missing requests directory, IO, transport, timeout, not a terminal, unreadable session file |
| `2` | CLI usage error |
| `3` | project or request definition: invalid JSON, schema violation, template error, undefined variable, output evaluation, unknown request id |
| `4` | status `>= 400` with `--fail-on-error` |

## Worked example: the Pirate API

`examples/pirate` exercises `sample-api/` end to end:

| request | what it shows |
|---|---|
| `auth/login` | `body: "${pirate}"` (a whole-placeholder object) and capturing `token`, `role` |
| `auth/login-admiral` | a second login capturing `admiralToken` for the admiral-only routes |
| `auth/me` | `Authorization: Bearer ${token}` resolved from the session |
| `ships/list` | `limit=${limit}` from the environment plus a header capture (`shipCount`) |
| `ships/detail` | nested capture `response.body.pirates[0].name` |
| `ships/create` | `Authorization` + an inline body built from `${random.string.8}` and `${random.int.1.500}`, capturing `shipId` and `shipName` |
| `ships/delete` | `DELETE /ships/${shipId}` with the admiral token |
| `debug/echo` | proves typed substitution: `/_debug/echo` reports `"typed":42`, `"limit":2` and a `${random.bool}` flag with their JSON types, with `content_type: application/json` |
| `debug/slow` | the `--timeout` path (`curlyfries run debug/slow --timeout 1` exits `1`) |
| `ships/missing` | a `404` is a normal result: exit `0`, or `4` with `--fail-on-error` |

`ship.name` is unique on that API, so `ships/create` derives a fresh name from
`${random.string.8}` on every run and can be run repeatedly. The uniqueness rule
is still visible: pin the random part with `--var` and run twice, and the second
`201` becomes a `409` (duplicate name). That error envelope has no `id`, so the
capture refuses to invent one:

```
error: output `shipId` failed in requests/ships/create.json
  expression: response.body.id
  reason: no key `id` in object (available: error, requestId)
```

Exit code `3` — the loud answer, rather than a silently missing `shipId`:

```bash
curlyfries run ships/create --var random.string.8=pinned
curlyfries run ships/create --var random.string.8=pinned   # 409, exit 3
```
