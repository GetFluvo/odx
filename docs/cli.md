# Command line

Every command supports `--help`. Commands that are not implemented yet say
so, name the roadmap phase in which they land, and exit with code `2`.

| Exit code | Meaning |
| --- | --- |
| `0` | success |
| `1` | operational failure: conflict, network, policy |
| `2` | usage or configuration error |

## Global options

| Option | Effect |
| --- | --- |
| `-C, --project-dir <DIR>` | run as if started in `DIR` |
| `-c, --config <FILE>` | the `pyproject.toml` or `repos.yaml` to use |
| `-j, --jobs <N>` | override the number of parallel jobs |
| `--offline` | serve forge and index data from the cache only |
| `--fresh` | ignore cached data; refresh everything first |
| `--no-refresh` | use cached data even when stale |
| `--json` | machine-readable output on stdout |
| `-v` / `-q` | more or less logging on stderr |

## Editing `pyproject.toml`

`odx add` and `odx drop` edit `pyproject.toml` in place. Comments and
formatting outside the touched entries stay exactly as they were, and the
result is validated before it is written: an edit that would make the
configuration invalid is refused and leaves the file untouched.

```console
$ odx add OCA/pos#123                       # pull request (repository added if new)
$ odx add OCA/pos@a1b2c3d                   # commit
$ odx add OCA/pos@17.0-fix                  # branch
$ odx add https://github.com/OCA/pos/pull/123
$ odx add OCA/pos#45 --package odoo-addon-pos_vouchers --subdirectory pos_vouchers --group oca_unmerged
$ odx drop OCA/pos#123 --reason "merged upstream"
$ odx drop --package odoo-addon-pos_vouchers
```

A package goes into the project's convention: as a PEP 508 direct reference
when its dependency group already lists direct references, otherwise as a
`[tool.odx.packages]` entry. `drop --package` removes it from
`[tool.odx.packages]`, from direct references in the configured groups, and
from `[tool.uv.sources]`. Both forms of `drop` also remove the entry from
`odx.lock`; a lockfile that cannot be read stops the command before anything
is written.
