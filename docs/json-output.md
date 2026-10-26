# JSON output

Every command that reports something takes `--json` and prints one line of
JSON on standard output; warnings and errors stay on standard error. The
layout of each output is published as a [JSON Schema](https://json-schema.org)
(draft 2020-12), generated from the types odx prints, so the schema and the
output cannot drift apart.

```console
$ odx --json status | jq '.pull_requests[] | select(.state == "merged") | .name'
```

## Stability

Each output carries a `schema` number (`odx --json dashboard` prints a list
of dashboards, each with its own). It changes only when a field is removed
or changes meaning. New fields can appear in any release, so read the fields
you need and ignore the others. The order of keys is not part of the
contract.

## Schemas

| Output | Command | Schema |
| --- | --- | --- |
| `aggregate` | odx aggregate | [aggregate.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/aggregate.schema.json) |
| `bot` | odx bot | [bot.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/bot.schema.json) |
| `cache-info` | odx cache info | [cache-info.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/cache-info.schema.json) |
| `changelog` | odx changelog | [changelog.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/changelog.schema.json) |
| `check-deps` | odx check-deps | [check-deps.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/check-deps.schema.json) |
| `daemon-status` | odx daemon status | [daemon-status.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/daemon-status.schema.json) |
| `dashboard` | odx dashboard | [dashboard.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/dashboard.schema.json) |
| `diff` | odx diff | [diff.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/diff.schema.json) |
| `doctor` | odx doctor | [doctor.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/doctor.schema.json) |
| `env-diff` | odx env diff | [env-diff.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/env-diff.schema.json) |
| `env-list` | odx env list | [env-list.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/env-list.schema.json) |
| `glue` | odx glue | [glue.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/glue.schema.json) |
| `lock-auto-update` | odx lock --auto-update | [lock-auto-update.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/lock-auto-update.schema.json) |
| `migrate` | odx migrate | [migrate.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/migrate.schema.json) |
| `migrate-own` | odx migrate own | [migrate-own.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/migrate-own.schema.json) |
| `migrate-pin` | odx migrate pin | [migrate-pin.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/migrate-pin.schema.json) |
| `migrate-plan` | odx migrate plan | [migrate-plan.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/migrate-plan.schema.json) |
| `migrate-plan-repository` | odx migrate plan (repository) | [migrate-plan-repository.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/migrate-plan-repository.schema.json) |
| `migrate-pr` | odx migrate pr | [migrate-pr.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/migrate-pr.schema.json) |
| `migrate-unpin` | odx migrate unpin | [migrate-unpin.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/migrate-unpin.schema.json) |
| `migrate-update` | odx migrate update | [migrate-update.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/migrate-update.schema.json) |
| `port` | odx port | [port.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/port.schema.json) |
| `port-sweep` | odx port sweep | [port-sweep.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/port-sweep.schema.json) |
| `promote` | odx promote | [promote.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/promote.schema.json) |
| `promote-rollback` | odx promote --rollback | [promote-rollback.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/promote-rollback.schema.json) |
| `promote-search` | odx promote --search | [promote-search.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/promote-search.schema.json) |
| `sniff` | odx sniff | [sniff.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/sniff.schema.json) |
| `stats` | odx stats | [stats.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/stats.schema.json) |
| `stats-per-addon` | odx stats --per-addon | [stats-per-addon.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/stats-per-addon.schema.json) |
| `status` | odx status | [status.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/status.schema.json) |
| `suggest` | odx suggest | [suggest.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/suggest.schema.json) |
| `sync` | odx sync | [sync.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/sync.schema.json) |
| `upgrade` | odx upgrade | [upgrade.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/upgrade.schema.json) |
| `vendor` | odx vendor | [vendor.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/vendor.schema.json) |
| `verify` | odx verify | [verify.schema.json](https://github.com/getfluvo/odx/blob/main/docs/json/verify.schema.json) |
