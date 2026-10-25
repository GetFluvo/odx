<p align="center">
  <img src="https://raw.githubusercontent.com/getfluvo/odx/main/docs/_static/logo.svg" alt="odx logo" width="160">
</p>

<h1 align="center">odx</h1>

<p align="center">
  <a href="https://pypi.org/project/odx-cli/"><img src="https://img.shields.io/pypi/v/odx-cli?style=flat-square&logo=pypi&logoColor=white&label=PyPI&color=ef662f" alt="PyPI"></a>
  <a href="https://crates.io/crates/odx-cli"><img src="https://img.shields.io/crates/v/odx-cli?style=flat-square&logo=rust&logoColor=white&label=crates.io&color=ef662f" alt="crates.io"></a>
  <a href="https://github.com/getfluvo/odx/actions/workflows/tests.yml"><img src="https://img.shields.io/github/actions/workflow/status/getfluvo/odx/tests.yml?branch=main&style=flat-square&logo=githubactions&logoColor=white&label=tests" alt="Tests"></a>
  <a href="https://odx.readthedocs.io/"><img src="https://img.shields.io/readthedocs/odx?style=flat-square&logo=readthedocs&logoColor=white&label=docs" alt="Documentation"></a>
  <a href="https://github.com/getfluvo/odx/blob/main/LICENSE"><img src="https://img.shields.io/github/license/getfluvo/odx?style=flat-square&color=7e5a8c" alt="License: MIT"></a>
</p>

<!-- start-docs -->

**Odoo Developer eXtension**: one Rust CLI for Odoo projects, driven by a
single `[tool.odx]` table in `pyproject.toml`.

odx builds a project's addon repositories from upstream branches plus the
pull requests you need, pins and vaults every commit so rebuilds never break,
keeps an eye on what upstream merged, ports fixes across Odoo series, migrates
addons the OCA way, and lints with project context. It replaces
git-aggregator, uvault and a handful of scripts with one fast binary.

## Install

```console
$ uv tool install odx-cli       # or: pipx install odx-cli
$ cargo install odx-cli
```

Binary wheels are published for Linux (glibc and musl, x86_64 and aarch64),
macOS (Intel and Apple silicon) and Windows; no Rust toolchain needed.

The [odx extension for VS Code](https://open-vsx.org/extension/getfluvo/odx)
(Open VSX: VSCodium, Cursor, Windsurf and others) shows the dashboard and the
project's pull requests, and downloads `odx` when it is not installed.

## Quick start

```console
$ odx import repos.yaml --write     # start from an existing git-aggregator setup
$ odx aggregate                     # fetch and build every repository
$ odx lock                          # pin and vault every merged commit
$ odx status                        # what upstream merged, what changed
$ odx lint --only-changed           # odoo-lint with the project's context
```

## What it does

| Area | Commands |
|---|---|
| Aggregate and lock | `aggregate`, `lock`, `verify`, `add`, `drop`, `import`, `export` |
| Packages and deployment | `vendor`, `build`, platform profiles (wheels, vendored source, Odoo.sh) |
| Local development | `dev`, `doctor`, `auth`, `sync`, `cache`, `daemon` |
| Upstream insight | `status`, `suggest`, `verdict`, `dashboard`, `check-deps`, `upgrade`, `sniff`, `stats`, `glue`, `changelog`, `diff` |
| Environments and automation | `env`, `promote`, `bot` |
| Porting and migration | `port pr/addon/sweep`, `migrate plan/addon/pr/update/pin/unpin/own` |
| Linting | `lint`, with odoo-lint built in and project rules on top |

Every command supports `--json`. The full reference, configuration and
compatibility notes are on [Read the Docs](https://odx.readthedocs.io/).

The aggregation engine is also available as a drop-in replacement for
git-aggregator's `gitaggregate`: `odx git-aggregate`, or the standalone
[`git-aggregate-rs`](https://pypi.org/project/git-aggregate-rs/) package.

## Status

Pre-alpha (0.0.x): the configuration and the CLI may still change between
releases.

<!-- github-only -->

## Packages

| Package | Registry | Purpose |
|---|---|---|
| `odx-cli` | [PyPI](https://pypi.org/project/odx-cli/), [crates.io](https://crates.io/crates/odx-cli) | the `odx` command |
| `git-aggregate-rs` | [PyPI](https://pypi.org/project/git-aggregate-rs/), [crates.io](https://crates.io/crates/git-aggregate-rs) | the gitaggregate drop-in |
| `odx-*` | [crates.io](https://crates.io/search?q=odx-) | the libraries odx is built from; no stable API |

## Contributing

Contributions are welcome: see
[CONTRIBUTING.md](https://github.com/getfluvo/odx/blob/main/CONTRIBUTING.md)
for the development setup and the quality gates (clippy pedantic, 100%
coverage, MSRV 1.97). This project follows the
[Contributor Covenant](https://github.com/getfluvo/odx/blob/main/CODE_OF_CONDUCT.md).

## License

MIT, see [LICENSE](https://github.com/getfluvo/odx/blob/main/LICENSE).
Maintained by Strategic Efficiency B.V. under the
[getfluvo](https://github.com/getfluvo) organization.

Built with Rust & AI-accelerated engineering
