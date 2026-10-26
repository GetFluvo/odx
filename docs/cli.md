# Command line

Every command supports `--help`; most end with examples. This page lists the
commands and the options they share; the pages linked below explain each
command with the task it belongs to.

| Exit code | Meaning |
| --- | --- |
| `0` | success |
| `1` | operational failure: conflict, network, policy |
| `2` | usage or configuration error, such as a missing `[tool.odx]` project |

## Global options

| Option | Effect |
| --- | --- |
| `-C, --project-dir <DIR>` | run as if started in `DIR` |
| `-c, --config <FILE>` | the `pyproject.toml` to use (a `repos.yaml` for the commands that read one) |
| `-j, --jobs <N>` | override the number of parallel jobs |
| `--offline` | serve git repositories and forge and index data from the cache only |
| `--fresh` | ignore cached data; refresh everything first |
| `--no-refresh` | use cached data even when stale |
| `--json` | machine-readable output on stdout; see [JSON output](json-output.md) |
| `-v` / `-q` | more or less progress on stderr |

`--offline` and `--no-refresh` save network time only: local work such as git,
pre-commit and odoo-lint takes as long as before. In a measured migration they
saved about 3 % of a ten-second run. `--no-refresh` still fetches a repository
that is not in the cache yet; `--offline` never does, and fails instead:

```console
$ odx --offline aggregate
   Failed pos: branch 17.0 of https://github.com/OCA/pos is not in the cache; fetch it once without --offline
```

### Progress

While repositories are fetched and built, odx writes a line per repository to
standard error; standard output keeps only the result, so `--json` and pipes
are unaffected:

```console
$ odx aggregate
  Fetched pos in 2.3s
   Cached web
    Built pos in 0.4s: 2 merged, 1 up to date
    Built web in 0.1s: base only
```

`Cached` means no network was needed. `-v` adds a line when each fetch and
build starts, every source with its commit and where it came from (upstream,
a mirror, a local clone or the cache), and every merge. `-q` leaves only
errors on standard error: no progress lines and no warnings.

## Commands

| Command | What it does | Explained in |
| --- | --- | --- |
| `aggregate` | fetch and merge the configured pull requests and branches into working trees | [Building the workspace](workspace.md#building-the-working-trees) |
| `lock` | resolve every merge and package to a commit, vault it, write `odx.lock` | [Building the workspace](workspace.md#locking) |
| `add`, `drop` | add or remove a repository, merge or package in `pyproject.toml` | [Building the workspace](workspace.md#editing-pyprojecttoml) |
| `sync`, `daemon`, `cache` | prefetch into the shared cache, on a schedule, and trim it | [Building the workspace](workspace.md#fetching-into-the-cache) |
| `verify` | check that working trees, lockfiles, vendored content or installed wheels match | [Building the workspace](workspace.md#verifying-a-checkout) |
| `vendor` | install the locked packages into the addon directories | [Building the workspace](workspace.md#vendoring-addon-directories) |
| `dev` | prepare a local development workspace, or develop one package | [Building the workspace](workspace.md#development-workspace) |
| `import`, `export`, `git-aggregate` | read and write `repos.yaml` and `addons.yaml`; the gitaggregate drop-in | [git-aggregator and doodba](compatibility.md) |
| `status` | per merge and package: its state upstream and what to do about it | [Following upstream](upstream.md#upstream-status) |
| `suggest` | upstream changes on other series that touch the project's addons | [Following upstream](upstream.md#suggestions-from-other-series) |
| `dashboard`, `bot` | one view of everything to act on, and the same as an issue with pull requests | [Following upstream](upstream.md#the-dashboard) |
| `verdict` | record and share decisions about upstream changes | [Following upstream](upstream.md#verdicts-and-mutes) |
| `glue` | glue addons Odoo would install with what the project has | [Following upstream](upstream.md#glue-addons) |
| `check-deps` | compare manifest dependencies with `pyproject.toml` | [Checking addons](checks.md#checking-dependencies) |
| `lint` | Odoo-specific lint checks with project context | [Checking addons](checks.md#linting) |
| `stats` | lines of code per language | [Checking addons](checks.md#lines-of-code) |
| `sniff` | database migration scripts the merges bring in | [Reviewing a lock change](reviewing.md#migration-scripts) |
| `changelog` | release notes between two states of `odx.lock` | [Reviewing a lock change](reviewing.md#release-notes) |
| `diff` | changes between two states of `odx.lock` likely to touch the database | [Reviewing a lock change](reviewing.md#schema-changes) |
| `upgrade` | addon availability and code readiness for the next series | [Upgrading and porting](series.md#preparing-a-series-upgrade) |
| `port` | port pull requests or missing commits to another series | [Upgrading and porting](series.md#porting-changes-between-series) |
| `migrate` | migrate addons to another series, the OCA way | [Upgrading and porting](series.md#migrating-an-addon) |
| `env`, `promote` | list and compare environments; copy exact pins between them | [Environments](environments.md) |
| `build` | build and publish wheels of aggregated, unmerged and private addons | [Wheels and platforms](wheels.md#building-wheels) |
| `doctor` | check the local setup | [Troubleshooting](troubleshooting.md#checking-the-setup) |
| `auth` | store or remove forge tokens in the OS keyring | [User configuration](user-config.md#forge-tokens) |
| `completions` | print shell completions | [Installation](installation.md#shell-completions) |

