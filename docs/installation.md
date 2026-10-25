# Installation

odx installs as a single binary from either registry:

```console
$ uv tool install odx-cli       # PyPI, binary wheel
$ cargo install odx-cli         # crates.io
```

Both install the `odx` command. The gitaggregate drop-in is available as
`odx git-aggregate`, or on its own:

```console
$ uv tool install git-aggregate-rs
$ cargo install git-aggregate-rs
```

## Editors

The [odx extension](https://open-vsx.org/extension/getfluvo/odx) for VS Code
and its Open VSX based relatives (VSCodium, Cursor, Windsurf) adds an odx view
with the dashboard, per section, and the project's pull requests; entries take
their actions (`odx dashboard act`) from the context menu. Its commands run
`odx aggregate`, `lock`, `vendor`, `dev`, `sniff`, `check-deps`, `verify` and
`glue` as tasks. It uses `odx` from the `odx.path` setting, the workspace's
`.venv` or `PATH`, and otherwise downloads the wheel from PyPI.

## Shell completions

```console
$ odx completions bash > ~/.local/share/bash-completion/completions/odx
$ odx completions zsh  > ~/.zfunc/_odx
$ odx completions fish > ~/.config/fish/completions/odx.fish
```
