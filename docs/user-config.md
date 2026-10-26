# User configuration

Settings that belong to a person and a machine rather than to a project live
in `config.toml` in the user configuration directory:

| Platform | File |
| --- | --- |
| Linux | `~/.config/odx/config.toml` |
| macOS | `~/Library/Application Support/odx/config.toml` |
| Windows | `%APPDATA%\odx\config.toml` |

`ODX_CONFIG_DIR` names another directory. A missing file is an empty
configuration; unknown keys are errors, as in `[tool.odx]`. `~/` at the start
of a path is expanded.

```toml
[cache]
dir = "~/cache/odx"
max_size = "20GB"
register_workspaces = true

[clones]
dirs = ["~/git/OCA", "~/git/acme"]
mode = "reference"

[sync]
interval = "30m"
max_duration = "20m"
jobs = 2

[mirrors]
"github.com/OCA" = { url = "https://forge.example.com/mirrors/oca.git", prefer = true }

[verdicts]
repo = "https://forge.example.com/acme/odx-verdicts.git"

[trust]
projects = ["~/work/acme"]
```

## `[cache]`

| Key | Type | Description |
| --- | --- | --- |
| `dir` | path | The shared cache directory. Default: `ODX_CACHE_DIR`, else the platform cache directory (`~/.cache/odx`, `~/Library/Caches/odx`, `%LOCALAPPDATA%\odx\cache`). A project's `[tool.odx] cache_dir` wins over both. |
| `max_size` | string | The size `odx cache gc` works towards, e.g. `"20GB"`. Default: no limit. |
| `register_workspaces` | boolean | Register a workspace for `odx sync --all` the first time it is aggregated. Default: off. |
| `filter` | string | Accepted, not used yet. |

## `[clones]`

Local clones the cache borrows objects from; see
[Using your local clones](workspace.md#using-your-local-clones).

| Key | Type | Description |
| --- | --- | --- |
| `dirs` | list of paths | Directories holding one clone per subdirectory. |
| `mode` | `"reference"` \| `"update"` | `reference` only borrows objects; `update` also fetches base branches into the clones' remote-tracking refs. Default: `"reference"`. |

## `[sync]`

Background prefetching; see
[Prefetching in the background](workspace.md#prefetching-in-the-background).

| Key | Type | Description |
| --- | --- | --- |
| `enabled` | boolean | Whether the scheduled job is installed; `odx daemon install` and `uninstall` set it. |
| `interval` | duration | Time between scheduled syncs. Default: `"30m"`. |
| `max_duration` | duration | Longest run of one sync. Default: `"20m"`. |
| `jobs` | integer | Concurrent fetches per workspace. Default: `--jobs`, else 2. |
| `nice` | boolean | Run at low CPU and IO priority. Default: on. |
| `pause_on_battery` | boolean | Skip scheduled syncs on battery power (systemd and Task Scheduler). Default: off. |
| `api_budget` | number | Accepted, not used yet. |

## `[mirrors]`

Private mirrors that serve commits of upstream repositories for every project
on the machine, like a project's vault. Keys are a host, optionally with a
path prefix (`"github.com"`, `"github.com/OCA"`); the most specific key that
matches a repository's URL wins.

| Key | Type | Description |
| --- | --- | --- |
| `url` | string | The mirror's URL. |
| `prefer` | boolean | Fetch commits from the mirror before upstream; otherwise the mirror only serves commits upstream no longer has. Default: off. |

## `[workspaces]`

The workspaces `odx sync --all` visits, keyed by path, with `registered`
(a date) and `series`. odx maintains this table; there is no need to edit it.

## `[verdicts]`

| Key | Type | Description |
| --- | --- | --- |
| `repo` | string | URL of the private repository holding verdicts and project mutes; see [Verdicts and mutes](upstream.md#verdicts-and-mutes). |

## `[trust]`

| Key | Type | Description |
| --- | --- | --- |
| `projects` | list of paths | Project roots whose shell hooks (the `custom` platform profile) run without `--allow-shell`. |

## Forge tokens

```console
$ odx auth login forge.example.com              # prompts for the token
$ odx auth login forge.example.com < token.txt  # or reads it from standard input
$ odx auth status                               # per host of the project: where its token comes from
forge.example.com: token from the system keyring
github.com: token from GITHUB_TOKEN
$ odx auth logout forge.example.com
```

odx looks for a host's token in this order: `ODX_TOKEN_<HOST>` (the host in
capitals, other characters as `_`), the forge's usual variables
(`GITHUB_TOKEN`, `GH_TOKEN`, `GITLAB_TOKEN`, `GITEA_TOKEN`,
`FORGEJO_TOKEN`), `gh auth token` for GitHub, and finally the token `odx
auth login` stored in the system keyring (Keychain on macOS, Credential
Manager on Windows, the Secret Service on Linux). Tokens are never written
to `pyproject.toml` or the user configuration. On a machine without a
keyring, `ODX_SECRETS_FILE` names a file to keep them in instead, readable
only by its owner.

Without a token odx still works against public repositories, within the
forge's anonymous API limits (60 requests an hour on GitHub); `odx doctor`
says which hosts have none.

## Environment variables

| Variable | Effect |
| --- | --- |
| `ODX_CONFIG_DIR` | the user configuration directory |
| `ODX_CACHE_DIR` | the shared cache directory |
| `ODX_TOKEN_<HOST>` | the forge token for a host, e.g. `ODX_TOKEN_GITHUB_COM` |
| `ODX_SECRETS_FILE` | a file for tokens on machines without a keyring |
| `ODX_UV` | the `uv` executable |
| `ODX_MERGIRAF` | the `mergiraf` executable |
| `ODX_ODL` | the odoo-lint executable (`odl`) |
| `ODX_PRE_COMMIT` | the `pre-commit` executable `odx migrate` runs |
| `ODX_PYPI_URL` | the package index JSON API (default PyPI's) |
| `ODX_OPENUPGRADE_URL` | where the OpenUpgrade rename lists are read |
| `ODX_SCHEDULER` | `systemd`, `launchd` or `schtasks` for `odx daemon`, instead of the platform's |
| `CI` | when set (and not `false` or `0`), locked commits come from the vault first |

The `custom` platform profile's hooks receive `ODX_PROJECT_ROOT`,
`ODX_SERIES`, `ODX_ENVIRONMENT`, `ODX_LOCKFILE`, `ODX_REQUIREMENTS` and
`ODX_VENDOR_MANIFEST`; see [Custom platforms](wheels.md#custom-platforms).
