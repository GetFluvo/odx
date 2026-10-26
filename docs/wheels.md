# Wheels and platforms

Building, checking and updating installable packages: `build`, the platform
profiles, and automatic updates.

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
