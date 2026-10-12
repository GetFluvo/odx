# git-aggregate-rs

Drop-in replacement for [git-aggregator](https://github.com/acsone/git-aggregator)'s
`gitaggregate`, written in Rust. It reads the same `repos.yaml` and accepts the
same flags (`-c`, `-p`, `-d`, `-e`, `--env-file`, `-f`, `-j`, `--log-level`,
`--no-color`), with opt-in extensions: `--allow-shell`, `--mergiraf` and
`--json`.

```console
$ git-aggregate-rs -c repos.yaml
$ git-aggregate-rs -c repos.yaml -d '*/sale-*' -j 8
$ git-aggregate-rs -c repos.yaml -e --env-file .env -p
```

Every remote is fetched once into a shared cache, concurrently; each target
directory becomes a working tree of that cache with the merges applied. A
second project using the same repositories fetches only what changed.

Differences from `gitaggregate`, all deliberate:

- Target directories are detached working trees of the cache. `git log`,
  `git diff` and `git status` work as usual; the remotes are not configured
  in them and the target branch is only created when pushing (`-p`).
- Directories made by `gitaggregate` are replaced only with `-f`.
- Every merge is a merge commit, so commit ids differ from `gitaggregate`'s;
  the resulting trees are the same (checked in CI against git-aggregator 4.1).
- `shell_command_after` runs only with `--allow-shell`; otherwise it is
  skipped with a warning.
- `-j` defaults to the number of CPUs.
- With `-e`, an unset variable is left as written with a warning.
- `show-closed-prs` and `show-all-prs` are not implemented yet.

Part of [odx](https://github.com/getfluvo/odx), where the same engine is
available as `odx git-aggregate` and, for `[tool.odx]` projects, as
`odx aggregate`.

Built with Rust & AI-accelerated engineering. MIT licensed.
