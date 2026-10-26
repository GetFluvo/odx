# Upgrading and porting

Moving a project or single changes between Odoo series: `upgrade`, `port`
and `migrate`.

## Preparing a series upgrade

```console
$ odx upgrade --target 18.0          # every addon of the project on 18.0
$ odx upgrade --only-missing         # what has no 18.0 version yet
$ odx upgrade --no-lint              # skip odoo-lint's readiness counts
```

`odx upgrade` looks at every addon the project uses: its own addons (under
`project_addons`), the selected addons of aggregated repositories, and the
addon packages in `uv.lock`. For each one it reports whether the target
series has it:

- `released`: the package index has a release for the target series;
- `available`: the addon is on the target branch of a repository odx has in
  its cache, `[tool.odx.index.overrides]` names its repository, or a
  repository of an organization in `[tool.odx.index] orgs` has it;
- `migration PR #n`: an open `[MIG]` pull request for the target series in
  the repository the addon lives in now;
- `missing`: none of the above, so the team migrates it.

An addon that moved to another repository is shown with both repositories,
and one renamed or merged on the way (OpenUpgrade's rename tables, plus
`[tool.odx.index.renames]`) under its new name. The project's own addons
get odoo-lint's upgrade counts (`odl upgrade-check`; `ODX_ODL` names the
executable): how many changes are automatic, need review, or are manual.
The target defaults to `[tool.odx.upgrade] target_odoo_version`.

Outside a project, in a plain clone of an addons repository, `odx upgrade
--target 19.0` looks at the addons in the directory instead: each gets
odoo-lint's counts, and every dependency they have outside the repository
(Odoo's own addons aside) is `released`, has a `migration PR #n`, or is
`missing` on the target series; `not checked` with `--offline`. The source
series comes from the addons' versions (`18.0.1.0.0`), else it is the one
before the target.

`[tool.odx.index] orgs` lists organizations (GitHub names such as `"OCA"`,
or URLs such as `"https://gitlab.com/group"` or `"https://forge.example.com/org"`)
whose repositories odx reads when an addon is still missing: the addons on
the series branch of every repository they own (on GitHub, which needs a
token for this, the directories at the root of the branch, apart from
hidden ones and `setup`; GitLab groups include their subgroups). `sources` without `"forge"`
turns this off.

The answers of the package index and the organization listings are cached
per series in the cache's `index/` directory for `[tool.odx.index] ttl`
(default 7 days); `--refresh-index` asks again, `--offline` uses what is
cached.

## Porting changes between series

```console
$ odx port pr OCA/pos#123 --to 19.0              # one pull request
$ odx port addon pos_x --from 18.0 --to 19.0     # what pos_x lacks on 19.0
$ odx port --continue                            # after resolving a conflict
$ odx port --abort
$ odx port pr OCA/pos#123 --to 19.0 --open-pr    # then push it and open a draft PR
```

Both run in a clone of the repository and cherry-pick (`git cherry-pick -x`)
onto a new branch of the target series, `<to>-port-<n>` or
`<to>-port-<addon>`: authors and dates are kept, you are the committer, and
each commit says where it was picked from. `odx port pr` asks the forge for
the pull request's commits and fetches its head from the remote that points
at the repository (`--remote` to name another). `odx port addon` runs the
analysis of `odx suggest` on the clone: the changes the addon has on
`--from` and lacks on `--to` (by patch-id), grouped by pull request; it
lists what it ports and what it skips (migration commits, repo-wide
changes, translations, blacklisted pull requests) and ports the rest.

Then odoo-lint's upgrade fixes for the target series run on every addon the
commits touch, each in a commit `[FW] <addon>: apply <to> migration fixes`
(`--no-migrate` leaves them out), and what odoo-lint leaves to fix by hand
is listed. A conflict stops at git's cherry-pick: resolve it, `git add` the
files, then `odx port --continue`; `odx port --abort` restores what was
checked out and deletes the branch. With `--open-pr` the branch is pushed
and a draft pull request `[<to>][FW] ...` opened the way `odx migrate pr`
does (`--push-remote` to push to a remote of your choice); its description
lists the ported pull requests and the remaining findings.

```console
$ odx port blacklist pos_x OCA/pos#123 --reason "superseded on 19.0"
$ odx port blacklist pos_x orphaned --reason "migration commits"
```

`odx port blacklist` adds an entry to the addon's oca-port blacklist,
`.oca/oca-port/blacklist/<addon>.json`, written byte for byte as oca-port
writes it, and commits only that file on the branch checked out (`[FW]
<addon>: blacklist 1 port suggestion(s)`). The reason is mandatory. Once the
entry is on the target branch upstream, `odx port addon` and `odx suggest`
skip what it names.


```console
$ odx port sweep --to 19.0 --from 16.0,17.0,18.0
$ odx port sweep --to 19.0 --from 18.0 --addon 'pos_*' --write-blacklist --open-pr
```

`odx port sweep` is the maintainer's check before a release: every addon on
the target series against each source series, with what reached the target
or a newer source series counted as present. It lists the portable pull
requests (port them with `odx port addon`) and the false positives:
migration commits, repo-wide changes and translation updates. With
`--write-blacklist` it blacklists those false positives with their reason,
one commit per addon on a branch `<to>-sweep` of the target series;
`--open-pr` pushes it and opens a draft pull request whose description lists
the entries and their kinds, with a marker a later sweep finds it by.

## Migrating an addon

```console
$ odx migrate addon my_addon --to 19.0          # from 18.0, on branch 19.0-mig-my_addon
$ odx migrate addon my_addon --to 19.0 --from 17.0 --remote oca
$ odx migrate --continue                         # after resolving a conflict
$ odx migrate --abort                            # back to where you were
$ odx migrate pr                                 # push the branch, open a draft PR
$ odx migrate addon my_addon --to 19.0 --open-pr # all at once
```

`odx migrate addon` runs in a clone of the addon's repository and follows
the OCA migration guide on a new branch `<to>-mig-<addon>` made from the
target series:

1. When an open pull request on the remote's forge already migrates the
   addon (`[MIG]` and the addon's name in its title), odx stops and names
   it: the work belongs there. `--force` migrates anyway; `--offline` does
   not ask.
2. The addon's history from the source series is replayed with `git
   format-patch --keep-subject | git am -3 --keep`: only the commits that
   touch the addon, with their authors and dates.
   Administrative commits are then squashed, as OCA asks of a migration:
   bot commits (`[BOT] post-merge updates`, regenerated README and `.pot`,
   by the OCA bots) go into the real commit they follow, and Weblate
   commits into the nearest earlier one of the same language by the same
   translator, authors and dates kept. One that would conflict is kept as
   it is, with a warning. `--no-squash` keeps the history as replayed;
   `[tool.odx.port] squash_authors` and `squash_subjects` name other bots.
3. `[IMP] <addon>: pre-commit auto fixes`: the repository's pre-commit hooks
   on the addon's files, when it has a `.pre-commit-config.yaml`; the commit
   only exists when they change something (`--no-pre-commit` leaves this out;
   `ODX_PRE_COMMIT` names the program).
4. `[MIG] <addon>: Migration to <to>`: odoo-lint's upgrade fixes for every
   step between the series, unsafe ones included (`odl upgrade-check --fix
   --unsafe-fixes --bump-version`, `ODX_ODL` names the program), the
   manifest version set to `<to>.1.0.0`, the old `migrations/` scripts
   removed, and pre-commit run again.

The summary also says how long each phase took (history, squash,
pre-commit, odoo-lint): the first time pre-commit meets a configuration it
builds its hook environments, which can take a minute; later runs reuse them.

Dependencies of the addon that the target series does not have yet (not in
Odoo, not on the target branch, no release on the package index) are listed
with their open migration pull request upstream, when there is one.
`--pin-unreleased`, or `odx migrate pin` later on the branch, pins them the
OCA way: a line `odoo-addon-<name> @ git+<repository>.git@refs/pull/<n>/head#subdirectory=<name>`
in `test-requirements.txt`, in a commit of its own titled `[DON'T MERGE]
test-requirements.txt`, so it can be dropped once the dependency is merged
and released. `odx migrate pr` then adds a `Depends on:` checklist of those
pull requests to the description. OCA CI's check for unreleased
dependencies stays red while the commit is there; that is expected.

`odx migrate unpin` is the return trip, run when you want to know (odx
watches nothing in the background). It says where each pinned pull request
stands (open, merged but not released for the target yet, merged and
released, or closed without being merged), and once every one is merged
and released it drops the `[DON'T MERGE] test-requirements.txt` commit, or
removes the pins in a commit `[REM] test-requirements.txt: pins of released
dependencies` when the commit cannot be dropped cleanly. `--check` only
reports. `--push` then sends the branch where its pull request comes from,
replacing it only when it has nothing the branch lacks; the checks of the
pull request on the branch's head follow. `--merge` merges the pull request
(rebased, where the forge lets odx choose) only when no pin is left, the
branch is pushed and its checks are green: nothing is merged without it.
On OCA, maintainers merge with `/ocabot merge` instead. The pull request is
the one `odx migrate pr` or `odx migrate update` recorded on the branch, or
`--pr <owner>/<repo>#<number>`; the target series comes from the branch
name, or `--to`.

```console
$ odx migrate unpin --check
$ odx migrate unpin --push
$ odx migrate unpin --merge
```

odx then lists what odoo-lint leaves to fix by hand. Nothing is pushed: you
review the branch, finish the migration and run the tests, then `odx migrate
pr` pushes it and opens the draft pull request. Both series are fetched from `--remote` (default `upstream` when
the clone has it, else `origin`); `--from` defaults to the series before
`--to`. A conflict while replaying the history stops at git's own `am`
state: resolve it, `git add` the files, then `odx migrate --continue`.
`odx migrate --abort` checks out what was checked out before and deletes
the branch.

```console
$ odx migrate plan --to 18.0
```

In a project, `odx migrate plan` says what moving to the next series takes,
addon by addon: what is already available there, which addons have an open
migration pull request to review, test and contribute to instead of
migrating them again, and what is left to migrate, dependencies first (from
the manifests the aggregated repositories have); the project's own addons
apart, as they are migrated in place. The target defaults to
`[tool.odx.upgrade] target_odoo_version`.

Outside a project, in a plain clone of an addons repository (an OCA
repository with its new, still empty series branch, say), `odx migrate plan
--to 19.0 [--from 18.0] [--remote upstream]` plans the repository instead:
the addons on `<remote>/<from>` with a manifest, those already on
`<remote>/<to>`, those with an open `[MIG]` pull request, and the rest in
dependency order (from the manifests on the source series). In both modes, every
addon to migrate lists the dependencies the target lacks: `waits on
module_analysis: open migration pull request OCA/server-tools#3631`. `odx migrate
addon`, `odx migrate pr` and `odx port` never need a project.

```console
$ odx migrate own --to 18.0
```

`odx migrate own` migrates the project's own addons (`project_addons`) in
place, in the project's repository: for each, odoo-lint's upgrade fixes and
version bump, the old `migrations/` scripts removed and pre-commit run, in a
commit `[MIG] <addon>: Migration to <to>` when anything changed, then what
odoo-lint leaves to fix by hand. No pull request: they are yours.

`odx migrate pr`, on the migration branch, pushes it and opens the pull
request `[<to>][MIG] <addon>: Migration to <to>` against the target series:
to the migrated remote when your token may push there (a company
repository), else to your fork, which odx finds or creates (an OCA
repository); `--push-remote <name>` pushes to a remote of your choice. The
pull request is a draft (`--ready` opens it for review), and its
description lists what odoo-lint still finds as a checklist. `odx migrate
addon --open-pr` does this right after the migration.

```console
$ odx migrate update                  # on the migration branch checked out
$ odx migrate update OCA/server-tools#3631 --push
```

A migration pull request often stays open while the source series keeps
getting fixes. `odx migrate update` brings it up to date, OCA style: every
commit that touched the addon on `<remote>/<from>` since the migration is
replayed on its own, with its author (one forward commit for all of them is
not how OCA works). Bot commits are left out and Weblate commits squashed,
by the same rules as `odx migrate addon` (`--no-squash` keeps them all);
odoo-lint's fixes for the target and pre-commit then go into a commit
`[FW] <addon>: <to> fixes for the changes from <from>`, when they change
anything. odx finds where to start from the newest source commit already on
the branch (same author, date and subject); `--since <commit>` names it
when odx cannot tell, and `--from` the source series when the addon came
from another one than the series before the target. A conflict stops like `odx migrate addon`: resolve,
`git add`, then `odx migrate --continue`, or `odx migrate --abort` to go
back to where the branch was.

Named as `<owner>/<repo>#<number>`, someone else's migration pull request
is checked out as `<to>-mig-<addon>` (from its `[MIG] <addon>` title) and
updated the same way. `--push` sends the result where the pull request
comes from: your branch as `odx migrate pr` pushed it, or the author's fork
when they let maintainers edit it; otherwise open a pull request against
their branch.
