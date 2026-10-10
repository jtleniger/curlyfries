# curlyfries on-disk formats

Everything curlyfries knows lives in files you can read, diff and review.

## Project root and manifest

The project root is the current directory (or `-C <dir>`) when it contains
`curlyfries.json`; parent directories are never searched. The manifest is
optional in spirit — an empty `{}` marks the root — but every key it may hold is
listed here:

| key | type | default | meaning |
|---|---|---|---|
| `requestsDir` | string | `"requests"` | directory scanned for request files, relative to the root |
| `environmentsDir` | string | `"environments"` | directory holding environment files, relative to the root |
| `defaultEnvironment` | string | none | environment the UI opens on |
| `followRedirects` | boolean | `false` | follow same-origin `3xx` redirects |
| `followSymlinks` | boolean | `false` | follow symlinks in the request and environment directories |
| `timeout` | number | `30` | per-request timeout in seconds; `0` disables timeouts |

Unknown keys, wrong types and non-object documents are errors
(`error: invalid request file <path>` / `at: <pointer>` / `reason: …`).

## Request files

Every `*.json` file under `requestsDir` is one request. Directories whose name
starts with `.` are skipped, as are files that do not end in `.json`, and
symlinked entries are skipped unless `followSymlinks` is `true`. The
request **id** is the path relative to `requestsDir` with the `.json` suffix
removed, using `/` separators: `requests/ships/create.json` is `ships/create`.
Ids are listed in sorted order.

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
  "outputs": {
    "shipId": { "value": "response.body.id" },
    "shipName": { "value": "response.body.name" }
  }
}
```

| key | type | required | notes |
|---|---|---|---|
| `name` | string | no | display name; defaults to the id. A literal empty string is rejected when the file is loaded, and a templated name must not render empty either. |
| `method` | string | yes | case-insensitive, normalised to upper case. One of `GET POST PUT PATCH DELETE HEAD OPTIONS`; anything else is an error listing all seven. |
| `path` | string | yes | may be templated. If, after templating, it starts with `http://` or `https://` it is used verbatim, otherwise it is joined to the `baseUrl` variable. |
| `headers` | object of string → string | no | names and values may be templated. Order is preserved. |
| `body` | any JSON | no | templated recursively, object keys included, then serialised compactly. `"body": null` means *no body*. A body without a `Content-Type` header gains `Content-Type: application/json`. |
| `outputs` | object of name → entry | no | every entry is `{ "value": "<expression>" }`; add `"secret": true` to keep the capture out of the UI. The expression may use the `[]` map step to capture a whole array (see [Outputs](#outputs-expressions)). Order is file order; keys are unique by construction. |

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

`${name[]}` is a *pick* token rather than a variable: it names an array variable
whose chosen element is substituted when the request is sent (see
[Picks](#picks)). The base name follows the same rule, so `${[]}` and `${a[]b}`
are still `invalid variable name` failures.

Parse failures are reported when a request file is loaded — the UI loads every
request at startup:

| situation | message |
|---|---|
| `${` with no later `}` | `error: unterminated `${` in <file>` |
| `${}` or `${ }` | `error: empty variable name in <file>` |
| `${1x}`, `${a b}`, `${a/b}`, `${[]}`, `${a[]b}` | `error: invalid variable name in <file>` |

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
example request in `examples/pirate` relies on exactly this. A whole `${name[]}`
pick behaves the same way, sending the chosen element with its own type.

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
The built-ins are consulted only when the session store and the environment
both miss, so either can shadow one to pin a value:

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

Template syntax is validated when a request is loaded, but variables are looked
up when the request runs, so `${token}` is legal in a file that no environment
can satisfy — until you run it. Random names parse like any other (they match
`[A-Za-z_][A-Za-z0-9_.-]*`) but are generated when the request runs.

## Outputs (expressions)

```
expr  := "response" "." ( "status" | "body" path* | "headers" hkey )
path  := "." ident | "[" integer "]" | '["' chars '"]' | "[]"
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
| `response.body.ships[].id` | an array of every `id`: `[4, 7, 9]` |
| `response.body.ships[]` | the array itself |
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
- `[]` is a *map* step: it applies the remaining steps to every element of an
  array and collects the results, so `response.body.rows[].cells[]` nests. A map
  over a non-array fails with
  `cannot map over a string with [] (expected an array)`; the first failing
  element aborts the whole expression and errors are not prefixed with its index.

Expression *syntax* is validated at load time; *evaluation* happens after the
response arrives. A failed output means the whole capture is refused: no
variable from that request is stored, and the UI shows the failure in its own
**Output error** section.

Every `outputs` entry is an object: `{ "value": "<expression>" }`, or
`{ "value": "<expression>", "secret": true }` to mark the *captured value*
secret, so a token is stored and usable but never echoed (see
[Secret values](#secret-values)). `secret: false` is allowed and behaves like the
absent flag. A missing `value`, a non-boolean `secret` or any other key is a
schema error naming the offending key by JSON pointer, e.g.
`unknown key `nope` (allowed: secret, value)`.

## Picks

A `${name[]}` placeholder in a request is a *pick* token: the request is not sent
until you choose one element of the array variable `name`. The resolved request
pane keeps the placeholder — it shows `${shipClasses[]}`, not a value — and
pressing `Enter` on that request opens a picker listing the array's elements,
strings printed bare and anything else as compact JSON. `↑`/`↓` move the
highlight, `Enter` takes the element and sends the request, `Esc` closes the
picker without sending.

The chosen element is substituted into the request that is actually sent, with
the same typing as any other placeholder: a whole `${name[]}` sends the element
with its JSON type intact, and inside a longer string it uses the
[string form](#whole-placeholders-and-string-form). Picks are looked up under the
key `name[]`, ahead of the session store and the environment, so a pick never
shadows a variable of the same name.

Every `Enter` prompts again: choices are not remembered in the session file.

A pick that cannot be offered fails before anything is sent:

| situation | message |
|---|---|
| no variable `name` in any scope | `no array `name` for `${name[]}`` |
| the value is not an array | `variable `name` is not an array` |
| the array is empty | `array `name` is empty` |
| `name` is a secret | `cannot pick from secret variable `name`` |

Outside the terminal UI nothing can fill a pick slot, so a request that reaches
one fails with `error: no value chosen for `name[]` in <file>` (exit `3`).

## Environments

Environment files live in `environmentsDir` and are named `<name>.json`. The
name must match `^[A-Za-z0-9][A-Za-z0-9_-]*$`; any other stem is a schema error
naming the file. Files that are not `*.json` and dot-files are ignored.

```json
{
  "baseUrl": { "value": "http://127.0.0.1:4000" },
  "limit": { "value": 2 },
  "pirate": { "secret": true, "value": { "username": "silver", "password": "piecesof8" } }
}
```

The document must be a JSON object; every entry becomes a variable whose `value`
may hold any JSON type. Every entry is an object with a required `value` key and
an optional boolean `secret` key: `{ "value": <any JSON> }`, or
`{ "value": <any JSON>, "secret": true }` to keep the terminal UI from echoing it
(see [Secret values](#secret-values)). `secret: false` is allowed and behaves
like the absent flag. A missing `value`, a non-boolean `secret` or any other key
is a schema error naming the offending key by JSON pointer, e.g.
`unknown key `password` (allowed: secret, value)`. Because the entry is always an
object, a literal object value is nested under `value`:
`{ "value": { "secret": 1 } }` stores the object `{ "secret": 1 }`.

An array value is a set of choices for a [pick](#picks):
`"shipClasses": { "value": ["galleon", "frigate"] }` lets a `${shipClasses[]}`
slot offer those two.

Values are **literal**: environment files are never templated, so resolution
stays one pass with no cycles. A derived value belongs in the environment file,
in a request, or in a capture.

Which environment is used:

1. `defaultEnvironment` from the manifest;
2. otherwise the only environment file, if exactly one exists;
3. otherwise none — a missing or empty `environmentsDir`, or several candidates
   with no default. The UI then opens on the `(none)` scope and `e` picks one.

## Variable lookup

| priority | scope | written by |
|---|---|---|
| 1 | session store: persisted captures plus captures from this invocation | `outputs` |
| 2 | selected environment | `environments/<name>.json` |

A miss in every scope is `error: undefined variable `<name>` in <file>` with the
`at:` pointer, the `value:` string and the known names per scope:

```
error: undefined variable `token` in requests/auth/me.json
  at:    /headers/Authorization
  value: "Bearer ${token}"
  known: environment `dev` → admiral, baseUrl, limit, pirate
         session           → (none)
```

## Secret values

A secret is display-only. The value is substituted, sent and persisted exactly
like any other; only the UI withholds it, printing the fixed mask `••••••` in its
place. Secrets come from two places:

- an environment entry written `{ "value": …, "secret": true }`; and
- an `outputs` entry written the same way, which makes the *captured* value
  secret.

In the resolved request pane every slot is rendered through the mask, so a
secret base URL, path segment, header or body value shows as `••••••` there; the
**definition** pane shows the request file as written, and the response pane is
server data and is never masked. In the captures strip a secret capture prints as
`name = ••••••`. When a session capture and an environment entry share a name,
the session capture wins — including its secret flag.

## Session file

Captured variables are persisted in `<root>/.curlyfries/session.json`:

```json
{
  "version": 3,
  "updatedAtUnix": 1759276800,
  "varScopes": {
    "dev": {
      "token": { "value": "eyJ…", "secret": true },
      "shipId": { "value": 6 }
    }
  }
}
```

- Scope keys are environment names, or `(none)` when no environment is selected.
  `(none)` cannot collide with an environment because parentheses are not valid
  in an environment name.
- Every capture is written in the same object form as an environment entry:
  `{ "value": … }`, or `{ "value": …, "secret": true }` for a secret capture.
  Secret captures are stored in plaintext, so a chain that uses a captured token
  still resolves after a restart.
- `version` must be `3`, `updatedAtUnix` is a Unix epoch second and every
  `varScopes` value must be an object. Unknown keys, an unknown version and
  malformed values are errors — never silent discards — that suggest clearing
  the captures from the UI:

```
error: unreadable session file .curlyfries/session.json
  reason: unsupported version 2 (expected 3)
  hint:  delete the file, or press x in the terminal UI to clear the captures
```

- On start, the active scope's persisted variables seed the session store. After
  each successful request whose captures are non-empty, the captures are merged
  into the scope and the file is rewritten atomically (temp file + rename).
- A write failure prints `warning: could not write session file <path>: <reason>`
  on stderr and leaves the exit code alone.
- In the UI, `x` clears the active scope and `X` clears every scope; each is
  behind a `y/n` confirmation. A missing file is an empty store, not an error.

## Exit codes

| code | meaning |
|---|---|
| `0` | the UI exited cleanly |
| `1` | filesystem or environment: project not found, missing requests directory, IO, transport, timeout, refused redirect, not a terminal, unreadable session file |
| `2` | CLI usage error (from the argument parser) |
| `3` | project or request definition: invalid JSON, schema violation, template error, undefined variable, unfilled pick, output evaluation, unknown request id |

## Worked example: the Pirate API

`examples/pirate` exercises `sample-api/` end to end:

| request | what it shows |
|---|---|
| `auth/login` | `body: "${pirate}"` (a whole-placeholder object, and a secret environment value) and capturing `token` (secret) and `role` |
| `auth/login-admiral` | a second login capturing `admiralToken` (secret) for the admiral-only routes |
| `auth/me` | `Authorization: Bearer ${token}` resolved from the session |
| `ships/list` | `limit=${limit}` from the environment plus a header capture (`shipCount`) and an array capture (`shipNames` = `response.body[].name`) |
| `ships/by-class` | `/ships?class=${shipClasses[]}&limit=${limit}`: `Enter` opens a picker over the environment's `shipClasses` array, and the response's ship ids are captured as an array (`classShipIds` = `response.body[].id`) |
| `ships/detail` | nested capture `response.body.pirates[0].name` |
| `ships/create` | `Authorization` + an inline body built from `${random.string.8}` and `${random.int.1.500}`, capturing `shipId` and `shipName` |
| `ships/delete` | `DELETE /ships/${shipId}` with the admiral token |
| `debug/echo` | proves typed substitution: `/_debug/echo` reports `"typed":42`, `"limit":2` and a `${random.bool}` flag with their JSON types, with `content_type: application/json` |
| `debug/slow` | the manifest `"timeout"` (`"timeout": 1` makes a slow request fail with `timed out after 1s`) |
| `ships/missing` | a `404` is a normal result, shown like any other status |

`ship.name` is unique on that API, so `ships/create` derives a fresh name from
`${random.string.8}` on every run and can be run repeatedly. To see the
uniqueness rule, give the request a fixed `"name"` and run it twice: the second
`201` becomes a `409` (duplicate name). That error envelope has no `id`, so the
capture refuses to invent one, and the UI shows the refusal in its own
**Output error** section:

```
error: output `shipId` failed in requests/ships/create.json
  expression: response.body.id
  reason: no key `id` in object (available: error, requestId)
```

The loud answer, rather than a silently missing `shipId`. From the UI, open the
project, select `auth/login` and press `Enter`, then run the chained requests
(`ships/detail`, `ships/list`, `ships/by-class`, …) the same way. Captures
persist in `.curlyfries/session.json` between runs.
