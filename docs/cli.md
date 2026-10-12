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

## Fetching into the cache

```console
$ odx aggregate --fetch-only                  # every repository
$ odx aggregate --fetch-only --repo pos       # one repository
$ odx aggregate --fetch-only --prefer-mirror  # locked commits from the private mirror first
```

`--fetch-only` brings the base branch and every merge (pull requests,
commits, branches from other remotes) of each repository into a shared
cache: one bare repository per URL, fetched once per run however many
repositories point at it. Fetches run in parallel, at most `--jobs` (or
`jobs`) at a time and at most `[tool.odx.http].per_host_concurrency` per
host. Ctrl-C stops fetches that have not started; running ones finish so no
repository is left half-updated.

The cache lives in `cache_dir` from `[tool.odx]`, else `$ODX_CACHE_DIR`,
else the platform cache directory (`$XDG_CACHE_HOME/odx` or
`~/.cache/odx` on Linux, `~/Library/Caches/odx` on macOS,
`%LOCALAPPDATA%\odx\cache` on Windows). Concurrent odx processes take
turns per repository through an advisory lock.

## Using your local clones

If you keep clones of the repositories you work on (for example every OCA
repository under `~/git/OCA`), point odx at them in your user configuration
(`~/.config/odx/config.toml` on Linux, `~/Library/Application Support/odx/`
on macOS, `%APPDATA%\\odx\\` on Windows, or `$ODX_CONFIG_DIR`):

```toml
[clones]
dirs = ["~/git/OCA", "~/git/company"]
mode = "reference"     # or "update"
```

- **reference** (default): the cache borrows objects from the matching clone
  (git alternates), so repositories you already have cost almost no extra
  disk. Your clones are never written.
- **update**: base branches are also fetched into the clone's
  remote-tracking refs (`origin/17.0`), so one download keeps both current.
  Local branches and working trees are never touched; update them with
  `git merge` or `git pull` as usual, which is instant because the objects
  are already there.

Clones are matched by remote URL in any spelling (https, ssh, scp-style).
If you delete or prune a clone the cache borrows from, the next fetch of
that repository stops with a message naming the cache repository to delete;
it is rebuilt on the following run.
