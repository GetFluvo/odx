# Following upstream

What odx learns from the forges about the pull requests and branches a
project depends on, and how it reports and acts on it: `status`, `suggest`,
`dashboard`, `bot`, `verdict`, `glue` and `wanted`.

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

## Suggestions from other series

```console
$ odx suggest                         # every environment's series, from the series below and above
$ odx suggest --env prod              # one environment
$ odx suggest --series 17.0 --from 16.0,18.0
$ odx suggest --addon 'pos_*' --show-muted
```

`odx suggest` looks for changes the addons you use have on other series and
not on yours: a fix merged on 16.0 that never reached 17.0, or one already
made on 18.0. For every aggregated addon and addon package it finds the
addon's repository on each series (the addon index; an aggregated
repository is also looked in on every series), fetches the branches into the
cache, and compares the commits that touch the addon:

- a commit whose patch (taken relative to the addon's directory, so a moved
  or renamed addon still matches) is already on your series is dropped;
- the rest are grouped by the pull request that merged them (from the merge
  commit, or `(#123)` in a squashed subject); commits without one are the
  addon's `orphaned_commits`;
- each group is classified: `portable`, `migration commit` (subjects
  matching `[tool.odx.port] migration_commit_patterns`, default `[MIG]`),
  `translations` (only `i18n/*.po`), `repo-wide change` (more than
  `repo_wide_threshold` addons, default 5, or only the generated README),
  `reverted` for a change and the later `Revert "..."` of it, which cancel
  out, and `blacklisted` when the target branch's oca-port blacklist
  (`.oca/oca-port/blacklist/<addon>.json`) lists it.

Blacklisted groups are hidden unless `--show-muted`. The series looked
on default to `[tool.odx.suggest] series`, else `[tool.odx.port]
default_from`, else the one below and the one above; each environment's series (`[tool.odx.environments.env.<name>]
series`) is a target of its own. `--check-apply` replays every portable
group on the target with `git merge-tree`, without touching a worktree, and
says whether it applies cleanly or conflicts.

## The dashboard

```console
$ odx dashboard                              # every environment series, as tables
$ odx dashboard --section forward-port --section pinned
$ odx dashboard --format markdown --checkboxes
$ odx dashboard act fwd:OCA/pos#2->17.0 --mute --reason "not for us"
$ odx dashboard act pr:OCA/pos#1 --drop --reason "merged upstream"
$ odx dashboard act new:OCA/pos#5 --include
$ odx dashboard act pkg:odoo-addon-web-z@17.0.1.0.1 --follow
$ odx dashboard act "drift:test->prod:addon/sale_x" --promote --open-pr
```

One view of what needs a decision, per environment series. The drift
section compares the pairs of `[tool.odx.dashboard] environment_pairs`,
else every promotion edge between environments that are not disposable;
with both directions of a pair shown, each difference is listed once, from
the side that has more of it, and only what a promotion along the pair
would copy offers `promote`. `--section` shows only the sections named;
`[tool.odx.dashboard] sections` sets the ones shown without it.

| Section | Entries | Actions |
| --- | --- | --- |
| `pinned` | pull requests the project merges, and packages from pull requests, with their state upstream (`odx status`) | `drop` |
| `droppable` | those merged upstream | `drop` |
| `new-upstream` | open pull requests on the project's series in the repositories it aggregates whose title names an addon it uses from them | `include` |
| `forward-port` | changes from lower series (`odx suggest`) | `port`, `mute` |
| `backport` | changes from higher series | `port`, `mute` |
| `auto-update` | index releases the auto-update policy would take, or leaves for review | `follow` |
| `glue` | glue addons Odoo would install with what the project has, that it does not have (`odx glue`) | `include`, `mute` |
| `wanted` | wanted addons that are on their series or being migrated (`odx wanted`) | `include` |
| `drift` | what differs between environments (`odx env diff`), per pair | `promote` |
| `suppressed` | what blacklists, verdicts and mutes hide, with the reason | |

Every entry has a stable id (`pr:`, `new:`, `fwd:`, `bwd:`, `pkg:`, `glue:`,
`want:`, `drift:`) that `odx dashboard act` takes: `--drop` and `--include` run `odx
drop` and `odx add` (a glue addon is added as a package from its
repository's series branch), `--follow` runs `odx lock --auto-update`,
`--mute` records a mute (`--kind`, default `not-wanted`, a mandatory
`--reason`, and `--until YYYY-MM-DD` for one that expires); a muted glue
addon goes into `[tool.odx.glue] ignore` with the reason instead. `--port`
ports the change, a person's decision: its commits are cherry-picked as
`odx port` does, in a clone of the repository under the cache
(`<cache>/ports/<owner>-<repo>`, sharing the cache's objects), and
`--open-pr` then pushes the branch and opens a draft pull request.
`--promote` plans promoting a drift entry's addon along its pair (`odx
promote <addon> --from <a> --to <b> --with-code`), and with `--open-pr`
writes it and opens its pull request. With
`--format markdown --checkboxes`, every action is a box `- [ ] <!--
odx:<action> <id> -->` for the bot to read back once ticked. `--json` gives
the model.

A built dashboard is kept in the cache; for `[tool.odx.cache] ttl.pr_state`
(default 30 minutes) `odx dashboard` shows it at once, with how old it is,
and `--fresh` rebuilds it. `odx sync` rebuilds it for every workspace, and
brings the verdict repository up to date on the way.

`odx dashboard --format tui` opens the same dashboard as an interactive
view in the terminal, one tab per series and section. Left and right switch
tabs, up and down select an entry, `i`, `d`, `f`, `m`, `p` and `P` include,
drop, follow, mute (asking for the reason), port (asking for
confirmation) and plan the promotion of the selected entry, `r` rebuilds the dashboard and `q` or Esc
quits. The view is an optional feature: install odx with `cargo install
odx-cli --features tui` (the PyPI wheel leaves it out).

### The dashboard as an issue: `odx bot`

```console
$ odx bot plan                     # what a run would do, as JSON; writes nothing
$ odx bot run --apply              # the nightly cycle: checkout, boxes, routine changes, issue
$ odx bot dashboard --apply        # only the issue; ticked boxes stay ticked
$ odx bot act --from-issue --apply # only the ticked boxes (for `issues` events)
```

The bot keeps one issue per series in the project repository (the
repository of `--remote`, default `origin`), titled `odx upstream dashboard
(<series>)` and labelled `odx:dashboard`: the dashboard above, rendered with
a box per action. It finds the issue by the marker on its first line, never
by its title. Closing the issue pauses that dashboard; `--reopen` opens it
again. Text below `<!-- odx:notes -->` is a person's and is kept. A run that
changes nothing writes nothing, not even the time of the run.

Ticking a box asks the bot to act on the next run, as `odx dashboard act`
does, on a branch `odx/<kind>/<key>` made from the bot's base branch
(`[tool.odx.bot] environment`, else the branch checked out):

| Box | Pull request |
| --- | --- |
| `include` | adds the merge or package and locks it (`odx:add`) |
| `drop` | drops the override and locks (`odx:drop`) |
| `follow` | takes the index update (`odx:repin`) |
| `mute` | records the mute in `.odx/mutes.toml` (`odx:mute`); with a verdict repository it goes there directly, without a pull request |
| `promote` | the promotion of a drift entry's addon, into the next environment's branch (`odx:promote`) |

Each pull request carries `odx:bot`, its kind's label and `[tool.odx.bot]
labels`, and starts with a marker `<!-- odx:pr kind=… key=… entry=… -->`.
On the dashboard the box becomes a link to it; ticked again while the pull
request is open, the branch is rewritten and the pull request updated,
never duplicated. A box that cannot be acted on stays ticked for the next
run. `--max-prs` (default `[tool.odx.bot] max_prs_per_run`, else 10) bounds
the pull requests a run opens; the rest wait for the next run. Mutes from
the dashboard expire after `[tool.odx.bot] mute_expiry` (default 180 days).
Porting upstream is never done by the bot: the dashboard shows the
`odx dashboard act … --port --open-pr` command instead of a box.

`odx bot run` first checks out what odx.lock pins (`odx aggregate
--locked`) and verifies it (`odx verify`); a checkout that fails proposes
nothing. It then proposes routine changes on its own, one pull request per
kind for everything the dashboard found (`[tool.odx.bot] auto`, all three by
default):

| Kind | Found in | Pull request |
| --- | --- | --- |
| `drop-override` | overrides merged upstream | `odx drop` for each, then `odx lock` |
| `repin` | pinned pull requests whose head moved | `odx lock --update`, proposed only when the guard rail takes a new head |
| `auto-update` | index updates the auto-update policy allows | `odx lock --auto-update` |

Its key is a digest of the change, so a run with the same change writes
nothing, a new change updates the open pull request on its branch, and a
pull request a person closed is not proposed again for the same change. The
dashboard links the entries it takes care of. With `[tool.odx.bot]
vault_heads = true` and a `[tool.odx.mirror]`, the heads the guard rail keeps
are pushed to the mirror too, so that `odx sync` fetches them from there.

A pull request merged into the base with the promotion label
(`[tool.odx.bot] promote_label`, default `promote`) gets a promotion pull
request into each environment the base's environment promotes into: what
`odx promote --from-pr <n> --with-code` writes (`odx:promote`), proposed
once. What `--max-prs` defers is listed on the dashboard.

The bot refuses to run on a public repository, since the dashboard names
the addons and pull requests a project uses, unless `[tool.odx.bot]
allow_public = true`. It only writes to the project repository (and the
verdict repository); upstream repositories are only read. On GitHub
Actions and Forgejo Actions the issue links the workflow run.

### Running the bot nightly

The bot runs in CI, usually nightly, with an identity of its own; see
[The nightly bot](ci.md#the-nightly-bot) for the workflow, the identity and
the token scopes.

## Verdicts and mutes

```console
$ odx verdict add OCA/pos#1480 --addon pos_x --kind not-needed --reason "17.0 computes this itself"
$ odx verdict add OCA/pos#1501 --addon pos_x --kind not-wanted --reason "we do not use loyalty"
$ odx verdict list [--series 17.0] [--addon pos_x] [--stale]
$ odx verdict show v:17.0:pos_x:48bfbbca
$ odx verdict confirm v:17.0:pos_x:48bfbbca     # still holds after the target code changed
$ odx verdict revoke v:17.0:pos_x:48bfbbca --reason "wrong call"
$ odx verdict sync
```

A suggestion of `odx suggest` that was looked at and turned down is
recorded with a **kind** and a mandatory **reason**, keyed by the patch-ids
of its commits (and its pull request as the handle you type):

| Kind | Meaning | Default scope |
| --- | --- | --- |
| `not-applicable` | the target series' code differs; the change does not apply | organisation |
| `not-needed` | the problem does not exist on the target series | organisation |
| `superseded` | the target has another change for the same issue | organisation |
| `already-present` | the change is there by another route | organisation |
| `declined` | rejected for other reasons (quality, risk, policy) | project |
| `not-wanted` | the project does not want the functionality | project |
| `deferred` | look again later (`--until`, or when the target code changes) | project |

Organisation **verdicts** are facts about the code: they hide the
suggestion in every project of the organisation on that series. Project
**mutes** are decisions of one project (`--scope environment --env <name>`
narrows a mute to environments). Both live in a private **verdict
repository** that odx clones into the cache and pushes to after every
change:

```toml
# ~/.config/odx/config.toml
[verdicts]
repo = "https://forge.example/odoo/verdicts.git"   # must be private; odx refuses a public one
```

Files there are `verdicts/<series>/<addon>.toml` and
`projects/<project id>/mutes.toml`; the project id is `[tool.odx]
project_id`, else derived from the project's `origin` URL. Without a
verdict repository, mutes go to the project's own `.odx/mutes.toml`, and
organisation verdicts are refused.

Except for `declined` and `not-wanted` (or with `--no-recheck`), an entry
records the files the change touches as they are on the target series.
When they change, the suggestion comes back marked "previously declined"
and `odx verdict list --stale` lists the entry: `confirm` keeps it,
`revoke` withdraws it. Nothing is deleted; a revocation is recorded with
who, when and why. `odx suggest --show-muted` shows what verdicts and
mutes hide.


```console
$ odx verdict upstream v:17.0:pos_x:48bfbbca --public-reason "fixed differently on 17.0"
$ odx verdict upstream v:17.0:pos_x:48bfbbca --public-reason "..." --open-pr --yes
```

`odx verdict upstream` turns a verdict into an oca-port blacklist entry in
the addon's repository, so the maintainers and every other user stop seeing
the change as a port to make. odx writes the entry with **only** the public
reason on a branch `<series>-blacklist-<addon>` of the target series, in a
worktree of the cache, and shows both reasons; the verdict's own reason
never leaves. With `--open-pr`, odx stops there until you add `--yes`; then
it pushes the branch (to the repository, or your fork), opens a draft pull
request and records it on the verdict (`[verdict.upstream]`).

## Glue addons

```console
$ odx glue                        # glue addons the project misses
$ odx glue --fail                 # exit 1 when there are any, for CI
$ odx --json glue
```

A glue addon joins two others (`sale_x_pos_y`) and has `auto_install`:
Odoo installs it on its own as soon as the addons it joins are installed.
A project that adds whole repositories gets glue along with the rest; one
that names its addons one by one does not, since nothing depends on glue
to pull it in. `odx glue` finds the glue addons Odoo would install with
what the project installs:

- what the project installs is its own addons and its addon packages, and
  everything they depend on, as far as the manifests odx can read tell;
  Odoo's own addons are followed too when Odoo is a git source
  (`[tool.uv.sources] odoo`, locked in `odx.lock`) or a clone of Odoo or
  OCB is among the local clones;
- the glue is looked for on the series branch of every repository in the
  cache and of every local clone of `[clones] dirs` in the user
  configuration (for example all of `~/git/OCA`), without the network;
  `odx sync` and the clones keep that current;
- a glue addon is reported when every addon that triggers it (its
  `depends` for `auto_install: True`, the listed ones for a list) is
  installed, at least one of them is not Odoo's own, and the project does
  not have it yet. Its other dependencies the project lacks are listed as
  well.

```text
glue addons Odoo would install with what the project has (18.0):
  sale_automatic_workflow_payment_mode (git@github.com:OCA/sale-workflow.git, with sale_automatic_workflow, account_payment_sale)
    odx add OCA/sale-workflow@18.0 --package odoo-addon-sale-automatic-workflow-payment-mode --subdirectory sale_automatic_workflow_payment_mode
```

`odx lock` names missing glue addons in a warning, and the dashboard lists
them in its `glue` section, so the bot's issue shows glue that appears on the
series branch; a ticked `include` or `mute` there becomes a pull request.
In CI the cache holds only the repositories the project fetches, so the bot
sees glue next to the addons the project uses, not in other repositories.
Glue the project does not want is listed with why, and no longer reported:

```toml
[tool.odx.glue]
ignore = { pos_sale_extra = "the customer sells no POS orders on credit" }
```

## Wanted addons

```console
$ odx wanted add contract --series 17.0,18.0 --note "customer asks in Q3"
$ odx wanted                      # where every wanted addon stands
$ odx --json wanted
$ odx wanted remove contract
```

A customer will want an addon some day, but not yet; on the project's
series, or on the next one, it may not even exist yet. `odx wanted add`
records it in `pyproject.toml` (run again, it changes only what it is
given), and `odx wanted` says per series where it stands:

```toml
[[tool.odx.wanted]]
addon = "contract"
series = ["17.0", "18.0"]        # default: odoo_version
repository = "OCA/contract"      # default: where the addon index finds it
note = "customer asks in Q3"
```

```text
contract (customer asks in Q3)
  17.0  migrating: OCA/contract#1234 by joe, opened 2026-05-06
        https://github.com/OCA/contract/pull/1234
        odx add OCA/contract#1234 --package odoo-addon-contract --subdirectory contract
  18.0  not started; last on 17.0 in OCA/contract
```

| State | Means |
| --- | --- |
| `available`, `released` | on the series: on the series branch of its repository (merged, not released yet), or released on the package index |
| `migrating` | not on the series yet, but a migration pull request of the addon is open in its repository |
| `not-started` | neither; odx says on which series it was last, up to three series back |
| `unchecked` | odx could not ask: offline, or the forge did not answer; `--json` says why in `reason` |

The repository is where the addon lived on an earlier series, unless
`repository` names it; an addon that is new, or lived more than three
series back, needs `repository` to be watched for a migration. The open
pull requests of that repository targeting the series are read (up to ten
pages). A migration pull request says `[MIG]` in its title, or `[ADD]`
for an addon brought to the series anew, in any case.
Which addon it is about comes from the `mod:<addon>` labels the OCA bot
puts on pull requests, and the title picks among them when there are
several; without such labels, the title must name the addon as a whole
word: `[17.0][MIG] contract_sale` does not count for `contract`.

Without an open pull request, odx looks for the addon's manifest on the
series branch of that repository: a migration merged but not released yet
is `available` there. Every run asks the package index again for a wanted
addon it did not know on the wanted series, so the release is seen at once;
the rest of what the index learnt is kept for `[tool.odx.index] ttl`, and
`--fresh` asks again.

A wanted addon that is there or being migrated is listed in the dashboard's
`wanted` section, on each series it is watched on. On the project's series
it is also offered: `odx dashboard act want:contract@17.0 --include` adds
it as a package, from the migration pull request when it is not merged yet,
so the project need not migrate it itself. The bot announces each step with a comment on the dashboard issue,
once per step, since an edited issue notifies nobody; the people in
`[tool.odx.bot] notify` are mentioned:

```toml
[tool.odx.bot]
notify = ["jane"]
```

The comment for a new migration pull request reads `@jane Someone started
migrating the wanted addon contract on 17.0: OCA/contract#1234 by joe,
opened 2026-05-06.` A later pull request, or the addon arriving on the
series, makes another; `available` and `released` are one step, so a
release after the addon is on the branch makes no second comment. A dry run (`odx bot dashboard` without `--apply`) lists the
comments it would post. Outside the bot, `odx --json wanted` gives the same
states to a scheduler of your own.
