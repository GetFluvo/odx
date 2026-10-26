# Stability

What odx keeps working from one release to the next, and how it tells you
when something changes.

## Releases

odx is released as `0.0.x`. Any release can change behaviour; the release
notes then start with a **Breaking** section that says what changed and what
to do. Read it before upgrading odx in CI.

## Project configuration

The `[tool.odx]` table is strict: an unknown key is an error with its line,
so a typo never passes as a setting that does nothing.

- A new key is optional and has a default, so a configuration that worked
  keeps working.
- A removed key is still recognised: the error says which release removed
  it and what replaces it, if anything.

  ```console
  $ odx status
  error: pyproject.toml:8:1: unknown field `show_backports`, expected `sections` or `environment_pairs`
  `dashboard.show_backports` was removed in odx 0.0.9, it had no effect; list `backport` in `sections`
  ```

- A renamed key keeps its old name for at least one release, with a warning
  that names the new one.

The [configuration reference](configuration.md) is generated from the code,
so it lists exactly the keys a release accepts. The same rules apply to the
[user configuration](user-config.md).

## `odx.lock`

The lockfile carries a format `version`. odx writes it in one layout, so
locking twice without upstream changes gives a byte-identical file; golden
lockfiles in the test suite fail on any change to that layout.

| The file has | odx |
| --- | --- |
| the version it writes | reads and writes it |
| an older version | asks for `odx lock --migrate`, which rewrites it |
| a newer version | refuses, and asks to upgrade odx |

A new version comes with `odx lock --migrate` for every version before it.
Fields can be added without a new version only when an older odx can ignore
them.

## oca-port blacklists

`odx port blacklist` and `odx port sweep --write-blacklist` read and write
`.oca/oca-port/blacklist/<addon>.json` exactly as oca-port does, so a
maintainer can switch between the two tools on the same branch. This is a
compatibility with oca-port, not a format odx owns: if oca-port changes its
format, odx follows.

## JSON output

Every `--json` output has a published schema with a `schema` number; see
[JSON output](json-output.md#stability).

## Exit codes

`0` for success, `1` for an operational failure, `2` for a usage or
configuration error; see [Command line](cli.md). New failures use one of
these three.
