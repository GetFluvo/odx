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

## Shell completions

```console
$ odx completions bash > ~/.local/share/bash-completion/completions/odx
$ odx completions zsh  > ~/.zfunc/_odx
$ odx completions fish > ~/.config/fish/completions/odx.fish
```
