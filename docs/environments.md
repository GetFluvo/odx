# Environments

Branches per environment, comparing them and promoting addons between them:
`env` and `promote`.

`[tool.odx.environments]` says which branches are which environment, the
series each runs, and which promotions are allowed. A preset supplies the
usual layouts; `env.<name>` overrides a preset environment key by key or
adds one, and `promotion` replaces the preset's edges:

```toml
[tool.odx.environments]
preset = "major-branches"
branch_specific = [".template-answers.yml"]   # never promoted

[tool.odx.environments.env.test]
series = "20.0"                               # test already runs the next series
[tool.odx.environments.env.prod]
repo = "https://git.example.com/odoo/project-prod.git"   # a separate repository
```

| Preset | Environments (branch) | Promotions |
| --- | --- | --- |
| `major-branches` | `test` (`{major}-test`), `uat` (`{major}-uat`, disposable), `prod` (`{major}-prod`) | test → uat, test → prod, uat → prod, prod → test |
| `series-branches` | `test` (`{series}-staging`), `prod` (`{series}`) | test → prod, prod → test |
| `main-production` | `test` (`main`), `prod` (`production`) | test → prod, prod → test |
| `odoo-sh` | `dev` (any other branch), `uat` (`staging/*`), `prod` (`production`) | dev → uat, uat → prod, prod → uat |
| `explicit` | only `env.<name>` | only `promotion` |

`{series}` is the environment's series (`19.0`), `{major}` its major version
(`19`), and `*` in a branch matches anything. An environment's series
defaults to `series`, then to `odoo_version`; its branch defaults to its
name; `uat` environments are disposable unless `disposable = false` (a
disposable environment is never the default source of a promotion). Without
a `preset`, a table that declares environments is `explicit`.

```console
$ odx env list
preset major-branches
  prod  prod  19.0  19-prod  https://git.example.com/odoo/project-prod.git
* test  test  20.0  20-test
  uat   uat   19.0  19-uat   (disposable)
promotion: test → uat, test → prod, uat → prod, prod → test
branch-specific: .template-answers.yml
```

`*` marks the environment of the current branch. `odx suggest --env <name>`
and the dashboard use each environment's series.

## Comparing environments

```console
$ odx env diff test prod
test (refs/remotes/origin/19-test) and prod (refs/remotes/origin/19-prod)
newer in test:
  account_invoice_x  19.0.1.1.0 tree dcbe79a | 19.0.1.0.0 tree f13f539
  odoo-addon-sale-y  19.0.1.2.0 sha256:41c9e0a | 19.0.1.1.0 sha256:9b03f4d
changed:
  repository pos     tree 3d1f9a2 | tree 77ab0c1
only in test:
  odoo-addon-z       19.0.1.0.0 sha256:e2f6b11 | -
branch-specific files that differ (never promoted):
  .template-answers.yml
4 differ, 38 identical
```

`odx env diff` reads `pyproject.toml`, `uv.lock` and `odx.lock` of both
environments' branches through git, without checking anything out:
`<remote>/<branch>` (`--remote`, default `origin`), else the local branch; an
environment with its own `repo` is fetched from there. It compares, by
name, the project's own addons (by the tree of their folder, and their
manifest version), every package `uv.lock` resolves (by wheel hash or
commit), the aggregated repositories (by the tree they build) and the Odoo
core revision. Each is identical, newer or older (by version), changed (a
different pin, same version), from another source, or only on one side; a
series mismatch is reported first. Fetch first: odx compares what git has.

An environment or one of its branches can be named (`odx env diff
staging/sprint-4 prod` for the `staging/*` pattern of the `odoo-sh` preset).
`--vendored A B` compares two vendor manifests and `--installed A B` the
wheels installed in two virtual environments, file hash by file hash:
identical entries there are the proof that one environment runs exactly
what the other ran. `--json` lists every entry with its kind, change and
both pins.

## Promoting addons

```console
$ odx promote sale_x --to prod
promote test (refs/remotes/origin/19-test) into prod (refs/remotes/origin/19-prod), series 19.0
selection: sale_x
  sale_x  new           19.0.1.0.0 0748fce7724a9e | -
  helper  identical     19.0.1.0.0 eaa0bb8a8497d7 | 19.0.1.0.0 eaa0bb8a8497d7
  x       newer         19.0.1.1.0 sha256:41c9e0 | 19.0.1.0.0 sha256:9b03f4
not copied (Odoo's own addons, or of aggregated repositories): base, sale
core drift: odoo bbbbbbbbbbbbbb | aaaaaaaaaaaaaa (copied with the pins; --no-with-odoo leaves it)
2 to copy into prod; plan only, nothing written
$ odx promote --feature invoicing --to prod
$ odx promote --search "invoicing" --to prod                       # candidates, scored
$ odx promote --search "invoicing" --select account_due --to prod   # the plan of one
```

`odx promote` plans copying exactly what the source environment runs for
the selected addons and everything they depend on (`depends` of the
project's addons, the dependencies `uv.lock` records between addon
packages) into the target. Both sides are read through git as for `odx env
diff`. `--from` defaults to the one environment allowed to promote into
`--to` that is not disposable; the edge must be in `promotion`, and both
environments must run the same series (`odx upgrade` prepares a move to
another series).

Every addon of the closure is listed with its pin on both sides. The plan is
refused (exit 1) when the target has a newer version of a package
(`--allow-downgrade`, or `[tool.odx.promote] allow_downgrade`), has the
addon from another source (`--allow-source-change`), or has a newer version
of a project addon (sync the target into the source first). A project addon
whose content differs at the same version, and a pinned merge whose new
head is red on the source, are warnings. The Odoo core revision is part of
the plan unless `--no-with-odoo` or `with_odoo = false`.

`--search` scores the source's project addons (technical name, manifest
`name`, `summary`, `category`, `description`) and feature sets (an exact
feature name first) against the words given and their synonyms (shipped per
Odoo domain, extended by `[tool.odx.promote] search_synonyms`), and only
lists candidates; `--select` names the ones to plan. `--json` gives the
candidates or the plan.

The plan also lists the source's commits on the closure's project addons
that the target does not have (compared by patch id): the code that goes
with the pins.

```console
$ odx promote sale_x --to prod --commit --yes                 # a commit on odx/promote/prod-sale_x
$ odx promote sale_x --to prod --commit --yes --with-code     # the missing commits cherry-picked first
$ odx promote sale_x --to prod --open-pr --yes                # pushed, and its pull request opened
```

`--commit` writes the plan on `odx/promote/<to>-<selection>` (the feature
name with `--feature`), branched from the target branch in a temporary
worktree, so the checkout is left alone. For every package of the closure it
copies the requirement strings of `pyproject.toml` (into the same group),
the `[tool.uv.sources]` entry and the `uv.lock` blocks verbatim, and the
`odx.lock` entries; the Odoo core revision too unless `--no-with-odoo`. It
then runs `uv lock` there and refuses when that re-resolves anything else
(`transitive drift`; `--allow-transitive` keeps it). `--with-code`
cherry-picks the listed commits first, authors kept; a commit that does not
apply stops the promotion. Writing needs `--yes`: odx asks no questions.

Branch-specific files are never promoted: what cherry-picked commits change
in them is put back as the target has it. `--include-branch-specific <path>`
copies one such file or folder from the source anyway (or removes it when
the source has none); the path must match `branch_specific`, it is repeated
per path, and the output and the commit message say so loudly.

`--open-pr` also pushes the branch to the environment's repository (its
`repo`, else `--remote`) and opens a pull request into the environment's
branch, with every addon and its pin on both sides in the description. The
description and the commit message end with the changelog of the target's
`odx.lock`, as `odx changelog` writes it (*Release notes*), when the
promotion changes it (a package or the core; addons alone leave it as it
is). The
branch is odx's: writing it again replaces it. Approvals stay with the
forge's branch protection.

```console
$ odx promote --from-pr 42 --to prod     # the project addons #42 changed on test
$ odx promote --rollback 57 --to prod    # revert the promotion #57 merged into prod
```

`--from-pr` selects the project addons that a pull request merged into the
source environment's branch changed. `--rollback` finds the commits that
brought pull request #57 onto the target, however it was merged (its own
commits after a merge commit; the same change by patch id after a rebase,
or all of it at once after a squash), and plans reverting them, newest
first. With `--commit --yes` the revert is written on
`odx/rollback/<to>-<n>`; with `--open-pr` its pull request gets the `odx
env diff` of the target against the rollback, so reviewers see the
previous pins come back. Database changes are not undone: check the
migration scripts the promotion carried.
