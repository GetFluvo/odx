# Benchmarks

```{image} _static/benchmark.svg
:alt: Bar chart of the cold timings below
:width: 640px
```

| Tool                                       | cold   | warm   | no-op  |
| ------------------------------------------ | ------ | ------ | ------ |
| git-aggregate-rs 0.0.4                     | 3.28 s | 1.06 s | 0.75 s |
| git-aggregate-rs 0.0.4, 1 job              | 15.3 s | 5.39 s | 2.14 s |
| git-aggregator 4.1, `-j 28`                | 8.59 s | 8.84 s | 0.80 s |
| git-aggregator 4.1, 1 job (its default)    | 47.3 s | 47.2 s | 2.83 s |

Both tools aggregate the same `repos.yaml`
([`tests/fixtures/benchmark/repos.yaml`](https://github.com/getfluvo/odx/blob/main/tests/fixtures/benchmark/repos.yaml)):
12 OCA repositories on 17.0, each at a pinned base commit, with 25 pull
requests that were merged upstream later, so the fixture never drifts.
Times are the median of 5 runs after one warm-up run, on a 28-thread x86-64
Linux machine with git 2.55.

- **cold**: no cache and no target directories.
- **warm**: target directories removed, the git-aggregate-rs cache kept. For
  git-aggregator the target directories are its only cache, so warm is the
  same work as cold. This is the case of a second project, or a fresh
  checkout in CI with a persisted cache.
- **no-op**: everything from the previous run still in place. Both tools
  fetch and merge again; git-aggregate-rs is not faster here yet. With the
  lockfile (roadmap phase 4) it can skip work whose inputs did not change.

The repositories are served from local bare mirrors over `file://`, so the
numbers measure the tools rather than the network; over the internet the
cold and warm differences grow with the amount fetched. Every run's result is
checked against the trees git-aggregator builds
([`trees.json`](https://github.com/getfluvo/odx/blob/main/tests/fixtures/benchmark/trees.json)),
so a timing only counts when both tools did the same work.

To reproduce:

```console
$ just bench --seed ~/git/OCA      # mirrors built from existing OCA clones
$ just bench --mode github         # fetch from github.com instead
```

The script ([`scripts/benchmark.py`](https://github.com/getfluvo/odx/blob/main/scripts/benchmark.py))
builds the mirrors under `target/bench`, installs git-aggregator 4.1 in a
virtual environment of its own with `uv`, prints the table and rewrites the
chart in `docs/_static/`.
