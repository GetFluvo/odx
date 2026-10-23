"""A forge that keeps issues in memory, enough for `odx bot` on CI.

Serves the GitHub API for one repository, `acme/project`, on localhost,
and appends every request to a log file, one line each, so that a job can
tell what a run wrote. Run it as `python3 scripts/fake_forge.py PORT LOG`.
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

PREFIX = "/api/v3/repos/acme/project"
ITEMS: list[dict] = []


class Forge(BaseHTTPRequestHandler):
    def answer(self, status: int, body: object) -> None:
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def handle_any(self, method: str) -> None:
        url = urlsplit(self.path)
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length).decode() if length else ""
        with open(LOG, "a", encoding="utf-8") as log:
            log.write(f"{method} {url.path} {raw}\n")
        body = json.loads(raw) if raw else {}
        query = {key: values[0] for key, values in parse_qs(url.query).items()}
        rest = url.path.removeprefix(PREFIX) if url.path.startswith(PREFIX) else None
        if method == "GET" and url.path == "/api/v3/meta":
            return self.answer(200, {})
        if method == "GET" and rest == "":
            return self.answer(200, {"private": True, "permissions": {"push": True}})
        if method == "GET" and rest == "/issues":
            label = query.get("labels", "")
            found = [i for i in ITEMS if any(l["name"] == label for l in i["labels"])]
            return self.answer(200, found)
        if method == "POST" and rest == "/issues":
            number = len(ITEMS) + 1
            item = {
                "number": number,
                "title": body["title"],
                "body": body["body"],
                "state": "open",
                "labels": [{"name": name} for name in body.get("labels", [])],
                "html_url": f"http://{self.headers['Host']}/acme/project/issues/{number}",
            }
            ITEMS.append(item)
            return self.answer(201, item)
        if method == "PATCH" and rest and rest.startswith("/issues/"):
            number = int(rest.removeprefix("/issues/"))
            item = next((i for i in ITEMS if i["number"] == number), None)
            if item is None:
                return self.answer(404, {})
            item.update({k: body[k] for k in ("title", "body", "state") if k in body})
            return self.answer(200, item)
        return self.answer(404, {})

    def do_GET(self) -> None:
        self.handle_any("GET")

    def do_POST(self) -> None:
        self.handle_any("POST")

    def do_PATCH(self) -> None:
        self.handle_any("PATCH")

    def log_message(self, format: str, *args: object) -> None:
        pass


if __name__ == "__main__":
    LOG = sys.argv[2]
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Forge).serve_forever()
