# Checking addons

Static checks of the addons a project builds: `check-deps`, `lint` and `stats`.

## Checking dependencies

```console
$ odx check-deps                  # exit 1 when something is missing or odd
$ odx check-deps --fix            # add missing addons from the package index
$ odx check-deps --unused --licenses
```

`odx check-deps` reads the `__manifest__.py` of the project's own addons
and of the selected addons of aggregated repositories, and reports:

- `missing`: an addon in `depends` that nothing provides. Providers are
  Odoo's own addons for the series (16.0 to 19.0 are bundled), the project,
  the aggregated repositories (their working trees, or their cached base
  before `odx aggregate`), the addon packages in any dependency group, and
  `[tool.odx.check_deps] provided` (for example enterprise addons);
- `missing python package`: an `external_dependencies` entry that
  `pyproject.toml` does not declare;
- `duplicate`: one addon from two sources, which Odoo resolves by
  `addons_path` order;
- `stray`: an addon directory in a vendored group that `odx vendor` did not
  write, or one of the project's addons that git does not track.

`--unused` adds declared addon packages that no scanned addon depends on;
`--licenses` adds AGPL or GPL dependencies of addons under a weaker
license. `--fix` adds the missing addons the package index has a release
of for the series to `[tool.odx.groups] default`; run `uv lock` afterwards.

```toml
[tool.odx.check_deps]
provided = ["account_accountant"]   # available on the platform, not in the project
```

## Linting

```console
$ odx lint                            # the project's own addons
$ odx lint --only-changed             # the addons changed against the upstream
$ odx lint --fix                      # apply odoo-lint's safe fixes
$ odx lint --diff                     # show them instead
$ odx lint --output-format github     # annotations on pull requests
```

`odx lint` runs [odoo-lint](https://github.com/bosd/odoo-lint) inside odx, the
version `odx --version` names, so no `odl` needs to be installed. In a project
it lints the own addons (`project_addons`) for `odoo_version`, with the own,
aggregated, vendored and installed (`.venv`) addons on odoo-lint's addons path
for its checks across modules; outside a project, the current directory with
odoo-lint's own configuration. Paths given on the command line replace the
default; `[tool.odx.lint] paths` sets it, and `only_changed = true` makes
`--only-changed` the default. Rule selection and settings are odoo-lint's (`[tool.odoo-lint]` in
`pyproject.toml`, or `odoo-lint.toml`); `--select` and `--ignore` adjust them
for one run.

`--only-changed` keeps the addons with a change against `--base` (default: the
branch's upstream, else `origin/HEAD`), committed, staged, unstaged or
untracked. `--fix` applies the safe fixes (`--unsafe-fixes` the rest too) and
reports what is left; `--diff` prints them as a unified diff and writes
nothing. `--output-format` is `text`, `json`, `github`, `gitlab` or `sarif`,
as `odl check` writes them. odx exits 1 when findings are left (or `--diff`
would change something), 0 otherwise or with `--exit-zero`.

In a project, odx adds its own rules on the own addons, reported like
odoo-lint's and selected the same way (`--select ODX`, `--ignore ODX005`):

| Code | Name | Finds |
|---|---|---|
| `ODX001` | `manifest-version-series` | a manifest `version` not of the project's `odoo_version` |
| `ODX002` | `depends-unavailable` | a dependency that is neither Odoo's nor provided by the project (own, aggregated, packages, `[tool.odx.check_deps] provided`) |
| `ODX003` | `migration-folder-version` | a folder in `migrations/` that is no version, of another series, or newer than the manifest version: its scripts never run |
| `ODX004` | `addon-from-two-sources` | an addon the project gets from two places |
| `ODX005` | `version-not-bumped` | an addon changed since the last tag (`git describe --tags`) whose version is still the released one: the platform would not update it |

`[tool.odx] languages` names the languages the project is translated into
besides English, as Odoo codes (`languages = ["es", "nl"]`); `odx doctor`
shows them, and `odx lint` hands them to odoo-lint's
[PO105](https://github.com/bosd/odoo-lint/blob/main/docs/rules/PO105.md)
(`po-translation-missing`), which reports a module with translatable terms
but no `.po` file for a language, and a `.po` file that leaves terms
untranslated. A regional language is satisfied by its base language's file,
as Odoo loads them: `nl.po` serves `nl_BE`. odoo-lint's own
`[tool.odoo-lint.rules.po-translation-missing] languages` wins when set.

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
