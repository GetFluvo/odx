# git-aggregate-rs

Drop-in replacement for [git-aggregator](https://github.com/acsone/git-aggregator)'s
`gitaggregate`, written in Rust. It reads the same `repos.yaml`, accepts the same
actions and flags, and adds opt-in extensions: `--allow-shell`, `--lock`,
`--mergiraf` and `--json`.

```console
$ git-aggregate-rs -c repos.yaml aggregate
```

Part of [odx](https://github.com/getfluvo/odx), where the same engine is
available as `odx git-aggregate` and `odx aggregate`.

**Status:** pre-alpha. The command line is in place; the engine lands in
roadmap phase 3.

Built with Rust & AI-accelerated engineering. MIT licensed.
