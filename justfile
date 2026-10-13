# Developer commands. CI runs the same recipes, so local and CI results match.

msrv := "1.97"

# Tests that run uv keep its cache inside target/, away from the user's cache.
export UV_CACHE_DIR := justfile_directory() / "target" / "uv-cache"

# List the recipes.
default:
    @just --list

# Format the code.
fmt:
    cargo fmt --all

# Check formatting.
fmt-check:
    cargo fmt --all -- --check

# Lint every target with warnings as errors.
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run the test suite, including doc tests.
test:
    cargo test --workspace --all-features

# Review pending insta snapshots.
snapshots:
    cargo insta review

# Build the API documentation with warnings as errors.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps

# Build the user documentation with warnings as errors.
docs:
    uv run --only-group docs sphinx-build -W --keep-going -b html docs docs/_build/html

# Serve the user documentation with live reload.
docs-live:
    uv run --only-group docs sphinx-autobuild docs docs/_build/html

# Check links in the user documentation.
linkcheck:
    uv run --only-group docs sphinx-build -W -b linkcheck docs docs/_build/linkcheck

# Enforce 100% line, function and region coverage.
cov:
    cargo +nightly llvm-cov --workspace --all-features --doctests \
        --fail-under-lines 100 --fail-under-functions 100 --fail-under-regions 100

# Coverage report in the browser.
cov-html:
    cargo +nightly llvm-cov --workspace --all-features --doctests --html --open

# Licenses, advisories, bans and sources.
deny:
    cargo deny check

# Unused dependencies.
machete:
    cargo machete --with-metadata

# Build with the minimum supported Rust version.
msrv:
    cargo +{{msrv}} check --workspace --all-features --all-targets

# Spelling.
typos:
    typos

# Audit workflows and Dependabot config (online audits when GH_TOKEN is set).
zizmor:
    uvx zizmor .github

# Conventional commits since the last tag.
commits:
    cog check --from-latest-tag

# Both distribution paths: crates.io and PyPI wheels.
package:
    cargo publish --dry-run --allow-dirty -p odx-core -p odx-config -p odx-git -p odx-forge -p odx-aggregate -p odx-analyze -p odx-platform -p odx-vendor -p odx-dev -p odx-sync -p odx-index -p odx-port -p odx-verdicts -p odx-dashboard -p git-aggregate-rs -p odx-cli
    uvx maturin build --release
    uvx maturin build --release -m crates/git-aggregate-rs/Cargo.toml

# Benchmark against git-aggregator (args: e.g. --seed ~/git/OCA, --mode github).
bench *args:
    cargo build --release -p git-aggregate-rs -p odx-cli
    python3 scripts/benchmark.py {{args}}

# Every gate, in CI order (except commits, which needs a tag).
all: fmt-check clippy test doc docs cov deny machete msrv typos zizmor package
