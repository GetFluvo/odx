# Continuous integration

odx runs the same in CI as on a laptop. A CI job usually does three things:
build exactly what `odx.lock` pins, check that the checkout matches it, and
check the addons. A nightly job can run the bot that keeps the upstream
dashboard and opens pull requests.

## GitHub Actions and Forgejo Actions

[`getfluvo/odx-action`](https://github.com/getfluvo/odx-action) is a
composite action for both. It installs odx from PyPI (`version`, default
`latest`; `none` keeps the odx on `PATH`) and restores the odx cache, keyed
on `odx.lock`, so a run fetches only what changed since the last one.

```yaml
# .github/workflows/odx.yml
on: pull_request
permissions:
  contents: read
jobs:
  odx:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
        with:
          fetch-depth: 0
      - uses: getfluvo/odx-action@v1
      - run: odx aggregate --locked
      - run: odx verify
      - run: odx check-deps
      - run: odx lint --only-changed --output-format github
```

- `odx aggregate --locked` fails when `pyproject.toml` changed without
  `odx lock`; with `CI` set, locked commits come from the vault first.
- `odx verify` checks, without network, that the working trees and
  lockfiles are what `odx.lock` describes
  ([Verifying a checkout](workspace.md#verifying-a-checkout)).
- `odx lint --output-format github` shows findings as annotations on the
  pull request; `sarif` feeds code scanning instead.
- `odx status --fail-on-merged` fails when a carried pull request was merged
  upstream and the override can be dropped.

On Forgejo the workflow lives in `.forgejo/workflows` and uses
`https://github.com/getfluvo/odx-action@v1`: actions hosted elsewhere need
the full URL.

## Other CI systems

Install odx with uv or pip and keep `ODX_CACHE_DIR` in the CI system's
cache. On GitLab CI:

```yaml
odx:
  image: python:3.13-slim
  variables:
    ODX_CACHE_DIR: "$CI_PROJECT_DIR/.odx-cache"
  cache:
    key:
      files: [odx.lock]
    paths: [.odx-cache]
  before_script:
    - apt-get update && apt-get install -y --no-install-recommends git
    - pip install odx-cli
  script:
    - odx aggregate --locked
    - odx verify
```

GitLab sets `CI`, so locked commits come from the vault first here as well.
The vault and the forge API need a token; pass it as `ODX_TOKEN_<HOST>`
(see [Forge tokens](user-config.md#forge-tokens)).

## Pre-commit

The same checks run before a commit with the hooks described in
[Pre-commit hooks](workspace.md#pre-commit-hooks).

## The nightly bot

With `bot: true`, the action runs `odx --json bot run`: with `--apply` on a
schedule, as a plan on `pull_request` events (`apply: auto`). Its outputs are
`version`, `plan-json`, the path of the run's JSON, and `dashboard-issue`.
What the bot does is described in
[The dashboard as an issue](upstream.md#the-dashboard-as-an-issue-odx-bot).

```yaml
# .github/workflows/odx-bot.yml
on:
  schedule: [{ cron: "10 2 * * *" }]
  issues: { types: [edited] }
  workflow_dispatch:
permissions:
  contents: read
jobs:
  bot:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/create-github-app-token@v2
        id: app
        with:
          app-id: ${{ vars.ODX_APP_ID }}
          private-key: ${{ secrets.ODX_APP_KEY }}
      - uses: actions/checkout@v5
        with:
          token: ${{ steps.app.outputs.token }}
          fetch-depth: 0
      - uses: getfluvo/odx-action@v1
        id: odx
        with:
          bot: true
        env:
          GITHUB_TOKEN: ${{ steps.app.outputs.token }}
      - uses: actions/upload-artifact@v4
        with:
          name: odx-plan
          path: ${{ steps.odx.outputs.plan-json }}
```

On Forgejo the same workflow lives in `.forgejo/workflows`, uses
`https://github.com/getfluvo/odx-action@v1` (actions hosted elsewhere need the
full URL), and takes the token of a bot account from a secret instead of
the app; Forgejo's `upload-artifact` stays at its v3-compatible version.

| Platform | Identity | Why |
| --- | --- | --- |
| GitHub | a GitHub App installed on the organization; the workflow mints an installation token (`actions/create-github-app-token`) | pull requests opened with the default `GITHUB_TOKEN` do not trigger other workflows, so CI would not run on bot pull requests; a personal token works but belongs to a person |
| Forgejo, Gitea | a bot account with a token scoped to the project repository | there is no app API; workflows run on the bot's pushes |

The token needs `contents: write`, `pull-requests: write`, `issues: write`
and `metadata: read` on the project repository, nothing on upstream
repositories (public ones are read without a token), and no rights on the
deployment repository or the package index: merging and promoting stay a
person's. Pushing to the mirror uses its own credential (a separate secret,
`ODX_TOKEN_<HOST>`), write-only on the vault. odx reads tokens from the
environment and never prints them.

