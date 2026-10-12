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

## gitaggregate drop-in

```console
$ odx git-aggregate -c repos.yaml          # or the standalone git-aggregate-rs
$ odx git-aggregate -c repos.yaml -d '*/sale-*' -j 8
$ odx git-aggregate -c repos.yaml -e --env-file .env -p
```

`odx git-aggregate` (and the `git-aggregate-rs` package) runs a `repos.yaml`
the way `gitaggregate` does, on the odx engine: every remote is fetched once
into the shared cache, concurrently, and each target directory becomes a
working tree of that cache with the merges applied in order. Target
directories are relative to the current directory, as with `gitaggregate`.

| Option | Effect |
| --- | --- |
| `-c, --config <FILE>` | the `repos.yaml` (default `repos.yaml`) |
| `-d, --dirmatch <PATTERN>` | only target directories matching the fnmatch pattern |
| `-e, --expand-env` | expand `$VAR` and `${VAR}`; the environment wins over `--env-file` |
| `--env-file <FILE>` | `KEY=VALUE` lines for `--expand-env` |
| `-f, --force` | discard local changes; replace directories odx did not create |
| `-p, --push` | push the result to `target: <remote> <branch>` |
| `-j, --jobs <N>` | repositories at once (default: number of CPUs) |
| `--log-level <LEVEL>` | `DEBUG`, `INFO`, `WARNING`, `ERROR` or `CRITICAL` |
| `--allow-shell` | run `shell_command_after` (skipped with a warning otherwise) |
| `--mergiraf` | resolve conflicts with mergiraf (`ODX_MERGIRAF` names the executable) |
| `--json` | summary on stdout (schema 1) |

Differences from `gitaggregate`:

- Target directories are detached working trees of the cache: `git log`,
  `git diff` and `git status` work, but the remotes are not configured in
  them and the target branch only exists on the push remote.
- Directories created by `gitaggregate` are replaced only with `-f`.
- Every merge is a merge commit, so commit ids differ; the resulting trees
  are identical, which CI checks against git-aggregator 4.1.
- With `-e`, an unset variable is left as written with a warning.
- `show-closed-prs` and `show-all-prs` land in roadmap phase 7.

The cache and local clones are shared with `odx aggregate` (see
[Command line](cli.md)).
