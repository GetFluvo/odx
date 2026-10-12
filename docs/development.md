# Development

See [CONTRIBUTING.md](https://github.com/getfluvo/odx/blob/main/CONTRIBUTING.md)
for the full guide. Every quality gate runs through `just`:

```console
$ just all        # every gate, as in CI
$ just clippy     # clippy on all targets, warnings are errors
$ just cov        # 100% line, function and region coverage
```
