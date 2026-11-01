# Recipe configuration

A recipe's configuration fields: typed, permissioned, validated at declaration, and delivered to the
software through a channel that holds the permission. Also derived fields, a recipe's own strings,
and sandboxed templating. TCAdmin sells a graphical configuration designer and command-line builder
as premium features; here they are a core primitive, where Pterodactyl offers untyped environment
variables and a text editor.

## Contents

- [Fields](#fields)
- [Permissions](#permissions)
- [Delivery holds the permission](#delivery-holds-the-permission)
- [Secrets](#secrets)
- [How a value reaches the runtime](#how-a-value-reaches-the-runtime)
- [Managed files](#managed-files)
- [Derived fields](#derived-fields)
- [A recipe's strings](#a-recipes-strings)
- [Templating](#templating)

## Fields

```yaml
config:
  fields:
    - {key: max_players, type: integer, min: 1, max: 200, default: 20, permission: tenant,
       label: "Maximum players"}
    - {key: motd, type: string, max_length: 59, pattern: '^[^\r\n]*$', permission: tenant}
    - {key: rcon_password, type: string, secret: true, permission: operator,
       deliver: env, deliver_as: RCON_PASSWORD}
    - {key: java_heap, type: quantity, permission: system, derived: "resources.memory * 0.8"}
    - {key: jvm_flags, type: string, permission: operator, pattern: '^[-A-Za-z0-9:+=./ ]*$',
       deliver: env, deliver_as: JDK_JAVA_OPTIONS}
    - {key: eula, type: boolean, const: true, hidden: true}
```

- **Korpis generates the form**, so it matches validation exactly, both coming from one declaration.
  The panel shows every field not `hidden`, in the recipe's words; derived, fixed and `system`
  fields are shown and not offered; a secret is typed where it cannot be read over a shoulder.
- **Validation is server-side and total.** A tenant cannot set an `operator` field: not hidden in
  the interface, refused at the API.
- **Invalid intents never exist.** Configuration is validated at declaration, so an invalid value is
  a refused call, not a workload failing to start twenty minutes later.

A key is `[a-z][a-z0-9_]*`, at most 64 bytes; a `pattern` must match the whole value. A quantity is
a decimal with an optional binary (`Ki` to `Ei`), decimal (`k` to `E`) or `m` suffix, no exponent
and at most three fractional digits after scaling; a duration is `<integer><unit>` parts in `d`,
`h`, `m`, `s`, `ms`, largest first.

## Permissions

| Permission | Who may change it, as a rule the gateway evaluates |
|---|---|
| `tenant` | a holder of `workload.write` on the workload |
| `operator` | a holder of `workload.write` through a grant scoped to the parent of the workload's organization or above; for a root organization's own workloads, its own scope |
| `system` | Korpis alone, derived or fixed |

Each permission is an action on a scope the gateway can compare, so "grant-checked like everything
else" has something to check. Which fields a viewer may change is decided by the control plane,
field by field.

## Delivery holds the permission

Refusing a tenant's `jvm_flags` at the API is worthless if the value is rendered into
`/data/server.properties` and the tenant holds `workload.file.write` on `/data`: they edit the file.
A permission a file edit overrides is a displayed limit nothing enforces. So every field declares
how it reaches the software, and an `operator` or `secret` field may only use a channel the tenant
cannot write:

| `deliver` | Reaches the software as | Tenant-writable | Allowed for `operator`, `secret` |
|---|---|---|---|
| `env` | an environment variable the runtime sets | no | yes |
| `arg` | a command-line argument the runtime sets | no | `operator` only: arguments are visible in the process table |
| `managed_file` | a file in a read-only directory outside the volumes | no | yes |
| `file` (wire name `CONFIG_DELIVERY_TEMPLATE`) | a template rendered into a volume | yes | no: refused when the recipe loads |

Software reading a setting only from a file in its own data directory can still have the setting
typed, validated and in a form, but not as an `operator` field, and the loader says so rather than
letting the recipe claim a protection it cannot provide.

## Secrets

`secret: true` marks a value redacted from streams before persistence, never rendered into a
template the tenant can read, never shown in plans or effects (they record that it changed), and
readable only with `workload.secret.read`. Every read returning an intent (`GetWorkload`,
`ListWorkloads`, `WatchWorkload`, `ListIntents`, and `DeclareIntent`'s answer) withholds, from a
caller without that action, the values of fields the intent's recipe marks secret, naming the keys
in `withheld`, so a client knows a value is set without learning it; a recipe that cannot be read
withholds every value. Such a caller changes everything else by declaring with `keep`, which carries
named values from the head as they are. Only the recipe a value was set under is known to keep it
from the caller's sight: another, even one marking it secret, could hand it to a command printing it
to a console the caller reads, encoded past redaction, so keeping a value into any recipe but the
head's needs `workload.secret.read`. A clone made by a caller who may not read the source's secrets
starts with the recipe's defaults for them.

## How a value reaches the runtime

`env` and `arg` values are set by the driver at `Prepare`, from the resolved configuration the
intent stores, in declaration order. An `env` field's `deliver_as` names the variable, which two
fields may not share. An `arg` field's `deliver_as` is a template of space-separated arguments where
`{}` is replaced by the value (`--port {}` gives two arguments, `-Xmx{}` one). **A value is never
split**, so no value can add a flag the template did not write; a field needing a list of flags
delivers it through a variable the software splits itself, as `jvm_flags` goes to
`JDK_JAVA_OPTIONS`. A boolean's template without `{}` is a flag, present when true. A field with no
value sets nothing.

Values are written as text by type: strings and enums as they are, integers in decimal, numbers in
their shortest exact decimal, booleans as `true` or `false`, quantities in their base unit (`4Gi` is
`4294967296`, `500m` is `0.5`), durations in seconds (`1h30m` is `5400`). When the recipe's `entry`
names no command, the arguments replace the image's own `CMD`, as an OCI runtime does.

## Managed files

A recipe declares managed files beside its templates (`managed_files`), each rendered by the node
from a template in the recipe and shown to the workload at `/run/korpis/config/<name>`, in the
read-only directory every workload already has for its identity socket. A name is a plain file name,
unique in the recipe. A managed file's `vars` may name only `managed_file` fields, and each such
field must be named by at least one, since a value with nowhere to go is a setting the software
never sees. A managed file may render a secret: it is the node's file, read-only to the workload and
in no volume.

A fixed directory rather than a path the recipe chooses, because that directory is already mounted
read-only everywhere, so a managed file needs no mount, no path check against the volumes, and no
rule for a recipe naming `/etc/passwd`. Software reading a setting only from a fixed path elsewhere
uses a managed file only if it takes the path as an argument or its image links that path.

The agent renders every managed file each time it builds the runtime object, so a changed value
reaches the software at its next start: each template read from the recipe's files in the content
store, at most 1 MiB, rendered with only that file's fields, written whole with mode `0444` into
`config/`, and files the recipe no longer declares removed. The agent renders rather than the worker
because the engine reads only its source and context and writes by name into the node's directory,
so a template has no path to escape by. A guest receives them as copies (see
[guests](guests.md#identity-and-managed-files)). A node giving its workloads no directory refuses
such a recipe.

## Derived fields

A derived field is computed by the control plane at declaration from the intent being declared, and
the result is written into the intent like any value: a node never evaluates an expression, an old
intent keeps its value, and a rollback restores it.

```
expr    := term (('+' | '-') term)*
term    := factor (('*' | '/') factor)*
factor  := number | quantity | path | '-' factor | '(' expr ')'
         | ('min' | 'max') '(' expr ',' expr ')'
path    := 'resources.' ('cpu' | 'memory' | 'disk') ['.reservation' | '.limit'] | 'resources.pids'
         | 'config.' key
```

`resources.memory` alone means the limit if set, else the reservation; CPU is in millicores, memory
and disk in bytes. A `config.` path names only a numeric field that is not derived, so evaluation
needs no ordering and cannot cycle. A derived field is `integer`, `number` or `quantity` with
permission `system`, rounded down for integer types and checked against its `min` and `max`.
Division by zero, a non-finite result or a negative quantity refuses the declaration. Expressions
are at most 256 bytes and sixteen levels deep. A recipe author needing a conditional or a string
operation asks for an operator field instead: an expression language that branches is a program, and
recipes contain none.

## A recipe's strings

A field's `label` and `metadata.description` are messages in the recipe's locale (`metadata.locale`,
a plain BCP 47 tag, `en` if absent); `metadata.name` is an identifier, never translated. Catalogues
for other locales are files `i18n/<locale>.json` in the artifact, signed and versioned with it, each
mapping a message's key (its path in the document, `config.fields.<key>.label` or
`metadata.description`) to its translation. A message holds no argument, since the panel formats
them with no values, so `{players}` would reach a screen as a formatting failure and is refused
where the recipe loads; what remains of ICU MessageFormat is its quoting, read as the panel's
formatter reads it. A message is at most 4 KiB. The control plane refuses at resolve a catalogue
that is not a JSON object of strings, names a key the document does not show, or is for the recipe's
own locale, so a translation cannot add a string the recipe never showed, and breaks at load where
someone can fix it rather than on a screen later; `korpis` refuses to push such a recipe.

`WorkloadService.GetWorkloadRecipe`, under `workload.read`, serves the recipe the workload's
approved intent names (its head's while nothing is approved): metadata, configuration schema and
every message in one locale, the first of the caller's languages (at most 32) the recipe speaks by
exact tag or language alone, else its own; a message the catalogue lacks is given in the recipe's
own words. `korpis workload recipe` prints it.

## Templating

Egg templating could render the node's daemon token and registry credentials into a file the tenant
could read (GHSA-pfvc-3p5h-x7h6), because the template's context and the daemon's secrets were one
context. So:

- **The variable set is enumerated** in the recipe (`vars`), and nothing outside it is reachable.
- **The context is a flat, pre-resolved map** built by the control plane from declared values only,
  with no object graph, parent scope, environment or configuration object to traverse.
- **The language is logic-less**: `{{name}}`, a section `{{#name}}...{{/name}}` rendered when the
  value is present and not false or empty, its inverse `{{^name}}`, and a comment `{{! ... }}`;
  sections nest at most sixteen deep, and a template is at most 1 MiB. No loops, traversal, calls,
  file reads or includes.
- **Each value is encoded for the declared format**, written as a whole token so it can never end
  the line, string or element it was placed in; a field whose type could carry a structural
  character it cannot be encoded away from is refused at validation, as `motd`'s pattern refuses a
  newline. Without this, a tenant's `motd` with a newline would write any key into
  `server.properties`, the operator's included.
- **Volume templates are rendered by the workload's own worker**, so even a total escape reaches
  only that workload's data.

| Format | Text is written as | Refused |
|---|---|---|
| `json` | a JSON string, quotes included | nothing |
| `yaml` | a double-quoted scalar, every line break escaped | nothing |
| `toml` | a basic string | nothing |
| `env` | a double-quoted value, `\`, `"`, `$` and backquote escaped | a line break |
| `properties` | escaped, non-ASCII as `\uXXXX`, so ISO-8859-1 and UTF-8 readers agree | nothing |
| `ini` | as it is | a line break, NUL |
| `xml` | character data with the five entities | a character XML 1.0 has none for |

Numbers and booleans stand bare; an unset value is `null` in JSON and YAML, empty in TOML and env,
and nothing elsewhere. The context travels in the plan step's detail. A template cannot contain a
literal `{{`, the language having no escape for its delimiters, and a value is quoted for the
author, so a template writes `"motd": {{motd}}`, never `"motd": "{{motd}}"`.
