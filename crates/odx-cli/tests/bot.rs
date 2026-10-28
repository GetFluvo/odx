//! `odx bot` on a project whose repository lives on a local forge that
//! keeps issues and pull requests (GitHub and Gitea flavours), aggregating
//! an upstream behind a second local forge. Runs the `odx` binary so git
//! can be pointed at repositories on disk; every request is logged, so the
//! tests can tell what was written, and where.

#![cfg(unix)]
#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]
#![expect(
    clippy::indexing_slicing,
    reason = "the fake forge reads and writes JSON by key, failing loudly"
)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Jane")
        .env("GIT_AUTHOR_EMAIL", "jane@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn change(dir: &Path, files: &[(&str, &str)], message: &str) {
    for (file, content) in files {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", message]);
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// One HTTP request: method, path, query and body.
struct Request {
    method: String,
    path: String,
    query: String,
    body: String,
}

fn read_request(stream: &mut TcpStream) -> Request {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => request.extend(buffer.iter().take(n)),
        }
    }
    let text = String::from_utf8_lossy(&request).into_owned();
    let length: usize = text
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0);
    let head_end = text.find("\r\n\r\n").map_or(text.len(), |at| at + 4);
    let mut body = request.get(head_end..).unwrap_or_default().to_vec();
    while body.len() < length {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend(buffer.iter().take(n)),
        }
    }
    let mut words = text.split_whitespace();
    let method = words.next().unwrap_or("GET").to_owned();
    let target = words.next().unwrap_or("/").to_owned();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Request {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// Serve `answer` on localhost; the base URL.
fn serve(answer: impl Fn(&Request) -> (u16, String) + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let request = read_request(&mut stream);
            let (status, body) = answer(&request);
            respond(&mut stream, status, &body);
        }
    });
    base
}

/// The project's forge: its issues and pull requests, kept.
#[derive(Default)]
struct State {
    gitea: bool,
    public: bool,
    items: Vec<Value>,
    labels: Vec<String>,
    log: Vec<String>,
    base: String,
    /// Requests answered with HTTP 500: method, path and a part of the query.
    failing: Vec<(String, String, String)>,
    /// Fixed answers to GET requests, by path below the repository.
    fixed: HashMap<String, String>,
}

impl State {
    fn prefix(&self) -> &'static str {
        if self.gitea {
            "/api/v1/repos/acme/project"
        } else {
            "/api/v3/repos/acme/project"
        }
    }

    /// Label names from a request: names on GitHub, ids on Gitea.
    fn label_names(&self, labels: &Value) -> Vec<String> {
        labels
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(|label| {
                label.as_str().map(str::to_owned).or_else(|| {
                    label
                        .as_u64()
                        .and_then(|id| self.labels.get(usize::try_from(id).ok()? - 1).cloned())
                })
            })
            .collect()
    }

    fn item(&mut self, number: u64) -> Option<&mut Value> {
        self.items
            .iter_mut()
            .find(|item| item["number"].as_u64() == Some(number))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one route per arm, as a forge has them"
    )]
    fn answer(&mut self, request: &Request) -> (u16, String) {
        self.log.push(format!(
            "{} {}{} {}",
            request.method,
            request.path,
            if request.query.is_empty() {
                String::new()
            } else {
                format!("?{}", request.query)
            },
            request.body
        ));
        if self.failing.iter().any(|(method, path, query)| {
            *method == request.method
                && *path == request.path
                && request.query.contains(query.as_str())
        }) {
            return (500, String::new());
        }
        let body: Value = serde_json::from_str(&request.body).unwrap_or_default();
        let prefix = self.prefix();
        let rest = request.path.strip_prefix(prefix);
        let query: HashMap<&str, &str> = request
            .query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .collect();
        if let Some(fixed) = rest
            .and_then(|rest| self.fixed.get(rest))
            .filter(|_| request.method == "GET")
        {
            return (200, fixed.clone());
        }
        match (request.method.as_str(), rest) {
            ("GET", _) if request.path == "/api/v1/version" && self.gitea => (200, "{}".to_owned()),
            ("GET", _) if request.path == "/api/v3/meta" && !self.gitea => (200, "{}".to_owned()),
            ("GET", Some("")) => (
                200,
                json!({"private": !self.public, "permissions": {"push": true}}).to_string(),
            ),
            ("GET", Some("/issues")) => {
                let label = query.get("labels").copied().unwrap_or_default();
                let kind = query.get("type").copied();
                let found: Vec<&Value> = self
                    .items
                    .iter()
                    .filter(|item| {
                        item["labels"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|l| l["name"] == label)
                    })
                    .filter(|item| match kind {
                        Some("pulls") => item["pull_request"].is_object(),
                        Some("issues") => !item["pull_request"].is_object(),
                        _ => true,
                    })
                    .collect();
                (200, json!(found).to_string())
            }
            ("POST", Some("/issues")) => {
                let number = self.items.len() as u64 + 1;
                let labels: Vec<Value> = self
                    .label_names(&body["labels"])
                    .into_iter()
                    .map(|name| json!({ "name": name }))
                    .collect();
                let item = json!({
                    "number": number, "title": body["title"], "body": body["body"],
                    "state": "open", "labels": labels,
                    "html_url": format!("{}/acme/project/issues/{number}", self.base),
                });
                self.items.push(item.clone());
                (201, item.to_string())
            }
            ("POST", Some("/pulls")) => {
                let number = self.items.len() as u64 + 1;
                let item = json!({
                    "number": number, "title": body["title"], "body": body["body"],
                    "state": "open", "labels": [], "pull_request": {},
                    "head": {"ref": body["head"]}, "base": {"ref": body["base"]},
                    "html_url": format!("{}/acme/project/pull/{number}", self.base),
                });
                self.items.push(item.clone());
                (201, item.to_string())
            }
            ("GET", Some("/labels")) => {
                let listed: Vec<Value> = self
                    .labels
                    .iter()
                    .enumerate()
                    .map(|(at, name)| json!({"id": at + 1, "name": name}))
                    .collect();
                (200, json!(listed).to_string())
            }
            ("POST", Some("/labels")) => {
                self.labels.push(body["name"].as_str().unwrap().to_owned());
                (
                    201,
                    json!({"id": self.labels.len(), "name": body["name"]}).to_string(),
                )
            }
            (method, Some(path)) if path.starts_with("/issues/") => {
                let rest = path.trim_start_matches("/issues/");
                let (number, more) = rest.split_once('/').unwrap_or((rest, ""));
                let labels = more == "labels";
                let number: u64 = number.parse().unwrap();
                let names = self.label_names(&body["labels"]);
                let Some(item) = self.item(number) else {
                    return (404, String::new());
                };
                if more == "comments" {
                    let comments = item
                        .as_object_mut()
                        .unwrap()
                        .entry("comments")
                        .or_insert_with(|| json!([]));
                    if method == "POST" {
                        let comment = json!({
                            "id": comments.as_array().unwrap().len() + 1,
                            "user": {"login": "odx-bot"},
                            "body": body["body"],
                        });
                        comments.as_array_mut().unwrap().push(comment.clone());
                        return (201, comment.to_string());
                    }
                    return (200, comments.to_string());
                }
                match (method, labels) {
                    ("POST", true) => {
                        let list = item["labels"].as_array_mut().unwrap();
                        list.extend(names.into_iter().map(|name| json!({ "name": name })));
                        (200, "[]".to_owned())
                    }
                    ("PATCH", false) => {
                        for key in ["title", "body", "state"] {
                            if let Some(value) = body.get(key) {
                                item[key] = value.clone();
                            }
                        }
                        (200, item.to_string())
                    }
                    _ => (404, String::new()),
                }
            }
            _ => (404, String::new()),
        }
    }
}

/// The upstream's forge: fixed answers, and what was asked of it.
type Routes = Arc<Mutex<HashMap<String, (u16, String)>>>;

struct Setup {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    up: PathBuf,
    project: PathBuf,
    bare: PathBuf,
    up_api: String,
    api: String,
    routes: Routes,
    upstream_log: Arc<Mutex<Vec<String>>>,
    state: Arc<Mutex<State>>,
}

impl Setup {
    /// `OCA/pos` with pull request 2 merged on 16.0 (missing on 17.0) and
    /// pull requests 1 (which the project merges) and 5 open on 17.0; the
    /// project on `main` (test), with `prod` one addon behind.
    #[expect(clippy::too_many_lines, reason = "the whole fixture, in order")]
    fn new(gitea: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let up = root.join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=16.0"]);
        change(
            &up,
            &[("pos_x/__manifest__.py", "{'version': '16'}")],
            "[MIG] pos_x: 16.0",
        );
        git(&up, &["checkout", "--quiet", "-b", "pr2"]);
        change(&up, &[("pos_x/b.py", "b\n")], "[FIX] pos_x: b");
        git(&up, &["checkout", "--quiet", "16.0"]);
        git(
            &up,
            &[
                "merge",
                "--quiet",
                "--no-ff",
                "pr2",
                "-m",
                "Merge PR #2 into 16.0",
            ],
        );
        git(&up, &["checkout", "--quiet", "--orphan", "17.0"]);
        git(&up, &["rm", "-r", "--quiet", "-f", "."]);
        change(
            &up,
            &[("pos_x/__manifest__.py", "{'version': '17'}")],
            "[MIG] pos_x: 17.0",
        );
        for (n, file) in [(1, "e.py"), (5, "f.py")] {
            git(&up, &["checkout", "--quiet", "-b", &format!("pr{n}")]);
            change(
                &up,
                &[(&format!("pos_x/{file}"), "x\n")],
                &format!("[FIX] pos_x: {n}"),
            );
            let head = git(&up, &["rev-parse", "HEAD"]);
            git(&up, &["update-ref", &format!("refs/pull/{n}/head"), &head]);
            git(&up, &["checkout", "--quiet", "17.0"]);
        }
        let routes: Routes = Arc::default();
        let upstream_log: Arc<Mutex<Vec<String>>> = Arc::default();
        let up_api = {
            let routes = Arc::clone(&routes);
            let log = Arc::clone(&upstream_log);
            serve(move |request| {
                log.lock()
                    .unwrap()
                    .push(format!("{} {}", request.method, request.path));
                routes
                    .lock()
                    .unwrap()
                    .get(&request.path)
                    .cloned()
                    .unwrap_or((404, String::new()))
            })
        };
        let state = Arc::new(Mutex::new(State {
            gitea,
            ..State::default()
        }));
        let api = {
            let state = Arc::clone(&state);
            serve(move |request| state.lock().unwrap().answer(request))
        };
        state.lock().unwrap().base.clone_from(&api);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        git(&project, &["init", "--quiet", "--initial-branch=main"]);
        std::fs::write(
            project.join("pyproject.toml"),
            format!(
                "[project]\nname = \"p\"\nversion = \"1\"\n\n[tool.odx]\nodoo_version = \"17.0\"\nmergiraf = \"never\"\n\n[tool.odx.port]\ndefault_from = [\"16.0\"]\n\n[tool.odx.environments]\npromotion = [[\"test\", \"prod\"]]\n\n[tool.odx.environments.env.test]\nbranch = \"main\"\n\n[tool.odx.environments.env.prod]\nbranch = \"prod\"\n\n[tool.odx.bot]\nenvironment = \"test\"\nlabels = [\"bot\"]\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{up_api}/OCA/pos.git\"\naddons = [\"pos_x\"]\nmerges = [{{ pr = 1 }}]\n"
            ),
        )
        .unwrap();
        std::fs::write(project.join(".gitignore"), "/external/\n").unwrap();
        git(&project, &["add", "-A"]);
        git(&project, &["commit", "--quiet", "-m", "project"]);
        git(&project, &["branch", "prod"]);
        change(
            &project,
            &[("my_addon/__manifest__.py", "{'version': '17.0.1.0.0'}")],
            "[ADD] my_addon",
        );
        let bare = root.join("project.git");
        git(
            &root,
            &["init", "--quiet", "--bare", &bare.display().to_string()],
        );
        let setup = Self {
            _tmp: tmp,
            root,
            up,
            project,
            bare,
            up_api,
            api,
            routes,
            upstream_log,
            state,
        };
        git(
            &setup.project,
            &["remote", "add", "origin", &setup.remote()],
        );
        setup.route("/api/v3/meta", 200, "{}");
        let head = |n: u32| git(&setup.up, &["rev-parse", &format!("refs/pull/{n}/head")]);
        for n in [1, 5] {
            setup.route(
                &format!("/api/v3/repos/OCA/pos/pulls/{n}"),
                200,
                &format!(
                    r#"{{"number": {n}, "title": "[17.0][FIX] pos_x: {n}", "state": "open", "merged_at": null, "base": {{"ref": "17.0"}}, "head": {{"sha": "{}"}}}}"#,
                    head(n)
                ),
            );
        }
        setup.route(
            "/api/v3/repos/OCA/pos/pulls",
            200,
            r#"[{"number": 5, "title": "[17.0][FIX] pos_x: 5", "state": "open", "base": {"ref": "17.0"}},
               {"number": 1, "title": "[17.0][FIX] pos_x: 1", "state": "open", "base": {"ref": "17.0"}}]"#,
        );
        let (code, out, err) = setup.odx(&["aggregate"]);
        assert_eq!(code, 0, "{out}{err}");
        git(&setup.project, &["add", "-A"]);
        git(&setup.project, &["commit", "--quiet", "-m", "lock"]);
        setup.git(&["push", "--quiet", "origin", "main", "prod"]);
        setup.git(&["fetch", "--quiet", "origin"]);
        setup
    }

    fn remote(&self) -> String {
        format!("{}/acme/project.git", self.api)
    }

    fn route(&self, path: &str, status: u16, body: &str) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, body.to_owned()));
    }

    fn config(&self) -> Vec<(String, String)> {
        vec![
            (
                format!("url.{}.insteadOf", file_url(&self.up)),
                format!("{}/OCA/pos.git", self.up_api),
            ),
            (
                format!("url.{}.insteadOf", file_url(&self.bare)),
                self.remote(),
            ),
        ]
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.project)
            .env("ODX_CACHE_DIR", self.root.join("cache"))
            .env("ODX_CONFIG_DIR", self.root.join("config"))
            .env("ODX_UV", "true")
            .env("GIT_AUTHOR_NAME", "Jane")
            .env("GIT_AUTHOR_EMAIL", "jane@example.com")
            .env("GIT_COMMITTER_NAME", "odx")
            .env("GIT_COMMITTER_EMAIL", "odx@example.com")
            .env("GITHUB_SERVER_URL", "https://forge.example")
            .env("GITHUB_REPOSITORY", "acme/project")
            .env("GITHUB_RUN_ID", "7")
            .env_remove("CI")
            .env("GIT_CONFIG_COUNT", "2");
        for (at, (key, value)) in self.config().into_iter().enumerate() {
            command
                .env(format!("GIT_CONFIG_KEY_{at}"), key)
                .env(format!("GIT_CONFIG_VALUE_{at}"), value);
        }
        command
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self.command("git").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn odx(&self, args: &[&str]) -> (i32, String, String) {
        let out = self
            .command(env!("CARGO_BIN_EXE_odx"))
            .args(args)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The requests the project's forge got from here on: run `f` and
    /// return what it wrote.
    fn writes(&self, f: impl FnOnce()) -> Vec<String> {
        let before = self.state.lock().unwrap().log.len();
        f();
        self.state.lock().unwrap().log[before..]
            .iter()
            .filter(|line| !line.starts_with("GET "))
            .cloned()
            .collect()
    }

    fn issue(&self, number: u64) -> Value {
        self.state.lock().unwrap().item(number).unwrap().clone()
    }

    /// Change the body of issue `number` as a person would.
    fn edit(&self, number: u64, change: impl FnOnce(&str) -> String) {
        let mut state = self.state.lock().unwrap();
        let item = state.item(number).unwrap();
        let body = change(item["body"].as_str().unwrap());
        item["body"] = json!(body);
        drop(state);
    }

    /// Tick the box of `action` on `id` in issue `number`.
    fn tick(&self, number: u64, action: &str, id: &str) {
        self.edit(number, |body| {
            let ticked = body.replace(
                &format!("- [ ] {action} <!-- odx:{action} {id} -->"),
                &format!("- [x] {action} <!-- odx:{action} {id} -->"),
            );
            assert!(ticked.contains(&format!("[x] {action}")), "{ticked}");
            ticked
        });
    }

    fn run(&self) -> String {
        let (code, out, err) = self.odx(&["bot", "run", "--apply"]);
        assert_eq!(code, 0, "{out}{err}");
        out
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "one scenario, run after run")]
fn the_dashboard_issue_and_pull_requests_on_github() {
    let setup = Setup::new(false);
    // The plan writes nothing.
    let writes = setup.writes(|| {
        let (code, out, err) = setup.odx(&["bot", "plan"]);
        assert_eq!(code, 0, "{out}{err}");
        let plan: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(plan["repository"], "127.0.0.1/acme/project");
        assert_eq!(plan["base"], "main");
        assert_eq!(plan["dashboards"][0]["issue"]["write"], "would be created");
    });
    assert_eq!(writes, Vec::<String>::new());
    // The first run opens the issue.
    let writes = setup.writes(|| {
        let out = setup.run();
        assert_eq!(out, "17.0: dashboard issue #1 created\n");
    });
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(writes[0].starts_with("POST /api/v3/repos/acme/project/issues "));
    let issue = setup.issue(1);
    let body = issue["body"].as_str().unwrap();
    assert!(
        body.starts_with("<!-- odx:dashboard series=17.0 -->\n# odx upstream dashboard (17.0)\n"),
        "{body}"
    );
    assert!(
        body.contains("([workflow run](https://forge.example/acme/project/actions/runs/7))"),
        "{body}"
    );
    for line in [
        "  - [ ] include <!-- odx:include new:OCA/pos#5 -->\n",
        "  - [ ] mute <!-- odx:mute fwd:OCA/pos#2->17.0 -->\n",
        "  - port: port upstream with `odx dashboard act fwd:OCA/pos#2->17.0 --port --open-pr`\n",
        "  - [ ] promote <!-- odx:promote drift:test->prod:addon/my_addon -->\n",
    ] {
        assert!(body.contains(line), "{line}\n---\n{body}");
    }
    assert_eq!(issue["labels"][0]["name"], "odx:dashboard");
    // Nothing changed: nothing is written.
    let writes = setup.writes(|| {
        assert_eq!(setup.run(), "17.0: dashboard issue #1 unchanged\n");
    });
    assert_eq!(writes, Vec::<String>::new());
    // A person keeps notes; ticked boxes become pull requests.
    setup.edit(1, |body| {
        body.replace(
            "Notes below this line are kept between runs.\n",
            "Reviewed on Monday.\n",
        )
    });
    setup.tick(1, "include", "new:OCA/pos#5");
    setup.tick(1, "mute", "fwd:OCA/pos#2->17.0");
    setup.tick(1, "promote", "drift:test->prod:addon/my_addon");
    let (code, out, err) = setup.odx(&["bot", "run", "--apply", "--max-prs", "2"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(
        out,
        format!(
            "17.0: include new:OCA/pos#5: opened #2 {api}/acme/project/pull/2\n\
             17.0: mute fwd:OCA/pos#2->17.0: opened #3 {api}/acme/project/pull/3\n\
             17.0: promote drift:test->prod:addon/my_addon: deferred to the next run (--max-prs)\n\
             17.0: dashboard issue #1 updated\n",
            api = setup.api
        )
    );
    let pull = setup.issue(2);
    assert_eq!(pull["title"], "chore(deps): add pos_x (new:OCA/pos#5)");
    assert_eq!(pull["base"]["ref"], "main");
    assert_eq!(pull["head"]["ref"], "odx/add/new-oca-pos-5");
    let labels: Vec<&str> = pull["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|label| label["name"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["odx:bot", "odx:add", "bot"]);
    assert!(
        pull["body"].as_str().unwrap().starts_with(
            "<!-- odx:pr kind=add key=new-oca-pos-5 entry=include:new:OCA/pos#5 -->\n"
        ),
        "{pull}"
    );
    // The branch adds the merge and locks it.
    let pyproject = git(
        &setup.bare,
        &["show", "odx/add/new-oca-pos-5:pyproject.toml"],
    );
    assert!(pyproject.contains("pr = 5"), "{pyproject}");
    let lock = git(&setup.bare, &["show", "odx/add/new-oca-pos-5:odx.lock"]);
    assert!(lock.contains("pr = 5"), "{lock}");
    // The mute goes into the project, through its pull request.
    let mutes = git(
        &setup.bare,
        &["show", "odx/mute/fwd-oca-pos-2-17.0:.odx/mutes.toml"],
    );
    assert!(mutes.contains("muted from the dashboard (#1)"), "{mutes}");
    assert!(mutes.contains("until = \""), "{mutes}");
    let body = setup.issue(1)["body"].as_str().unwrap().to_owned();
    assert!(
        body.contains(&format!(
            "  - include: [#2]({}/acme/project/pull/2)\n",
            setup.api
        )),
        "{body}"
    );
    assert!(
        body.contains("  - [x] promote <!-- odx:promote drift:test->prod:addon/my_addon -->\n"),
        "{body}"
    );
    assert!(body.contains("\n## Open bot pull requests (2)\n"), "{body}");
    assert!(
        body.ends_with("<!-- odx:notes -->\nReviewed on Monday.\n"),
        "{body}"
    );
    // The deferred promotion follows; the open pull requests stay as they are.
    let writes = setup.writes(|| {
        let out = setup.run();
        assert!(
            out.starts_with(&format!(
                "17.0: promote drift:test->prod:addon/my_addon: opened #4 {}/acme/project/pull/4\n",
                setup.api
            )),
            "{out}"
        );
    });
    assert_eq!(writes.len(), 3, "{writes:?}");
    let promotion = setup.issue(4);
    assert_eq!(promotion["base"]["ref"], "prod");
    assert_eq!(
        promotion["title"],
        "chore: promote my_addon from test to prod"
    );
    let promoted = git(
        &setup.bare,
        &[
            "show",
            "odx/promote/drift-test-prod-addon-my-addon:my_addon/__manifest__.py",
        ],
    );
    assert!(promoted.contains("17.0.1.0.0"), "{promoted}");
    assert_eq!(
        setup.writes(|| {
            setup.run();
        }),
        Vec::<String>::new()
    );
    // Ticked again while its pull request is open: updated, not duplicated.
    setup.edit(1, |body| {
        body.replace(
            &format!("  - include: [#2]({}/acme/project/pull/2)\n", setup.api),
            "  - [x] include <!-- odx:include new:OCA/pos#5 -->\n",
        )
    });
    let out = setup.run();
    assert!(
        out.starts_with(&format!(
            "17.0: include new:OCA/pos#5: updated #2 {}/acme/project/pull/2\n",
            setup.api
        )),
        "{out}"
    );
    // Only GETs went to the upstream.
    let upstream = setup.upstream_log.lock().unwrap().clone();
    assert!(
        upstream.iter().all(|line| line.starts_with("GET ")),
        "{upstream:?}"
    );
}

#[test]
fn closing_the_issue_pauses_the_dashboard() {
    let setup = Setup::new(false);
    // Mutes of the project's own, kept as they are when the bot adds one.
    let mutes = setup.project.join(".odx").join("mutes.toml");
    std::fs::create_dir_all(mutes.parent().unwrap()).unwrap();
    std::fs::write(&mutes, "").unwrap();
    git(&setup.project, &["add", "-A"]);
    git(&setup.project, &["commit", "--quiet", "-m", "mutes"]);
    setup.git(&["push", "--quiet", "origin", "main"]);
    setup.git(&["fetch", "--quiet", "origin"]);
    setup.run();
    setup.state.lock().unwrap().item(1).unwrap()["state"] = json!("closed");
    let writes = setup.writes(|| {
        assert_eq!(
            setup.run(),
            "17.0: the dashboard is paused (its issue is closed; --reopen reopens it)\n"
        );
    });
    assert_eq!(writes, Vec::<String>::new());
    let (code, out, err) = setup.odx(&["bot", "dashboard", "--apply", "--reopen"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "17.0: dashboard issue #1 updated\n"),
        "{err}"
    );
    assert_eq!(setup.issue(1)["state"], "open");
    // `odx bot dashboard` leaves ticked boxes ticked; `act` takes them.
    setup.tick(1, "mute", "fwd:OCA/pos#2->17.0");
    let (code, out, _) = setup.odx(&["bot", "dashboard", "--apply"]);
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            "17.0: mute fwd:OCA/pos#2->17.0: planned\n17.0: dashboard issue #1 unchanged\n"
        )
    );
    // An entry without an addon, on a base without its remote branch.
    setup.tick(1, "drop", "pr:OCA/pos#1");
    git(
        &setup.project,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let (code, out, _) = setup.odx(&["bot", "act", "--from-issue"]);
    assert_eq!(code, 0);
    assert!(
        out.ends_with("17.0: dashboard issue #1 unchanged\n"),
        "{out}"
    );
    let (code, out, err) = setup.odx(&[
        "--json",
        "bot",
        "act",
        "--from-issue",
        "--apply",
        "--series",
        "17.0",
    ]);
    assert_eq!(code, 0, "{err}");
    let value: Value = serde_json::from_str(&out).unwrap();
    let requests = &value["dashboards"][0]["requests"];
    assert_eq!(requests[0]["result"]["outcome"], "opened", "{value:#}");
    assert_eq!(requests[1]["result"]["outcome"], "opened", "{value:#}");
    let titles: Vec<String> = [2, 3]
        .iter()
        .map(|n| setup.issue(*n)["title"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        titles.contains(&"chore(deps): drop pr:OCA/pos#1".to_owned()),
        "{titles:?}"
    );
    let (_, out, _) = setup.odx(&["bot", "run", "--series", "16.0"]);
    assert_eq!(out, "");
}

#[test]
fn refusals() {
    let setup = Setup::new(false);
    setup.state.lock().unwrap().public = true;
    let (code, _, err) = setup.odx(&["bot", "run", "--apply"]);
    assert_eq!(code, 1);
    assert!(err.contains("/acme/project is public"), "{err}");
    let (code, _, err) = setup.odx(&["bot", "run", "--remote", "nowhere"]);
    assert_eq!(code, 2);
    assert_eq!(err, "error: nowhere is not on a forge\n");
}

#[test]
fn the_same_on_gitea_with_a_verdict_repository() {
    let setup = Setup::new(true);
    let verdicts = setup.root.join("verdicts.git");
    git(
        &setup.root,
        &["init", "--quiet", "--bare", &verdicts.display().to_string()],
    );
    std::fs::create_dir_all(setup.root.join("config")).unwrap();
    std::fs::write(
        setup.root.join("config").join("config.toml"),
        format!("[verdicts]\nrepo = \"{}\"\n", verdicts.display()),
    )
    .unwrap();
    let writes = setup.writes(|| {
        assert_eq!(setup.run(), "17.0: dashboard issue #1 created\n");
    });
    assert!(
        writes[0].starts_with("POST /api/v1/repos/acme/project/labels "),
        "{writes:?}"
    );
    assert!(writes[1].contains(r#""labels":[1]"#), "{writes:?}");
    assert_eq!(
        setup.writes(|| {
            setup.run();
        }),
        Vec::<String>::new()
    );
    // A mute goes straight into the verdict repository.
    setup.tick(1, "mute", "fwd:OCA/pos#2->17.0");
    let out = setup.run();
    assert!(
        out.starts_with("17.0: mute fwd:OCA/pos#2->17.0: recorded m:17.0:pos_x:"),
        "{out}"
    );
    let body = setup.issue(1)["body"].as_str().unwrap().to_owned();
    assert!(body.contains("  - mute: recorded m:17.0:pos_x:"), "{body}");
    // A ticked box that fails stays ticked.
    setup.tick(1, "include", "new:OCA/pos#5");
    git(&setup.up, &["update-ref", "-d", "refs/pull/5/head"]);
    let out = setup.run();
    assert!(
        out.contains("17.0: include new:OCA/pos#5: failed: "),
        "{out}"
    );
    let body = setup.issue(1)["body"].as_str().unwrap().to_owned();
    assert!(
        body.contains("  - [x] include <!-- odx:include new:OCA/pos#5 -->\n"),
        "{body}"
    );
}

#[test]
fn forge_failures_end_the_run() {
    let setup = Setup::new(false);
    let issues = "/api/v3/repos/acme/project/issues";
    for (method, path, query, message) in [
        ("GET", "/api/v3/meta", "", "does not look like"),
        ("GET", "/api/v3/repos/acme/project", "", "answered HTTP 500"),
        ("GET", issues, "labels=odx:dashboard", "answered HTTP 500"),
        ("GET", issues, "labels=odx:bot", "answered HTTP 500"),
        ("GET", issues, "labels=promote", "answered HTTP 500"),
        ("POST", issues, "", "answered HTTP 500"),
    ] {
        setup.state.lock().unwrap().failing =
            vec![(method.to_owned(), path.to_owned(), query.to_owned())];
        let (code, _, err) = setup.odx(&["bot", "run", "--apply"]);
        assert_eq!(code, 1, "{method} {path}: {err}");
        assert!(err.contains(message), "{method} {path}: {err}");
    }
    setup.state.lock().unwrap().failing.clear();
    setup.run();
    setup.edit(1, |body| body.replace("Tick a box", "Tick one box"));
    setup.state.lock().unwrap().failing =
        vec![("PATCH".to_owned(), format!("{issues}/1"), String::new())];
    let (code, _, err) = setup.odx(&["bot", "run", "--apply"]);
    assert_eq!(code, 1, "{err}");
    // Outside a project, and on no branch without an environment for the bot.
    let (code, _, _) = setup.odx(&["-C", "..", "bot", "plan"]);
    assert_eq!(code, 2);
    let pyproject = std::fs::read_to_string(setup.project.join("pyproject.toml")).unwrap();
    std::fs::write(
        setup.project.join("pyproject.toml"),
        pyproject.replace(
            "[tool.odx.bot]\nenvironment = \"test\"\nlabels = [\"bot\"]\n",
            "",
        ),
    )
    .unwrap();
    let (code, out, err) = setup.odx(&["bot", "plan"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"base\":\"main\""), "{out}");
    git(&setup.project, &["checkout", "--quiet", "--detach"]);
    let (code, _, err) = setup.odx(&["bot", "plan"]);
    assert_eq!(code, 2);
    assert!(err.contains("no branch checked out"), "{err}");
    // A dashboard that cannot be built.
    std::fs::write(
        setup.project.join("pyproject.toml"),
        pyproject.replace(
            "default_from = [\"16.0\"]\n",
            "default_from = [\"16.0\"]\nmigration_commit_patterns = [\"(\"]\n",
        ),
    )
    .unwrap();
    let (code, _, err) = setup.odx(&["bot", "plan"]);
    assert_ne!(code, 0, "{err}");
}

/// The pull request's head moved upstream: a new commit on it.
fn move_head(setup: &Setup, n: u32, file: &str) -> String {
    git(&setup.up, &["checkout", "--quiet", &format!("pr{n}")]);
    change(
        &setup.up,
        &[(&format!("pos_x/{file}"), "y\n")],
        "[FIX] more",
    );
    let head = git(&setup.up, &["rev-parse", "HEAD"]);
    git(
        &setup.up,
        &["update-ref", &format!("refs/pull/{n}/head"), &head],
    );
    git(&setup.up, &["checkout", "--quiet", "17.0"]);
    setup.route(
        &format!("/api/v3/repos/OCA/pos/pulls/{n}"),
        200,
        &format!(
            r#"{{"number": {n}, "title": "[17.0][FIX] pos_x: {n}", "state": "open", "merged_at": null, "base": {{"ref": "17.0"}}, "head": {{"sha": "{head}"}}}}"#
        ),
    );
    head
}

/// CI on `sha` upstream reports nothing to wait for.
fn no_checks(setup: &Setup, sha: &str) {
    let commits = format!("/api/v3/repos/OCA/pos/commits/{sha}");
    setup.route(
        &format!("{commits}/check-runs"),
        200,
        r#"{"check_runs": []}"#,
    );
    setup.route(&format!("{commits}/status"), 200, r#"{"statuses": []}"#);
}

#[test]
#[expect(clippy::too_many_lines, reason = "one scenario, run after run")]
fn routine_pull_requests_and_promotions_by_label() {
    let setup = Setup::new(false);
    let mirror = setup.root.join("mirror.git");
    git(
        &setup.root,
        &["init", "--quiet", "--bare", &mirror.display().to_string()],
    );
    let pyproject = std::fs::read_to_string(setup.project.join("pyproject.toml")).unwrap();
    std::fs::write(
        setup.project.join("pyproject.toml"),
        pyproject.replace(
            "labels = [\"bot\"]\n",
            &format!(
                "labels = [\"bot\"]\nvault_heads = true\n\n[tool.odx.mirror]\nurl = \"{}\"\n",
                mirror.display()
            ),
        ),
    )
    .unwrap();
    let (code, out, err) = setup.odx(&["lock"]);
    assert_eq!(code, 0, "{out}{err}");
    git(&setup.project, &["add", "-A"]);
    git(&setup.project, &["commit", "--quiet", "-m", "mirror"]);
    setup.git(&["push", "--quiet", "origin", "main"]);
    setup.git(&["fetch", "--quiet", "origin"]);
    // A new head whose CI cannot be read: kept, and vaulted.
    let kept = move_head(&setup, 1, "g.py");
    let writes = setup.writes(|| {
        assert_eq!(
            setup.run(),
            "17.0: repin (pr:OCA/pos#1): nothing to propose; vaulted 1 of 1 observed heads on the mirror\n\
             17.0: dashboard issue #1 created\n"
        );
    });
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(git(&mirror, &["ls-remote", "."]).contains(&kept));
    // Nothing to wait for: taken, through a pull request.
    no_checks(&setup, &kept);
    let out = setup.run();
    assert!(
        out.starts_with(&format!(
            "17.0: repin (pr:OCA/pos#1): opened #2 {}/acme/project/pull/2\n",
            setup.api
        )),
        "{out}"
    );
    let pull = setup.issue(2);
    assert_eq!(pull["title"], "chore(deps): take new pull request heads");
    let branch = pull["head"]["ref"].as_str().unwrap().to_owned();
    assert!(branch.starts_with("odx/repin/"), "{branch}");
    let labels: Vec<&str> = pull["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|label| label["name"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["odx:bot", "odx:repin", "bot"]);
    let body = pull["body"].as_str().unwrap();
    assert!(body.starts_with("<!-- odx:pr kind=repin key="), "{body}");
    assert!(
        body.contains("- `pr:OCA/pos#1` <!-- odx:covers pr:OCA/pos#1 -->\n"),
        "{body}"
    );
    let lock = git(&setup.bare, &["show", &format!("{branch}:odx.lock")]);
    assert!(lock.contains(&kept), "{lock}");
    let issue = setup.issue(1)["body"].as_str().unwrap().to_owned();
    assert!(
        issue.contains(&format!(
            "  - drop: in [#2]({}/acme/project/pull/2)\n",
            setup.api
        )),
        "{issue}"
    );
    // The same change again: nothing is written.
    let writes = setup.writes(|| {
        assert_eq!(
            setup.run(),
            format!(
                "17.0: repin (pr:OCA/pos#1): unchanged #2 {}/acme/project/pull/2\n\
                 17.0: dashboard issue #1 unchanged\n",
                setup.api
            )
        );
    });
    assert_eq!(writes, Vec::<String>::new());
    // A newer head: the open pull request is updated on its branch.
    let newer = move_head(&setup, 1, "h.py");
    no_checks(&setup, &newer);
    let out = setup.run();
    assert!(
        out.starts_with(&format!(
            "17.0: repin (pr:OCA/pos#1): updated #2 {}/acme/project/pull/2\n",
            setup.api
        )),
        "{out}"
    );
    let lock = git(&setup.bare, &["show", &format!("{branch}:odx.lock")]);
    assert!(lock.contains(&newer), "{lock}");
    // Closed by a person: not proposed again.
    setup.state.lock().unwrap().item(2).unwrap()["state"] = json!("closed");
    let out = setup.run();
    assert!(
        out.starts_with("17.0: repin (pr:OCA/pos#1): #2 was closed: not proposed again\n"),
        "{out}"
    );
    // Merged upstream: the override is dropped, within the budget.
    setup.route(
        "/api/v3/repos/OCA/pos/pulls/1",
        200,
        &format!(
            r#"{{"number": 1, "title": "[17.0][FIX] pos_x: 1", "state": "closed", "merged_at": "2026-10-01T00:00:00Z", "base": {{"ref": "17.0"}}, "head": {{"sha": "{newer}"}}}}"#
        ),
    );
    let (code, out, err) = setup.odx(&["bot", "run", "--apply", "--max-prs", "0"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with(
            "17.0: drop-override (pr:OCA/pos#1): deferred to the next run (--max-prs)\n"
        ),
        "{out}"
    );
    let issue = setup.issue(1)["body"].as_str().unwrap().to_owned();
    assert!(
        issue.contains("\n## Deferred to the next run (1)\n\n- drop-override (pr:OCA/pos#1)\n"),
        "{issue}"
    );
    let out = setup.run();
    assert!(
        out.starts_with(&format!(
            "17.0: drop-override (pr:OCA/pos#1): opened #3 {}/acme/project/pull/3\n",
            setup.api
        )),
        "{out}"
    );
    let dropped = setup.issue(3);
    assert_eq!(
        dropped["title"],
        "chore(deps): drop overrides merged upstream"
    );
    let branch = dropped["head"]["ref"].as_str().unwrap().to_owned();
    let pyproject = git(&setup.bare, &["show", &format!("{branch}:pyproject.toml")]);
    assert!(!pyproject.contains("pr = 1"), "{pyproject}");
    // A pull request merged into test with the promotion label.
    git(&setup.project, &["checkout", "--quiet", "-b", "feature"]);
    change(
        &setup.project,
        &[("my_addon/__manifest__.py", "{'version': '17.0.1.1.0'}")],
        "[IMP] my_addon",
    );
    let feature = git(&setup.project, &["rev-parse", "HEAD"]);
    git(&setup.project, &["checkout", "--quiet", "main"]);
    git(
        &setup.project,
        &["merge", "--quiet", "--no-ff", "feature", "-m", "Merge #50"],
    );
    setup.git(&["push", "--quiet", "origin", "main"]);
    setup.git(&["fetch", "--quiet", "origin"]);
    git(&setup.bare, &["update-ref", "refs/pull/50/head", &feature]);
    {
        let mut state = setup.state.lock().unwrap();
        let url = format!("{}/acme/project/pull/50", setup.api);
        state.items.push(json!({
            "number": 50, "title": "my_addon", "body": "", "state": "closed",
            "labels": [{"name": "promote"}], "pull_request": {}, "html_url": url,
        }));
        state.fixed.insert(
            "/pulls/50".to_owned(),
            json!({
                "number": 50, "title": "my_addon", "state": "closed",
                "merged_at": "2026-10-20T00:00:00Z", "base": {"ref": "main"},
                "head": {"sha": feature},
            })
            .to_string(),
        );
        state.fixed.insert(
            "/pulls/50/commits".to_owned(),
            json!([{ "sha": feature }]).to_string(),
        );
        drop(state);
    }
    let (code, out, err) = setup.odx(&["bot", "plan"]);
    assert_eq!(code, 0, "{err}");
    let plan: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        plan["promotions"][0]["result"]["outcome"], "planned",
        "{plan:#}"
    );
    let out = setup.run();
    assert!(
        out.starts_with(&format!(
            "promote #50 (test -> prod): opened #5 {}/acme/project/pull/5\n",
            setup.api
        )),
        "{out}"
    );
    let promotion = setup.issue(5);
    assert_eq!(promotion["title"], "chore: promote #50 from test to prod");
    assert_eq!(promotion["base"]["ref"], "prod");
    assert_eq!(promotion["head"]["ref"], "odx/promote/pr-50-prod");
    let promoted = git(
        &setup.bare,
        &["show", "odx/promote/pr-50-prod:my_addon/__manifest__.py"],
    );
    assert!(promoted.contains("17.0.1.1.0"), "{promoted}");
    // Proposed once.
    let out = setup.run();
    assert!(!out.contains("promote #50"), "{out}");
    // Once prod has it, another labelled pull request of the same change
    // writes nothing, and pushes nothing (an empty commit would delete the
    // branch).
    git(
        &setup.bare,
        &["update-ref", "refs/heads/prod", "refs/heads/main"],
    );
    git(&setup.bare, &["update-ref", "refs/pull/51/head", &feature]);
    setup.git(&["fetch", "--quiet", "origin"]);
    {
        let mut state = setup.state.lock().unwrap();
        let url = format!("{}/acme/project/pull/51", setup.api);
        state.items.push(json!({
            "number": 51, "title": "my_addon again", "body": "", "state": "closed",
            "labels": [{"name": "promote"}], "pull_request": {}, "html_url": url,
        }));
        for (path, value) in [
            (
                "/pulls/51",
                json!({
                    "number": 51, "title": "my_addon again", "state": "closed",
                    "merged_at": "2026-10-21T00:00:00Z", "base": {"ref": "main"},
                    "head": {"sha": feature},
                }),
            ),
            ("/pulls/51/commits", json!([{ "sha": feature }])),
        ] {
            state.fixed.insert(path.to_owned(), value.to_string());
        }
        drop(state);
    }
    let out = setup.run();
    assert!(
        out.starts_with("promote #51 (test -> prod): nothing to promote: prod has it already\n"),
        "{out}"
    );
    let branches = git(&setup.bare, &["branch", "--list", "odx/promote/*"]);
    assert!(!branches.contains("pr-51-prod"), "{branches}");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one milestone after another, as runs see them"
)]
fn a_wanted_addon_whose_migration_starts_is_announced_once() {
    let setup = Setup::new(false);
    let pyproject = setup.project.join("pyproject.toml");
    let text = std::fs::read_to_string(&pyproject).unwrap().replace(
        "labels = [\"bot\"]\n",
        "labels = [\"bot\"]\nnotify = [\"ann\"]\n",
    );
    std::fs::write(
        &pyproject,
        format!(
            "{text}\n[[tool.odx.wanted]]\naddon = \"pos_y\"\nrepository = \"{}/OCA/pos\"\nnote = \"customer asks in Q3\"\n",
            setup.up_api
        ),
    )
    .unwrap();
    git(&setup.project, &["add", "-A"]);
    git(&setup.project, &["commit", "--quiet", "-m", "wanted"]);
    setup.git(&["push", "--quiet", "origin", "main"]);
    setup.git(&["fetch", "--quiet", "origin"]);
    setup.route(
        "/api/v3/repos/OCA/pos/pulls",
        200,
        r#"[{"number": 7, "title": "[17.0][MIG] pos_y: Migration to 17.0", "state": "open", "base": {"ref": "17.0"},
             "user": {"login": "joe"}, "created_at": "2026-03-04T05:06:07Z"},
           {"number": 5, "title": "[17.0][FIX] pos_x: 5", "state": "open", "base": {"ref": "17.0"}},
           {"number": 1, "title": "[17.0][FIX] pos_x: 1", "state": "open", "base": {"ref": "17.0"}}]"#,
    );
    let bot = |args: &[&str]| {
        let out = setup
            .command(env!("CARGO_BIN_EXE_odx"))
            .env("ODX_PYPI_URL", format!("{}/pypi", setup.up_api))
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    // Without --apply, it says what it would post, on the issue to be.
    let dry = bot(&["bot", "dashboard"]);
    assert!(
        dry.contains("17.0: dashboard issue would be created\n")
            && dry.ends_with("migrating OCA/pos#7 would comment on the new issue\n"),
        "{dry}"
    );
    let writes = setup.writes(|| {
        let out = bot(&["bot", "dashboard", "--apply"]);
        assert!(
            out.ends_with(
                "17.0: dashboard issue #1 created\nwant:pos_y@17.0: migrating OCA/pos#7 commented on #1\n"
            ),
            "{out}"
        );
    });
    let comments = writes
        .iter()
        .filter(|line| line.contains("/issues/1/comments"))
        .count();
    assert_eq!(comments, 1, "{writes:?}");
    let body = setup.issue(1)["comments"][0]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        body.starts_with("@ann Someone started migrating the wanted addon pos_y on 17.0: OCA/pos#7 by joe, opened 2026-03-04. `odx dashboard act want:pos_y@17.0 --include` adds it to the project."),
        "{body}"
    );
    assert!(
        setup.issue(1)["body"]
            .as_str()
            .unwrap()
            .contains("Wanted addons")
    );
    // The next run finds its comment and says nothing more.
    let writes = setup.writes(|| {
        let out = bot(&["--json", "bot", "dashboard", "--apply"]);
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["dashboards"][0]["wanted"], json!([]), "{value:#}");
    });
    assert!(
        writes.iter().all(|line| !line.contains("/comments")),
        "{writes:?}"
    );
    // Another migration pull request is another milestone; without
    // --apply, it is only planned.
    setup.route(
        "/api/v3/repos/OCA/pos/pulls",
        200,
        r#"[{"number": 8, "title": "[17.0][MIG] pos_y", "state": "open", "base": {"ref": "17.0"},
             "user": {"login": "kim"}, "created_at": "2026-04-01T00:00:00Z"}]"#,
    );
    let out = bot(&["--fresh", "bot", "dashboard"]);
    assert!(
        out.ends_with("want:pos_y@17.0: migrating OCA/pos#8 would comment on #1\n"),
        "{out}"
    );
    // A forge that fails to list or take comments ends the run.
    let comments = "/api/v3/repos/acme/project/issues/1/comments";
    for method in ["GET", "POST"] {
        setup.state.lock().unwrap().failing =
            vec![(method.to_owned(), comments.to_owned(), String::new())];
        let out = setup
            .command(env!("CARGO_BIN_EXE_odx"))
            .env("ODX_PYPI_URL", format!("{}/pypi", setup.up_api))
            .args(["bot", "dashboard", "--apply"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{method}");
    }
}
