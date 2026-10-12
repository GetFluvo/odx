# git-aggregator and doodba

odx reads and writes the files of
[git-aggregator](https://github.com/acsone/git-aggregator) (`repos.yaml`) and
[doodba](https://github.com/Tecnativa/doodba) (`addons.yaml`), so existing
projects can move over gradually.

## Import

```console
$ odx import                      # repos.yaml (+ addons.yaml next to it)
$ odx import repos.yaml addons.yaml
```

prints the equivalent `[[tool.odx.repositories]]` tables:

| repos.yaml | `[tool.odx]` |
| --- | --- |
| target directory key (`./vendor/pos`) | `target_dir = "vendor/pos"`, `name = "pos"` |
| first merge (`oca 17.0`) | `url` (the remote's URL) and `base = "17.0"` |
| `oca refs/pull/123/head`, `refs/merge-requests/123/head` | `{ pr = 123 }` |
| a commit (`oca a1b2c3d4`) | `{ sha = "a1b2c3d4" }` |
| a branch on the same remote | `{ branch = "17.0-fix" }` |
| a branch on another remote | `{ branch = "17.0-fix", remote = "<url>" }` |
| `target: <remote> <branch>` | `target_branch = "<branch>"` |
| `defaults.depth`, `fetch_all` | accepted and ignored (odx fetches into a shared object cache) |
| `shell_command_after` | not imported (warning) |
| addons.yaml `<repo>: [globs]` | `addons = [globs]` on the repository with that name |
| addons.yaml documents with `ONLY` | skipped (warning) |

Everything that cannot be represented is reported as a warning on standard
error. `odx import --write` adds the repositories to `pyproject.toml` instead
of printing them, skipping URLs that are already configured; when the file
has no `[tool.odx]` table yet, it is created with `--odoo-version` or the
first base branch as the series.

## Export

```console
$ odx export                 # writes repos.yaml and addons.yaml in the project root
$ odx export --stdout
```

writes the files back from `[tool.odx]`, deterministically and with a header
marking them as generated. The repository URL becomes remote `origin`;
branches from other URLs get a remote named after the URL's owner. GitLab
repositories use `refs/merge-requests/N/head`. Exporting and importing again
yields the same repositories, merges, targets and addon lists.
