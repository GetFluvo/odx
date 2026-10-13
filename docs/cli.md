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

A repository fetched less than `[tool.odx.cache] ttl.refs` ago (default
15 minutes) that already holds every ref a run needs is served from the
cache without network; the summary then says `(N already cached)`.
`--fresh` fetches regardless, `--no-refresh` (and `--offline`) use whatever
the cache holds. `odx lock`, `odx aggregate --update` and `odx sync` always
fetch.

```toml
[tool.odx.cache]
ttl = { refs = "15m" }
```

Private mirrors in the user configuration serve commits of upstream
repositories for every project on the machine, like a project's vault.
They are keyed by host, optionally with a path prefix; the most specific
key wins. With `prefer = true` commits come from the mirror first;
otherwise the mirror only serves commits upstream no longer has.

```toml
# ~/.config/odx/config.toml
[mirrors]
"github.com" = { url = "https://forge.example/odoo/vault.git", prefer = true }
```

### Prefetching in the background

```console
$ odx sync --register                 # add this workspace to `odx sync --all`
$ odx sync                            # fetch this workspace into the cache now
$ odx sync --all                      # every registered workspace
$ odx sync --all --max-duration 10m   # stop starting new workspaces after 10 minutes
$ odx sync --unregister
```

`odx sync` runs `odx aggregate --fetch-only --prefer-mirror` for each
workspace, quietly and with few jobs, so a later `odx aggregate` finds
everything in the cache. It also downloads the wheels `odx vendor` installs
for the project's dependency groups into the shared wheel cache (off with
`[tool.odx.cache] prefetch_wheels = false`). `--register` records the workspace (its path,
the date and the Odoo series) under `[workspaces]` in the user
configuration (`~/.config/odx/config.toml`); comments and other settings
there are kept. One sync runs per machine at a time: a second one prints
`another odx sync is running; nothing to do` and exits `0`. Workspaces
that are not reached before `max_duration` are reported as skipped.

With `[cache] register_workspaces = true` in the user configuration, every
workspace registers itself the first time `odx aggregate` or `odx lock`
runs in it. A sync waits while a foreground `odx aggregate`, `lock`, `dev`,
`build` or `vendor` uses the same cache (each announces itself with a file
under `active/` in the cache), up to `max_duration`.

Each run writes `last_sync.json` (what happened per workspace) and appends
to `sync.log` in the cache. A workspace that fails makes the run exit `1`;
the others are still synced. The user configuration tunes it:

```toml
# ~/.config/odx/config.toml
[sync]
jobs = 2              # parallel fetches per workspace
max_duration = "20m"  # default

[cache]
max_size = "20GB"     # the limit `odx cache gc` works towards
```

### Syncing on a schedule

```console
$ odx daemon install --dry-run        # show the job files and commands
$ odx daemon install --interval 30m   # schedule `odx sync --all`
$ odx daemon status
$ odx daemon uninstall
$ odx daemon run                      # the same loop in the foreground
```

`odx daemon install` schedules `odx sync --all` as a user-level job, so it
needs no administrator rights: a systemd user timer on Linux
(`~/.config/systemd/user/odx-sync.{service,timer}`, enabled with
`systemctl --user`), a launchd agent on macOS
(`~/Library/LaunchAgents/com.getfluvo.odx.sync.plist`), and a Task
Scheduler task named `odx-sync` on Windows. The job runs at low CPU and IO
priority unless `[sync] nice = false`; with `pause_on_battery = true` it
does not start on battery power (systemd and Task Scheduler). Installing
sets `[sync] enabled = true` and the interval in the user configuration;
`odx daemon uninstall` stops the job, removes its files and sets `enabled =
false`. `odx daemon status` shows whether the job is installed and active,
the interval, the last sync and the cache size; `odx doctor` reports it too.

### Inspecting and trimming the cache

```console
$ odx cache info                 # location, size, repositories, last sync
$ odx cache gc --dry-run         # what would be removed
$ odx cache gc                   # remove repositories no registered workspace uses
$ odx cache gc --max-size 10GB   # only as many as needed to get under 10 GB
$ odx cache detach               # stop borrowing objects from local clones
$ odx cache detach pos           # one repository, by name or URL
```

A repository is in use when a registered workspace (or the project in the
current directory) aggregates it or locks a package from it. `odx cache gc`
removes unused repositories, least recently fetched first; with a size
limit (`--max-size` or `[cache] max_size`) it stops once the cache fits.
A repository another odx is fetching into is left alone.

Cache repositories borrow objects from your local clones (`[clones]`).
Before deleting or pruning such a clone, run `odx cache detach`: it copies
what the cache borrows into the cache itself (`git repack -a -d`) and
removes the link.

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

## Building the working trees

```console
$ odx aggregate                    # fetch, then merge every repository
$ odx aggregate --repo pos         # one repository
$ odx --json aggregate             # machine-readable result (schema 1)
$ odx aggregate --keep-conflicts   # leave a conflicted merge in place to inspect
$ odx aggregate --force            # discard local changes in the working trees
```

Each repository is checked out from the cache at its base branch into
`target_dir` (default `vendor/<name>`, or `[tool.odx.layout].aggregate_dir`)
and its merges are applied in order. By default each merge is rebased onto
the result so far (`merge_strategy = "rebase"`), so history stays linear and
commits that are already applied are dropped; `merge_strategy = "merge"`
creates one merge commit per merge instead (`git merge --no-ff`).
A pull request that cannot be rebased (its history merges branches whose
changes conflict when replayed one by one) is merged with a merge commit
instead; when earlier rebased steps keep a later one from merging, the
repository is built again with merge commits throughout. Either way the
summary says so (`merged with a merge commit (could not be rebased): #123`),
and the result has the same files as merging. The
working tree is a detached checkout of the shared cache, so several projects
can use the same cache without their branch names getting in each other's
way.

A conflict aborts the merge, names the files and ends the command with exit
code 1; with `on_conflict = "skip"` (per merge or for the whole project) the
merge is skipped and reported instead. A working tree with local changes, or
a directory odx did not create, is only reset or replaced with `--force`.

With `mergiraf = "auto"` (the default) [mergiraf](https://mergiraf.org) is
used as merge driver when it is installed; `"always"` requires it and
`"never"` turns it off. mergiraf resolves, for example, two pull requests
that each add a method to the same Odoo model.

```text
fetched pos: 3 refs
built pos in vendor/pos: 1 merged, 1 skipped: #456 conflicts in views.xml (1a2b3c4d5e6f)
```

## Upstream status

```console
$ odx status                      # every tracked pull request (alias: odx track)
$ odx status --sort-by status     # merged first, then closed, open
$ odx status --fail-on-merged     # exit 1 when an override can go (for CI)
$ odx --offline status            # what the cache alone tells
```

`odx status` lists every merge from a pull request (unless
`track_upstream = false`) and every locked package from one, with its state
upstream (open, merged, closed), its labels and title, and how its current
head relates to the locked commit:

- `same`: nothing changed;
- `rebased-same-patch`: rebased on a newer base without changing the
  change; `odx lock --update` adopts it;
- `changed`: new commits (counted once the cache has them);
- `orphaned`: force-pushed; the locked commit is no longer in the pull
  request and survives only in the cache and the vault;
- `not locked`: run `odx lock`.

Each line ends with advice, such as dropping an override whose pull request
was merged, or a pin the guard rail keeps. Aggregated working trees with
local changes are listed at the end. The head is compared with the cache
(`odx sync` keeps it current), so `--offline` still detects force-pushes; a
pull request the forge cannot show is reported as a warning while the others
are listed. `--json` gives the same as data.

## Locking

```console
$ odx lock                    # lock what odx.lock does not pin yet
$ odx lock --update           # resolve every branch and pull request again
$ odx lock --check            # exit 1 when odx.lock is not up to date
$ odx aggregate --locked      # build exactly odx.lock; never resolve anything
$ odx aggregate --update      # resolve again, build, and update odx.lock
```

`odx.lock` (see [Files](files.md)) records, per repository, the base commit,
every merged commit with its tree and patch id, and the resulting tree.
Commit it: two builds from the same `odx.lock` merge the same commits, even
after pull request authors push again.

`odx aggregate` uses the commits in `odx.lock` for every repository whose
configuration is unchanged since it was locked, resolves the others and
records them, much like `cargo build` keeps `Cargo.lock` current. A working
tree that already has the locked result is left alone: no fetch and no
merge. Commits are immutable, so one already in the cache is not fetched
again.

`odx lock` resolves and merges in scratch working trees inside the cache, so
the project's working trees are not touched. A repository whose
configuration changed (a different base, merges added, removed or reordered,
or a merge that was skipped and may no longer be) is locked again. With
`--update` the latest heads are taken and the changes are reported: a pull
request whose new head carries the same change is **rebased (same patch)**,
one with different changes is **changed**.

```text
resolved pos: 2 merged (1a2b3c4d5e6f)
updated pos #123: 1a2b3c4d5e6f -> 6f5e4d3c2b1a (rebased (same patch))
locked pos #456 at 0a1b2c3d4e5f
```

### The force-push guard rail

A pull request author can push a broken change at any time. `odx lock
--update` therefore does not take a moved pull request head blindly: it asks
the forge for the CI state of the new head (GitHub check runs and commit
statuses, GitLab commit statuses, Gitea/Forgejo commit statuses) and applies
`on_head_change`, set for the project or per merge:

| `on_head_change` | green | pending | red | no checks |
| --- | --- | --- | --- | --- |
| `follow-if-green` (default) | adopt | keep the lock | keep the lock | adopt |
| `follow` | adopt | adopt | adopt | adopt |
| `pin` | keep the lock | keep the lock | keep the lock | keep the lock |

A kept pull request stays at its locked commit, which the vault keeps
available, with `status = "pinned"`, the head that was not adopted, its CI
state and the date it was first kept recorded in `odx.lock`; the next
`--update` checks again. For a red head odx tells what it can: whether the
base branch fails the same checks (**likely unrelated, base also failing**)
or whether the failing checks name only addons the pull request does not
touch (**likely unrelated, other addon**). The policy still keeps the lock;
`--follow <repo>#<pr>` adopts a head deliberately. Dropping a pull request is
never automatic. When the forge cannot be asked (no token for a private
repository, rate limit), the lock is kept and a warning says why.

```text
kept pos #123 at 1a2b3c4d5e6f: upstream head 6f5e4d3c2b1a is red, likely unrelated (base also failing)
```

Forge answers are cached with their `ETag` under the cache directory and
asked for again conditionally, which costs nothing on GitHub's rate limit.

## The vault

```toml
[tool.odx.mirror]
url = "ssh://git@forge.example.com/odoo/vault.git"   # private
```

With a mirror configured, `odx lock` pushes every locked commit (the base and
each merge) to it under a ref named after the commit and nothing else,
`refs/odx/<commit>` (`ref_prefix` changes the prefix; `ref_style = "tags"`
uses `refs/tags/odx/<commit>`). The refs never move and say nothing about
the project; `mirror_ref` in `odx.lock` records them. When a pull request
author force-pushes and the forge collects the old commit, `odx aggregate`
fetches it from the vault instead. In CI (`CI` set) and with
`--prefer-mirror`, locked commits come from the vault first.

Before the first push of a run, odx asks the forge's API who can read the
vault and refuses a public one unless `allow_public = true`. GitLab protects
tags, not custom refs, so GitLab vaults always use tags. The check needs a
token for private repositories: `ODX_TOKEN_<HOST>` (the host in upper case,
other characters as `_`), or `GITHUB_TOKEN`/`GH_TOKEN` (or `gh auth token`),
`GITLAB_TOKEN`, `GITEA_TOKEN`/`FORGEJO_TOKEN`. A vault given as a local path
or `file://` URL is not checked. `odx lock --no-push`, or `push_on_lock =
false`, locks without vaulting.

With `auto_create = true`, a vault that does not exist yet is created
through the forge's API: private, in the organization (or GitLab group) its
URL names, else for the token's user, with a rule that keeps its `odx/*`
tags from being deleted or moved (a GitHub tag ruleset, GitLab protected
tags, Gitea/Forgejo tag protection). Forges protect tags, not custom refs,
so such a vault always uses `refs/tags/odx/<commit>`. The token needs the
right to create repositories. odx never creates forks for vaulting: forks of
public repositories are public.

```text
vaulted pos: 3 commits
```

## Packages

Addons installed as Python packages from git are locked too: entries of
`[tool.odx.packages]` and git direct references in the dependency groups of
`[tool.odx.groups]`:

```toml
[project.optional-dependencies]
oca_unmerged = [
  "odoo-addon-pos_vouchers @ git+https://github.com/OCA/pos.git@refs/pull/45/head#subdirectory=pos_vouchers",
]

[tool.odx.packages]
odoo-addon-private-reports = { url = "https://forge.example.com/odoo/private-reports.git", branch = "17.0", subdirectory = "private_reports", group = "private" }
```

`odx lock` resolves each to a commit and records it in `odx.lock`
(`[packages.<name>]`); until `--update`, a package keeps its commit. With a
vault configured, every package commit is vaulted as the tag
`refs/tags/odx/<commit>` (uv fetches tags and branches, not other refs) and
the project is pointed at it: a direct reference becomes
`…@odx/<commit>`, and a `[tool.odx.packages]` entry gets a
`[tool.uv.sources]` entry (and is added to its group when missing). The
original source stays in `odx.lock`, so `--update` still follows the pull
request. Then `uv lock` runs (`--no-uv` skips it; `ODX_UV` names another
executable).

The Odoo core, `[tool.uv.sources] odoo = { git = …, branch = … }`, is
recorded as `[odoo]` with the commit the branch or tag points at, asked of
the remote without downloading it; it is not vaulted (uv.lock pins it, and
copying all of Odoo into the vault would be costly).

```text
locked odoo-addon-pos_vouchers package at 1a2b3c4d5e6f
locked odoo core at 0a1b2c3d4e5f
vaulted odoo-addon-pos_vouchers: 1 commits
pointed packages at the vault in pyproject.toml
```

### Bundles as a safety net

```console
$ odx lock --bundle bundles/                     # one git bundle per repository
$ odx aggregate --locked --bundle-dir bundles/   # locked commits from the bundles first
```

`--bundle` writes one git bundle per repository URL with every locked
commit of it, named after the URL's hash (no project or client name), for
CI to keep as an artifact. The commits are also kept under
`refs/odx/locked/<commit>` in the cache. `odx aggregate --bundle-dir` takes
locked commits from the bundles before asking any remote. Bundles must be
kept as long as the `odx.lock` that references them is deployable; the
vault is the mechanism, bundles are a safety net.

## Verifying a checkout

```console
$ odx verify          # exit 1 with a list when anything differs
$ odx --json verify   # {"schema": 1, "ok": …, "problems": […]}
```

`odx verify` checks, without network access, that the checkout is exactly
what the lockfiles describe:

- every repository and package is locked as configured, and nothing else is
  locked;
- `uv.lock` resolves every locked package to its locked commit;
- no locked package is in develop mode (an editable path source);
- every working tree is the locked result, without local changes;
- no addon directory under `[tool.odx.groups] project_addons` is unknown to
  git (the build would ship it although nobody committed it).

Use it as a pre-commit hook, as the first step of a build job, and as the
last step before a deployment. `odx lock --migrate` rewrites `odx.lock` in
the current format after an odx upgrade changed it.

Credentials written into URLs (`https://user:token@host/…`) are replaced by
`***` in everything odx prints.

## Vendoring addon directories

```console
$ odx vendor                          # every [tool.odx.groups] directory from the lockfiles
$ odx vendor --requirements           # and the platform requirements file
$ odx verify --vendored               # the directories are what odx vendor wrote
$ odx verify --deployed ../deploy     # the same, for a deployment repository checkout
```

For platforms that receive addon directories (`vendored-source`, `odoo-sh`,
`custom`), `odx vendor` fills the directory of every dependency group with
the addons of the packages the group declares in `uv.lock`, and of the addon
packages they depend on:

- a package from an index comes from its wheel: odx downloads the exact
  file `uv.lock` names, refuses it when its sha256 differs, keeps it in the
  cache and copies `odoo/addons/<addon>/` out of it (metadata such as
  `*.dist-info` stays behind);
- a package from git comes from the commit `odx.lock` locked, out of the
  cache, so the vault and `odx lock` decide what is vendored;
- files matching `[tool.odx.vendor] exclude` (default
  `static/description/index.html`) are left out.

The group directories belong to odx: they are replaced on every run, which
is why odx refuses a directory outside the project or one holding a
`pyproject.toml` or `.git`. Vendoring twice from the same lockfiles writes
byte-identical files.

The platform only updates an addon when its manifest `version` changes. When
an addon's content changed but its version did not (a pull request head
that moved), odx bumps the last segment (`17.0.1.2.0` becomes `17.0.1.2.1`)
and keeps that version for as long as the content stays the same; turn this
off with `[tool.odx.vendor] bump_on_content_change = false`. What was
written, with the wheel hash or commit and content hashes, goes to the
vendor manifest `.odx/vendor.lock` (`[tool.odx.vendor] manifest`); do not
commit it.

`odx verify --vendored` fails when a vendored addon changed, is missing or
is not odx's, and when `uv.lock` or `odx.lock` moved on since `odx vendor`.
`--deployed <dir>` checks the same directories in another checkout, such as
the deployment repository before a push. The `generic-wheels` profile
installs wheels instead, so `odx vendor` only writes its requirements there.

## Odoo.sh

Odoo.sh deploys the repository as it is: addons come from committed
directories or git submodules, and Python packages only from public PyPI
through `requirements.txt` at the root. With `profile = "odoo-sh"`:

- `odx vendor --requirements` writes `requirements.txt` at the root;
- `odx verify` checks every submodule: one that `.gitmodules` names
  without a committed commit is unpinned, and a submodule at a locked
  repository's `target_dir` must pin a commit whose tree is the locked
  result (push the aggregated branch and update the submodule);
- `odx verify --requirements` also refuses lines Odoo.sh cannot install
  (`name @ git+...`, `-e`, other options).

## Building wheels

```console
$ odx build                              # every locked repository addon and package
$ odx build --addon 'pos_*' --project    # some addons, plus the project's own
$ odx build --publish                    # and upload them to the private index
```

For platforms that install from an index (`generic-wheels`), released OCA
addons already are wheels; aggregated, unmerged and private addons are not.
`odx build` makes them from the locked sources:

1. **What:** the selected addons (`addons`) of every repository in
   `odx.lock`, taken from the locked result; every package in `odx.lock`,
   from its locked commit; with `--project` the project's own addons, from
   the committed `HEAD`. An addon that comes from two sources is refused.
2. **How:** the addon is exported from the git cache into a scratch
   directory and built with `uv build --wheel` and its own backend (whool
   when it declares none, as OCA addons do), with
   `SOURCE_DATE_EPOCH` set and whool's post-version off.
3. **Version:** the wheel gets `<manifest version>+odx.<tree>`, or
   `+odx.pr<N>.<tree>` for a package from a pull request, where `<tree>` is
   the first twelve hex digits of the git tree of the addon directory. The
   label is derived from the content alone: building the same locked
   source anywhere gives the same version, and an addon whose files did
   not change keeps its version when other addons of its repository move.
   odx rewrites the wheel with fixed timestamps, so the same source gives
   the same bytes and hash.
4. **Lock:** every wheel is recorded in `odx.lock` under `[wheels.<dist>]`
   with its version, sha256, source and tree; `odx verify --installed`
   reports an installed version that differs.
5. **Publish:** `--publish` runs `uv publish --index <name>` for the built
   wheels; the index is `--index`, else `[tool.odx.platform] index`, else
   `private`, and must be a `[[tool.uv.index]]` of the project with a
   `url` and a `publish-url`. uv checks the `url` first and skips files the
   index already has, so publishing again is a no-op, and a file with the
   same name but other content fails: with content-derived versions that
   means a build that is not reproducible. PyPI is refused: it does not
   accept local versions. The index is recorded in the wheel entries.

The wheels land in `dist/wheels` (`--out-dir`). Pointing a dependency group
at the published versions is not automated yet.

## Custom platforms

```toml
[tool.odx.platform]
profile = "custom"

[tool.odx.platform.custom]
prepare = "./deploy/prepare.sh"   # after odx vendor
verify = "./deploy/verify.sh"     # during odx verify
```

For a platform odx does not know, the `custom` profile runs your own
commands: `prepare` at the end of `odx vendor`, `verify` as part of
`odx verify` (a failure fails the verification). They run with `sh -c`
(`cmd /C` on Windows) in the project root, with `ODX_PROJECT_ROOT`,
`ODX_ENVIRONMENT`, `ODX_SERIES`, `ODX_LOCKFILE`, `ODX_VENDOR_MANIFEST` and
`ODX_REQUIREMENTS` set. Because they execute commands from the project,
they only run with `--allow-shell` or when the project is listed in the
user configuration:

```toml
# ~/.config/odx/config.toml
[trust]
projects = ["~/projects/acme"]
```

Otherwise `odx vendor` warns that the hook was skipped and `odx verify`
fails.

## Installed wheels

```console
$ odx verify --installed .venv         # a virtual environment
$ odx verify --installed /srv/odoo/lib/python3.12/site-packages
```

Proof that a deployment installed what was locked. For every distribution
in the environment that `uv.lock` knows:

- the installed version is the locked one;
- a package from git was installed from the locked commit
  (`direct_url.json`);
- a package from an index holds exactly the files of the locked wheel: odx
  takes the wheel `uv.lock` names for the installed tags (from the cache,
  or downloaded and checked against its sha256) and compares its `RECORD`
  with the installed one, so a different wheel with the same version is
  found;
- no installed file changed since installation (its hash against the
  installed `RECORD`).

Distributions `uv.lock` does not know (pip, setuptools) and packages from a
local directory are not compared.

## Platform requirements

```console
$ odx vendor --requirements              # writes [tool.odx.layout] requirements
$ odx vendor --requirements deploy/req.txt
$ odx vendor --requirements --strict     # fail on a conflict with the platform
$ odx verify --requirements              # the file is current, lists nothing provided, no conflict
```

Hosting platforms install a base stack (Odoo's own requirements, database
drivers, `lxml`, ...). Declaring one of those packages again, or pinning it
differently, breaks deploys, so `odx vendor --requirements` leaves out what
the platform provides:

```toml
[tool.odx.platform]
profile = "vendored-source"          # generic-wheels (default) | vendored-source | odoo-sh | custom
provided = "odoo-standard"           # or "odoo-sh", or a constraints file in the project
provided_extra = ["rq==1.16.2"]      # what the platform image adds
provided_remove = ["python-ldap"]    # what it lacks despite the named set
```

The named sets ship with odx per Odoo series (16.0 to 19.0) and contain
the packages of Odoo's `requirements.txt`; `odoo-sh` is the same set until
the platform image's additions are collected, so add those with
`provided_extra`.

- **`generic-wheels`**: the file is `uv export --frozen` (with hashes) of
  the project and its `[tool.odx.groups]`, without provided packages and
  without Odoo itself; addon wheels stay in it.
- **`vendored-source`, `odoo-sh`, `custom`**: the file lists the external
  Python packages that what you ship needs: the non-addon requirements of
  `[project]` and the configured groups, the dependencies of every addon
  package they reach in `uv.lock`, and `external_dependencies["python"]` of
  your own addons and the selected addons of every aggregated repository.
  Each is pinned to its version in `uv.lock` and annotated with who needs
  it; import names such as `dateutil` become their distribution
  (`python-dateutil`). Odoo.sh reads `requirements.txt` from the repository
  root, which is the default path for that profile.

A requirement whose range excludes every version the platform provides is
reported with the addon that declares it (`--strict` makes that an error).
odx owns the file: hand edits are overwritten and reported, recognized by a
digest in the header.

## Automatic updates of index packages

```console
$ odx lock --auto-update
auto-updated odoo-addon-web-responsive 17.0.1.1.9 -> 17.0.1.1.10 (i18n/nl.po)
needs review odoo-addon-account-statement-base 17.0.1.6.0 -> 17.0.1.6.1: changes files outside allowed_paths: models/account_bank_statement.py
needs review odoo-addon-base-tier-validation 17.0.1.2.0 -> 17.0.2.0.0: changes more than the last version segment
```

OCA releases a new wheel for almost every merged pull request, translation
updates included. `odx lock --auto-update` looks at every addon package in
`uv.lock` that comes from an index, matches `allow` and not `deny`, and asks
the index (`ODX_PYPI_URL`, default PyPI's JSON API) for newer releases in
the project's series:

- only an update that changes the last version segment is taken
  (`policy = "last-segment"`; `"none"` turns the command off);
- with `content_check` (the default), odx downloads the locked and the new
  wheel (hash-checked, cached) and compares the addon's files: when every
  changed file matches `allowed_paths`, the update is taken, otherwise it
  is listed for review with the offending files. The version line of
  `__manifest__.py` does not count as a change.

Taken updates go through `uv lock --upgrade-package <dist>==<version>`.

```toml
[tool.odx.lock.auto_update]
policy = "last-segment"
allow = ["odoo-addon-*"]
deny = ["odoo-addon-base_tier_validation"]
content_check = true
allowed_paths = ["i18n/*.po", "i18n/*.pot", "README.rst", "readme/**", "static/description/**"]
```

## Migration scripts

```console
$ odx sniff                        # what the merges bring on top of each base
$ odx sniff --since main           # what changed since odx.lock on main (repositories and packages)
$ odx sniff --fail-on-new          # exit 1 when there are any
```

A merge that adds `<addon>/migrations/<version>/pre-*.py` (or `post-*`,
`end-*`, also under `upgrades/` and in OpenUpgrade's
`openupgrade_scripts/scripts/<addon>/<version>/`) changes the database on
the next update of that addon. `odx sniff` lists these scripts per addon with
the addon's version, and flags a version change that comes without any
migration script (informational: OCA bumps the version for every change).
Scripts that were already in the base are never reported.

Without `--since`, every locked repository is compared from its base commit
to its locked result. With `--since <rev>`, the comparison is against
`odx.lock` as committed at that revision: a repository from its previous
result (from its base when it is new), a package from its previous commit,
limited to its subdirectory. `odx aggregate` prints the same report for
every repository it builds; `[tool.odx.sniff] fail_on_new = true` (or
`--fail-on-new`) turns any script into a failure. The commits must be in the
cache, which `odx aggregate` and `odx lock` take care of.

## Checking the setup

```console
$ odx doctor
ok   git: git version 2.56.0
ok   uv: uv 0.11.16
info odl: odl not found; optional: odoo-lint for odx lint
ok   mergiraf: mergiraf resolves conflicts
ok   project: /work/acme/pyproject.toml (Odoo 17.0)
ok   token github.com: found
ok   vault visibility: https://forge.example/odoo/vault.git is private on Forgejo
FAIL vault protection: odx/* tags are not protected; protect them on the forge (or create the vault with mirror.auto_create)
ok   platform: vendored-source, provided set odoo-standard-17.0 (58 packages, 1 overrides)
info cache: /home/dev/.cache/odx (1.2 GB)
```

`odx doctor` checks the machine (git, uv, mergiraf, odl) and, in a
project, the forge tokens per host, the vault, the platform profile with its
provided set, the cache and packages left in develop mode. It exits with
`1` when something required is missing or wrong:

- the vault is public (unless `allow_public = true`), or cannot be read;
- the vault's lock refs are not protected: odx needs a protection rule that
  stops `odx/*` tags from being deleted or moved (a GitHub tag ruleset,
  GitLab protected tags, Gitea/Forgejo tag protection). Lock refs under
  `refs/odx/*` cannot be protected on any forge, so use `ref_style =
  "tags"`;
- the provided set cannot be loaded, or `mergiraf = "always"` and mergiraf
  is missing.

With `--offline` the vault is not checked; `--json` gives the same list as
data.

## Forge tokens

```console
$ odx auth login forge.example.com < token.txt   # the token comes on standard input
$ odx auth status                                # per host of the project: where its token comes from
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

## Development workspace

```console
$ odx dev                                   # addons path, odoo.conf, editable installs
$ odx dev --print-addons-path               # just the addons path
$ odx dev odoo-addon-pos_vouchers --branch fix-rounding
$ odx dev --restore                         # every package back to its locked source
```

`odx dev` collects every directory that holds addons, in the order Odoo
should search them: the project's own addons, packages in develop mode,
the aggregated repositories, then the dependency groups. It writes the list
to `[tool.odx.dev] addons_path_file` (default `.odx/addons-path`), prints
it, and sets `addons_path` in the `[options]` section of `odoo_conf` when
configured. Addons with their own `pyproject.toml` (project addons and the
selected addons of aggregated repositories) are installed in editable mode
into `venv` (default `.venv`) with `uv pip install -e`; `--no-editable` or
`editable = false` skips that, `--dry-run` only shows what would happen.

`odx dev <package>` switches a locked package to a local clone: the locked
commit is cloned from the cache into `dev_dir` (default `.src`) with the
remotes `upstream` and, when configured, `mirror`; `--branch` creates or
switches to a branch. The package's `[tool.uv.sources]` entry then points
at the clone (`{ path = ..., editable = true }`), and a direct reference in
a group becomes the plain requirement; run `uv sync` to install it. `odx
dev --restore [<package>]` puts `pyproject.toml` back exactly as it was
(the clone stays). While a package is in develop mode, `odx verify` fails,
so it cannot be locked or deployed by accident.

## Lines of code

```console
$ odx stats                        # the project's own addons, per language
$ odx stats --include-vendor       # also the vendored groups and aggregated repositories
$ odx stats --per-addon            # one row per addon
$ odx stats --json                 # tokei's per-language structure, with a total
```

`odx stats` counts the addon directories (directories with a
`__manifest__.py`) under `[tool.odx.groups] project_addons` (default: the
project root) with [tokei](https://github.com/XAMPPRocky/tokei), so the
numbers match running `tokei` on the same addon directories: `.gitignore`,
`.ignore` and `.tokeignore` are respected, hidden files are skipped, and
code embedded in another language (CSS in HTML, code blocks in Markdown)
counts with its host language. `--include-vendor` adds the dependency group
directories and every repository's `target_dir`.
