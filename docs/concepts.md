# Concepts

The words odx uses, each with where it is explained in full.

## Building a project

Project
: A directory whose `pyproject.toml` has a `[tool.odx]` table; odx finds it
  from the current directory or any parent. Its `odoo_version` is the
  project's **series** (`"17.0"`).

Repository
: An upstream git repository of addons, such as `OCA/pos`, configured as a
  `[[tool.odx.repositories]]` table with a short **name**.

Base and merges
: A repository is built from its **base** branch (the series, unless
  configured) with **merges** applied in order: pull requests, branches
  (from any remote) and single commits that are not upstream yet. A merge is
  rebased on top by default (`merge_strategy`).

Working tree
: The result of a repository's base and merges, checked out under
  `vendor/<name>` by `odx aggregate`. Working trees are disposable: they are
  rebuilt from the cache, and odx refuses to overwrite one with local
  changes unless told to (`--force`).
  [Building the working trees](workspace.md#building-the-working-trees).

Cache
: One bare git repository per upstream URL, shared by every project on the
  machine (`~/.cache/odx` on Linux). Fetches go there once; working trees
  are git worktrees of it. It can borrow objects from your own clones, and
  `odx sync` keeps it current in the background.
  [Fetching into the cache](workspace.md#fetching-into-the-cache).

Lockfile
: `odx.lock` records the exact commit of every base, merge and package, and
  the tree that results. With a lock, `odx aggregate` builds the same trees
  everywhere; `odx lock --update` moves it forward. Commit it.
  [Locking](workspace.md#locking), [`odx.lock`](files.md#odxlock).

Vault
: A private repository (`[tool.odx.mirror]`) that keeps every locked commit
  under a ref named after the commit, so a build survives force-pushes and
  deleted branches upstream. [The vault](workspace.md#the-vault).

Guard rail
: When the head of a locked pull request moves, `odx lock --update` decides
  per merge whether to follow it (`on_head_change`: keep the pin, follow when
  its CI is green, or always follow).
  [The force-push guard rail](workspace.md#the-force-push-guard-rail).

Package
: A single addon installed as a Python distribution (`odoo-addon-pos_x`),
  from a package index or straight from a pull request. Packages are listed
  in **dependency groups** of `pyproject.toml`, and each group has a
  directory its addons are vendored into (`[tool.odx.groups]`).
  [Packages](workspace.md#packages).

Platform profile
: How the project is deployed: wheels from a package index, addons copied
  into the repository, Odoo.sh, or your own hooks. It decides what
  `odx vendor` and `odx build` produce.
  [Wheels and platforms](wheels.md).

## Following upstream

Tracked pull request
: A merge from a pull request whose state upstream odx follows: open,
  merged, closed, new commits, force-pushed. A merged one is an **override**
  that can be dropped. [Upstream status](upstream.md#upstream-status).

Suggestion
: A change on another series (a backport or forward-port candidate) that
  touches an addon the project uses.
  [Suggestions from other series](upstream.md#suggestions-from-other-series).

Dashboard
: Everything there is to act on, in sections: pinned pull requests,
  droppable overrides, new upstream pull requests, ports, index updates,
  glue addons and environment drift. `odx bot` keeps it as an issue and
  opens the pull requests. [The dashboard](upstream.md#the-dashboard).

Mute and verdict
: A decision not to act on a suggestion, with a reason. A **mute** is the
  project's (`.odx/mutes.toml`); a **verdict** is about the change itself and
  can be shared between projects through a private repository.
  [Verdicts and mutes](upstream.md#verdicts-and-mutes).

Glue addon
: An addon that Odoo installs automatically once all its dependencies are
  installed (`auto_install`), such as `sale_stock`. Listing repositories one
  by one can leave one out; `odx glue` finds them.
  [Glue addons](upstream.md#glue-addons).

## Across series and environments

Port and migration
: A **port** brings commits or a pull request from one series to another; a
  **migration** moves a whole addon to the next series, the OCA way.
  [Upgrading and porting](series.md).

Environment
: A branch (or repository) that a deployment follows, such as `17-test` and
  `17-prod`, each with its own `odx.lock`. **Promotion** copies the exact pins
  of chosen addons from one environment to the next, through a pull request.
  [Environments](environments.md).
