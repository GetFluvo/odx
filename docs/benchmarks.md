# Benchmarks

```{image} _static/benchmark.svg
:alt: Bar chart of the cold timings below
:width: 640px
```

| Tool                                         | cold   | warm   | no-op  |
| -------------------------------------------- | ------ | ------ | ------ |
| git-aggregate-rs 0.0.4                       | 3.79 s | 1.11 s | 0.72 s |
| git-aggregate-rs 0.0.4, 1 job                | 17.5 s | 5.46 s | 2.32 s |
| odx 0.0.4 `aggregate --locked`               | 4.39 s | 1.24 s | 0.18 s |
| odx 0.0.4 `aggregate --locked`, 1 job        | 18.3 s | 5.52 s | 0.17 s |
| git-aggregator 4.1, `-j 28`                  | 10.6 s | 10.3 s | 0.80 s |
| git-aggregator 4.1, 1 job (its default)      | 52.0 s | 51.7 s | 2.74 s |

All tools aggregate the same `repos.yaml`
([`tests/fixtures/benchmark/repos.yaml`](https://github.com/getfluvo/odx/blob/main/tests/fixtures/benchmark/repos.yaml)):
12 OCA repositories on 17.0, each at a pinned base commit, with 25 pull
requests that were merged upstream later, so the fixture never drifts. For odx it is the same project as
`[tool.odx]` (merge commits, as git-aggregator makes), locked once with
`odx lock` before the timings.
Times are the median of 5 runs after one warm-up run, on a 28-thread x86-64
Linux machine with git 2.55.

- **cold**: no cache and no target directories.
- **warm**: target directories removed, the git-aggregate-rs cache kept. For
  git-aggregator the target directories are its only cache, so warm is the
  same work as cold. This is the case of a second project, or a fresh
  checkout in CI with a persisted cache.
- **no-op**: everything from the previous run still in place. git-aggregator
  and git-aggregate-rs fetch and merge again; `odx aggregate --locked` sees
  that every working tree already is the locked result and does nothing.

The repositories are served from local bare mirrors over `file://`, so the
numbers measure the tools rather than the network; over the internet the
cold and warm differences grow with the amount fetched. Every run's result is
checked against the trees git-aggregator builds
([`trees.json`](https://github.com/getfluvo/odx/blob/main/tests/fixtures/benchmark/trees.json)),
so a timing only counts when every tool did the same work.

To reproduce:

```console
$ just bench --seed ~/git/OCA      # mirrors built from existing OCA clones
$ just bench --mode github         # fetch from github.com instead
```

The script ([`scripts/benchmark.py`](https://github.com/getfluvo/odx/blob/main/scripts/benchmark.py))
builds the mirrors under `target/bench`, installs git-aggregator 4.1 in a
virtual environment of its own with `uv`, prints the table and rewrites the
chart in `docs/_static/`.

On every pull request that touches the code, the `bench` workflow builds
the base and the pull request and times both on the same runner
(`--ours-only`: git-aggregate-rs and `odx aggregate --locked`, median of 3,
mirrors cached between runs), then comments the difference per scenario on
the pull request
([`scripts/bench_compare.py`](https://github.com/getfluvo/odx/blob/main/scripts/bench_compare.py)).
It is not a gate: GitHub runners vary by about 10% between runs, so only
larger changes are shown in bold.
