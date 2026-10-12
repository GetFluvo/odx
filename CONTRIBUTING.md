# Contributor Guide

Thank you for your interest in improving odx. The project is open source
under the [MIT license](LICENSE) and welcomes contributions in the form of bug
reports, feature requests and pull requests.

Important resources:

- [Source Code]
- [Documentation]
- [Issue Tracker]
- [Code of Conduct]

[source code]: https://github.com/getfluvo/odx
[documentation]: https://odx.readthedocs.io/
[issue tracker]: https://github.com/getfluvo/odx/issues
[code of conduct]: CODE_OF_CONDUCT.md

## Contributor License Agreement

odx is open-core: the MIT-licensed core is permanent, and some components may
be offered under a separate commercial license. To keep that possible, every
contributor agrees to our [Contributor License Agreement](CLA.md) (CLA) before
their first pull request is merged. It confirms your contribution is yours to
give and lets the maintainer license it under both MIT and commercial terms;
you keep full ownership of your work.

You don't need to do anything up front. When you open your first PR, the
**CLA Assistant** bot comments with a link to the CLA. To agree, post this
comment on the PR:

> I have read the CLA Document and I hereby sign the CLA

Your signature is recorded, the `license/cla` check goes green, and it is
remembered for all future PRs. Maintainers and bots (e.g. Dependabot) are
exempt.

## How to report a bug

Report bugs on the [Issue Tracker]. Please include:

- your operating system, the output of `odx --version` (which also prints the
  embedded odoo-lint version) and `git --version`;
- the relevant `[tool.odx]` section (with private URLs removed) and, if
  applicable, `odx.lock`;
- what you did, what you expected, and what happened instead;
- the output with `-vv` (logs go to stderr; tokens are redacted, but check
  before pasting).

A minimal reproduction (a small `pyproject.toml` plus public repositories) is
the fastest way to get a bug fixed. `odx doctor` output helps for setup
problems.

## How to request a feature

Request features on the [Issue Tracker]. Check the roadmap in the
[Documentation] first; many ideas are already planned and an issue can attach
to an existing phase.

## Development setup

You need:

- Rust via [rustup]. The repository's `rust-toolchain.toml` selects the
  stable toolchain used for development; the coverage gate additionally needs
  `nightly` (`rustup toolchain install nightly --component llvm-tools-preview`)
  and the MSRV gate needs `rustup toolchain install 1.97`.
- `git` 2.38 or newer on `PATH`.
- [uv], for the documentation build, the git-aggregator parity tests (which
  run `gitaggregate` through `uvx`) and the maturin wheel.
- Developer tools:

  ```console
  $ cargo install just cargo-llvm-cov cargo-deny cargo-machete cargo-msrv typos-cli cocogitto
  $ rustup component add rustfmt clippy llvm-tools-preview
  $ uv tool install pre-commit
  $ pre-commit install --hook-type pre-commit --hook-type commit-msg
  ```

- Optional, for the full integration tests: [mergiraf] and `odl`
  (`uv tool install odoo-linter`) on `PATH`; `hyperfine` for the benchmark.

Build and run the binaries from the workspace:

```console
$ cargo build
$ cargo run -p odx-cli -- --help
$ cargo run -p git-aggregate-rs -- --help
```

[rustup]: https://rustup.rs/
[mergiraf]: https://mergiraf.org/
[uv]: https://docs.astral.sh/uv/

## Running the quality gates

The `justfile` is the single entry point; CI runs the same targets.

```console
$ just all          # everything, in CI order
$ just fmt          # format
$ just clippy       # lints, warnings are errors
$ just test         # unit, integration and doc tests
$ just cov          # coverage, fails below 100% lines, functions or regions
$ just cov-html     # coverage report in the browser
$ just doc          # rustdoc with warnings denied
$ just docs         # Sphinx user docs with warnings denied
$ just docs-live    # live-reloading documentation
$ just deny         # licenses, advisories, bans, sources
$ just machete      # unused dependencies
$ just msrv         # builds on the minimum supported Rust version
$ just typos        # spelling
$ just commits      # conventional commit check
$ just bench        # benchmark against git-aggregator (needs hyperfine)
```

In short, the gates are:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  (`clippy::all` and `clippy::pedantic` as errors; `unwrap_used`,
  `expect_used`, `panic`, `todo`, `dbg_macro`, `print_stdout` and other
  restriction lints enabled; `unsafe_code = "forbid"`, `missing_docs = "deny"`)
- `cargo test --workspace --all-features` including doc tests
- `cargo +nightly llvm-cov --workspace --all-features --doctests --fail-under-lines 100`
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
- `sphinx-build -W --keep-going -b html docs docs/_build/html` and `-b linkcheck`
- `cargo deny check`, `cargo machete`, MSRV check (`1.97`), `typos`, `cog check`

The lint configuration lives in `[workspace.lints]` in the root `Cargo.toml`
and in `clippy.toml`; the full description is in the development chapter of
the [Documentation].

## Writing testable code

Coverage is 100% lines and is not negotiable, so code is written to be
testable:

- Keep `main` to runtime setup and a call into `run()`.
- Put filesystem, process, git, network and secret-store access behind the
  traits in `odx-core` and test with the provided fakes. Integration tests
  use git fixtures from `tests/fixtures` and recorded HTTP responses, never
  the network.
- Every error variant needs a test that triggers it.
- `#[cfg_attr(coverage_nightly, coverage(off))]` is reserved for glue that
  cannot run in a test process. Justify every use in the PR description;
  reviewers will push back on exclusions of logic.
- Use `#[expect(lint, reason = "...")]`, never `#[allow]`.

## Things that must never happen

- A project, customer or client identifier written to a remote: vault refs
  are `refs/odx/<sha>`, nothing else; the vault must be private unless the
  user opted in to `allow_public`.
- A token in a log line, a `--json` document or an error message.
- A PR dropped from the lockfile without an explicit `odx drop`.
- Shell commands executed from a configuration file without `--allow-shell`.

Tests for these are part of the definition of done for the features that
touch them.

## Submitting changes

Open a [pull request]. For acceptance it needs to:

- keep the `all-checks` gate green (it aggregates every job above and is the
  required status check on `main`);
- include tests for all new code paths and keep coverage at 100% lines;
- document public items (doc examples are compiled as tests) and update the
  user documentation under `docs/` when behavior or configuration changes;
- use [Conventional Commits] (`feat(config): ...`, `fix(git): ...`,
  `feat(gar): ...` for `git-aggregate-rs`); PRs are squash-merged, so the PR
  title must be a valid commit message;
- have the CLA signed (see above).

Feel free to submit early as a draft; we can iterate on it. It is recommended
to open an issue before starting work on anything substantial so the approach
can be discussed first.

[pull request]: https://github.com/getfluvo/odx/pulls
[conventional commits]: https://www.conventionalcommits.org/

## Releasing (maintainers)

1. Bump `version` in `[workspace.package]` of the root `Cargo.toml` (and the
   internal crate versions in `[workspace.dependencies]`) in a
   `build: release vX.Y.Z` pull request.
2. Release Drafter keeps a draft GitHub Release up to date from the merged,
   auto-labeled pull requests. Review it, set the tag `vX.Y.Z`, and publish.
3. Publishing the release runs `.github/workflows/release.yml`: it checks the
   tag against the workspace version, builds wheels of `odx-cli` and
   `git-aggregate-rs` for Linux (glibc and musl, x86_64 and aarch64), macOS
   and Windows plus sdists, attests and signs them with Sigstore, publishes
   them to PyPI and then the crates to crates.io. Both registries use trusted
   publishing (no tokens); the `pypi` and `crates-io` environments only accept
   `v*` tags.
