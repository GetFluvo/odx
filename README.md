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

**Odoo Developer eXtension**: a Rust CLI for managing Odoo projects from a
single `[tool.odx]` table in `pyproject.toml`. The command is `odx`; the
package is `odx-cli`.

odx aggregates unmerged pull requests on top of upstream Odoo and OCA
repositories, pins the result in a lockfile, vaults the pinned commits on a
private mirror so they cannot disappear, refuses to follow PR heads whose CI
is red, prepares a local development workspace, promotes exactly tested pins
between environments, and analyzes the project (migration scripts, dependency
drift, upstream merge status, portable fixes on other series, upgrade
readiness). The same engine is available as a drop-in replacement for
git-aggregator's `gitaggregate`: `odx git-aggregate`, or the standalone
`git-aggregate-rs` package.

> Status: pre-alpha. Working today: the `[tool.odx]` configuration,
> `odx import`/`export`/`add`/`drop`, `odx aggregate`, `odx lock` (with the vault and the
> force-push guard rail), `odx verify` and the gitaggregate
> drop-in (`odx git-aggregate`, `git-aggregate-rs`). Every command not yet
> implemented names the roadmap phase in which it lands.

## Features

- **Aggregate**: parallel fetch of PRs, commits and branches into a shared
  object cache; sequential merges per repository with clean abort on conflict;
  optional syntax-aware merging through [mergiraf](https://mergiraf.org).
- **Lock and vault**: `odx.lock` pins every merged commit with its SHA, tree id
  and patch-id; commits are pushed to content-addressed refs on a private
  mirror (Forgejo, Gitea, GitLab) so a force-pushed PR cannot break a rebuild.
- **Force-push guard rail**: when a tracked PR head changes, odx checks the
  new head's CI; red heads are not adopted, the vaulted pin is kept and
  flagged, and dropping a PR is always a human decision.
- **Packages**: addons consumed through `uv` VCS sources are vaulted the same
  way and written to `[tool.uv.sources]` (replaces uvault), with a one-command
  switch to a local editable clone.
- **Dev workspace**: generates the Odoo `addons_path` (and rewrites
  `odoo.conf`) and runs PEP 660 editable installs through `uv`.
- **Verify**: one command for CI and pre-commit that proves a checkout matches
  `odx.lock` and `uv.lock`.
- **Sniff**: lists database migration scripts introduced by aggregated PRs
  before they reach production.
- **Check-deps**: compares `__manifest__.py` dependencies with
  `pyproject.toml`; reports missing addons, the same addon from two sources,
  and license conflicts.
- **Status / track**: aggregated PRs that were merged upstream, PRs with new
  commits, orphaned pins, red upstream heads, local modifications on vendored
  modules.
- **Suggest**: a dashboard of PRs and commits on other Odoo series that touch
  the modules you use, with mutes (reason and expiry) committed to the repo.
- **Upgrade**: module availability on the index and the forge for the next
  Odoo series, plus code readiness from odoo-lint.
- **Port**: ports a PR or an addon's missing commits onto another series from
  the local cache, with a maintainer sweep over a whole repository,
  patch-id deduplication, oca-port-compatible blacklists, authorship
  preserved, odoo-lint's upgrade fixes applied, and PRs opened on GitHub,
  GitLab, Gitea or Forgejo; addons that moved repositories between series
  are followed through a cross-series addon index.
- **Platform profiles**: `generic-wheels`, `vendored-source`, `odoo-sh` and
  `custom` define how artefacts are delivered and verified; odx prepares
  and verifies, the platform deploys. Provided-package sets per platform and
  series keep requirements that the platform already ships out of the
  generated requirements file.
- **Build**: builds aggregated, unmerged and private addons as wheels with
  content-derived versions (`+odx.<shortsha>`) and publishes them to the
  private index, so wheel-capable platforms install hashes instead of
  copying source.
- **Vendor**: materializes the locked packages of each dependency group into
  deployable addon directories for source-based platforms, with the
  manifest version-bump rule, a content-hash manifest and the platform
  requirements file.
- **Verdicts**: a declined forward-port ("not needed on 19.0") is recorded
  once, keyed by patch-id, addon and target series, shared privately across
  every environment and every project of the organisation, and re-checked
  when the target code changes; project-level mutes stay with the project.
- **Promote**: copies the exact pins of modules, a feature set or a
  functional description (`--search "invoicing"`) along a configurable
  promotion graph between environments (presets for common branch layouts
  and Odoo.sh), always plan first, detects divergence, optionally carries
  the project's own commits, and supports rollback; `odx env diff` shows
  what differs. Scripts and AI coding agents drive it through `--json` and
  `--yes`.
- **Dashboard**: one view of everything beyond Dependabot: pinned PRs and
  droppable overrides, new upstream PRs touching deployed modules,
  forward-port suggestions (fixes merged on a lower series and missing on
  yours), translation-only bumps and promotion drift, in the terminal, as a
  TUI, as JSON or as Markdown, each entry with actions: include in the
  project, open a forward-port PR upstream (human-triggered only), or mute
  with a reason.
- **Bot**: a nightly job posts the same dashboard as one rolling issue per
  series with checkboxes, and opens grouped PRs for merged overrides, green
  re-pins, translation-only bumps and promotions, on GitHub Actions and
  Forgejo Actions, inside the private project repository only.
- **Background sync**: an opt-in user-level scheduled job prefetches
  repositories, PR refs, index metadata and wheels into the shared cache
  within the API budget, mirror first, so `aggregate`, `status`, `dashboard`
  and `port` answer from the cache immediately, show data staleness, refresh
  only what is stale, and keep working offline.
- **Lint**: [odoo-lint](https://github.com/bosd/odoo-lint) embedded with
  project context.
- **Stats**: lines of code per language via `tokei`.
- **Import**: converts git-aggregator `repos.yaml` and doodba `addons.yaml`
  to `[tool.odx]`.
- **Auto-update**: translation-only wheel bumps are re-locked automatically
  under a configurable policy with a wheel content check.
- **Polite to APIs**: tokens from the environment, `gh` or the keyring;
  batched GraphQL, ETag caching, per-host rate limiting, `--offline`.

## Example configuration

```toml
[tool.odx]
odoo_version = "17.0"
merge_strategy = "rebase"   # or "merge" for merge commits
mergiraf = "auto"
on_head_change = "follow-if-green"

[tool.odx.mirror]
url = "ssh://git@forgejo.internal/odoo/vault.git"   # private

[[tool.odx.repositories]]
url = "https://github.com/OCA/pos.git"
target_dir = "vendor/pos"
addons = ["pos_discount", "pos_loyalty*"]
merges = [
  { pr = 123, track_upstream = true },
  { sha = "a1b2c3d4e5f67890123456789012345678901234" },
]

[tool.odx.packages]
"odoo-addon-pos-vouchers" = { repo = "OCA/pos", pr = 45, subdirectory = "pos_vouchers" }

[tool.odx.features.pos-loyalty]
modules = ["pos_loyalty", "pos_loyalty_reports"]
```

Everything else in `pyproject.toml` (`[project]`, `[build-system]`, other
`[tool.*]` tables) is left alone.

## Planned CLI

```text
odx aggregate [--locked | --update] [--config repos.yaml]
odx lock [--check] [--auto-update] [--no-push]
odx vendor [--requirements]
odx build --publish --index private
odx verify [--vendored | --installed .venv | --requirements]
odx dev [<package> --branch <name>] [--restore]
odx add <url> --pr <n> | odx drop <repo>#<pr>
odx import repos.yaml [addons.yaml] [--write] | odx export repos.yaml
odx dashboard [--tui | --json | --markdown] | odx dashboard act <id> --include
odx status [--fail-on-merged]
odx suggest [--env uat] [mute <ref> --reason "..."]
odx verdict add OCA/contract#1480 --addon contract --target 19.0 --kind not-applicable --reason "..."
odx check-deps [--licenses] [--fix]
odx sniff | odx stats | odx upgrade --target 18.0
odx port pr OCA/sale-workflow#1234 --to 17.0 [--open-pr]
odx port sweep --to 19.0 --from 16.0,17.0,18.0 [--open-pr]
odx promote <module>... | --feature pos-loyalty | --search "invoicing" --to prod [--open-pr] [--yes --json]
odx env diff test prod
odx bot run --apply
odx sync [--all] | odx daemon install --interval 30m | odx cache gc
odx lint [--only-changed] [--format github]
odx doctor | odx auth login <host> | odx completions <shell>

odx git-aggregate -c repos.yaml -j 4     # gitaggregate drop-in
git-aggregate-rs -c repos.yaml -j 4      # same, standalone package
```

Every command supports `--json`.

## Installation (planned)

```bash
uv tool install odx-cli      # binary wheel from PyPI, no Rust toolchain needed
cargo install odx-cli        # from crates.io
```

Static binaries will also be published on GitHub Releases.

## Quality

The codebase is held to a fixed set of gates enforced in CI behind one
required `all-checks` status on `main`: `rustfmt`, `clippy` with `pedantic`
and selected restriction lints as errors, `unsafe_code = "forbid"`,
`missing_docs = "deny"`, 100% line coverage with `cargo llvm-cov`, doc tests,
`cargo doc` and the Sphinx documentation with warnings denied, `cargo deny`,
`cargo machete`, an MSRV check, `typos` and Conventional Commits.

Rust edition 2024, MSRV 1.97, MIT license.

<!-- github-only -->

## Documentation

User documentation is published on [Read the Docs](https://odx.readthedocs.io/).

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) for the
development setup, the quality gates and the Contributor License Agreement.
This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).

## License

MIT. See [LICENSE](LICENSE).

---

Maintained by Strategic Efficiency B.V. under the
[getfluvo](https://github.com/getfluvo) organization.

Built with Rust & AI-accelerated engineering
