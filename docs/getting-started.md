# Getting started

This page takes a project from nothing (or from a git-aggregator
`repos.yaml`) to working trees that are pinned, vaulted and reproducible in
CI. It takes about ten minutes; most of it is the first download of the
repositories.

## 1. Install odx

```console
$ uv tool install odx-cli
$ odx --version
$ odx doctor
```

`odx doctor` lists what odx found: git, uv, mergiraf, odoo-lint, and, once
you are in a project, the forge tokens and the cache. Only git is required.
See [Installation](installation.md) for the other ways to install.

## 2. Describe the project

odx reads one table, `[tool.odx]` in the project's `pyproject.toml`.

**From git-aggregator.** In the directory with `repos.yaml` (and doodba's
`addons.yaml`, if any):

```console
$ odx import repos.yaml --write
```

This creates `pyproject.toml` when there is none and adds a
`[[tool.odx.repositories]]` table per repository; what cannot be translated
is reported as a warning. [git-aggregator and doodba](compatibility.md) has
the full mapping and the steps to switch over.

**From scratch.** Write the table yourself:

```toml
[project]
name = "acme"
version = "0.1.0"

[tool.odx]
odoo_version = "17.0"

[[tool.odx.repositories]]
name = "pos"
url = "https://github.com/OCA/pos.git"   # base branch: the odoo_version

[[tool.odx.repositories]]
name = "web"
url = "https://github.com/OCA/web.git"
merges = [{ pr = 2817 }]
```

Then add the pull requests you need, one at a time:

```console
$ odx add OCA/pos#1234
$ odx add https://github.com/OCA/web/pull/2900
```

Every key is described in [Configuration](configuration.md).

## 3. Build the working trees

```console
$ odx aggregate
  Fetched pos in 4.1s
  Fetched web in 3.2s
    Built pos in 0.3s: 1 merged
    Built web in 0.2s: 2 merged
```

Each repository becomes a working tree under `vendor/<name>`: its base
branch with the merges applied in order. The repositories themselves live
once in a shared cache (`~/.cache/odx` on Linux), so a second project with
the same repositories costs almost no download.

A merge that conflicts stops its repository and says which files conflict;
[Troubleshooting](troubleshooting.md#common-errors) lists the ways out.

## 4. Lock

```console
$ odx lock
$ git add pyproject.toml odx.lock
$ git commit -m "Pin the addon repositories"
```

`odx.lock` records the exact commit of every base and merge. From now on
`odx aggregate` builds what the lock says, so the next person and CI get the
same trees, even after a pull request is force-pushed. `odx lock --update`
takes what upstream has now.

With a private vault repository configured (`[tool.odx.mirror]`), `odx lock`
also pushes every locked commit there, so a build still works when upstream
deletes a branch or rewrites a pull request. See
[The vault](workspace.md#the-vault).

## 5. Use it in CI

```console
$ odx aggregate --locked
```

`--locked` fails when `pyproject.toml` changed without `odx lock`. On GitHub
Actions and Forgejo, `getfluvo/odx-action` installs odx and caches its data;
see [Continuous integration](ci.md).

## 6. Keep up with upstream

```console
$ odx status         # per pull request: open, merged, new commits
$ odx dashboard      # everything to act on, in one view
```

When a pull request you carry is merged upstream, `odx status` says the merge
can be dropped; `odx drop OCA/pos#1234` removes it from `pyproject.toml` and
`odx.lock`. [Following upstream](upstream.md) covers the dashboard, the
nightly bot and suggestions from other series.

## Next steps

- [Concepts](concepts.md): the words odx uses, in one page.
- [Building the workspace](workspace.md): packages, vendoring, the
  development workspace and the cache.
- [Upgrading and porting](series.md): moving to the next Odoo series.
- [Environments](environments.md): test and production branches, and
  promoting pins between them.
