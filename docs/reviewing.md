# Reviewing a lock change

What a change of `odx.lock` brings in, for a pull request review or a
release: `sniff`, `changelog` and `diff`.

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

## Release notes

```console
$ odx changelog                          # odx.lock in the working tree against HEAD
$ odx changelog --since v1.4             # what changed since a release
$ odx changelog --since v1.4 --to v1.5   # between two committed states
$ odx --json changelog --since main      # the same, for scripts
```

`odx changelog` compares two states of `odx.lock` and writes Markdown
release notes: the Odoo core revision, then per repository its URL, its base
and its merges, then per package its source and commit. A merge or package
is added, removed or updated (another commit of the same pull request,
branch, commit or tag, with the number of commits when the cache has both);
a merge whose lock-time status changed (merged, skipped, pinned) says so.
Pull requests link to their page on the forge. The migration scripts the
change brings in follow under *Migrations*, as `odx sniff --since` reports
them; when the cache lacks a commit, that part is a warning and the rest of
the notes are still written.

`--since` defaults to `HEAD`; without `--to`, the later state is the file in
the working tree.

## Schema changes

```console
$ odx diff                               # odx.lock in the working tree against HEAD
$ odx diff --since v1.4 --to v1.5        # between two committed states
```

`odx diff` takes the same two states of `odx.lock` as `odx changelog` and
reports, per repository and package and per addon, what is likely to touch
the database on the next update: models created or no longer created,
fields added, removed or of another type (`Many2one(res.partner)` to
`Many2one(res.users)`), new or dropped `_inherit` and `_inherits` parents,
and access rights (`ir.model.access`) added or removed. The addon's version
change and the migration scripts it brings in are listed with it, and new
or removed addons are named as such:

```text
schema changes in pos (repository):
  pos_new: new addon (1 models, 1 fields)
  pos_x 17.0.1.1.0 (was 17.0.1.0.0)
    + field pos.order.total: Float
    pre  added    pos_x/migrations/17.0.1.1.0/pre-migrate.py
```

The models are read with odoo-lint's index from both versions of each
changed addon, extracted from the cache; this is static analysis, so a
field built dynamically is not seen. A repository or package the cache
cannot compare is a warning. `--json` gives the same per addon.
