"""The number of the first dashboard issue in the JSON of `odx bot run`."""

import json
import sys

with open(sys.argv[1], encoding="utf-8") as plan:
    numbers = [d["issue"].get("number") for d in json.load(plan)["dashboards"]]
print(next((n for n in numbers if n), ""))
