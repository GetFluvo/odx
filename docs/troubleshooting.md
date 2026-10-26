# Troubleshooting

Start with `odx doctor`; most problems below show up there first. `-v` on
`odx aggregate` and `odx lock` shows every source as it is fetched and every
merge as it is made.

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

## Common errors

`no pyproject.toml with a [tool.odx] table in … or its parents`
: odx runs in a project. `odx import repos.yaml --write` turns a
  git-aggregator directory into one; `odx port`, `odx migrate` and
  `odx migrate plan --to` also work in a plain clone of an addons repository.

``odx.lock does not lock pos as configured; run `odx lock` ``
: `odx aggregate --locked` builds only what `odx.lock` pins, and
  `pyproject.toml` changed since the last lock. Run `odx lock` and commit
  both files.

`#123 conflicts in a.xml, b.py`
: A merge does not apply on top of the base and the earlier merges. Look at
  it with `odx aggregate --keep-conflicts`, which leaves the conflicted
  working tree in place; skip it with `on_conflict = "skip"` on the merge; or
  install [mergiraf](https://mergiraf.org), which odx uses when it is found
  (`mergiraf = "auto"`).

`vendor/pos has local changes; commit or discard them, or use --force`
: odx never throws work away: a working tree with changes, or a directory odx
  did not create, is only reset with `--force`.

`… is locked by another odx process`
: Another odx process is fetching the same repository into the cache, for
  example `odx sync` in the background. odx waits ten seconds for it; run the
  command again once the other one is done.

`… is not in the cache; fetch it once without --offline`
: `--offline` serves git repositories from the cache only. Run the command
  once without it, or `odx sync`, to fill the cache.

`… borrows objects from …, which no longer exists; delete … to rebuild it`
: A local clone the cache borrowed objects from was moved or deleted. Delete
  the named cache repository; the next run fetches it again. Run
  `odx cache detach` before removing a clone to avoid this.

`rate limit exceeded; try again later or use a token`
: Without a token, forges allow few API requests (60 an hour on GitHub). Set
  one up with `odx auth login <host>`; see
  [Forge tokens](user-config.md#forge-tokens).

`mergiraf = "always" is configured, but mergiraf cannot be run`
: Install mergiraf, point `ODX_MERGIRAF` at it, or set `mergiraf = "auto"`.

## Starting over

The cache only holds what can be fetched again. `odx cache info` shows its
size and location, `odx cache gc` trims it, and deleting the directory is
always safe: the next run fetches what it needs. The working trees under the
aggregate directory are rebuilt by `odx aggregate --force`.

## Reporting a problem

Open an issue at <https://github.com/getfluvo/odx/issues> with the command,
its output with `-v`, `odx --version` and the output of `odx doctor`. odx
removes credentials from URLs in everything it prints, but check the output
for other private names before posting it.
