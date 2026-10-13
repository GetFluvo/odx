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
