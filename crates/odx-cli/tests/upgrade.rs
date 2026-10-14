//! `odx upgrade` against a local server playing the package index,
//! OpenUpgrade's raw files and a forge API.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

type Routes = Arc<Mutex<HashMap<String, (u16, String)>>>;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "odx")
        .env("GIT_AUTHOR_EMAIL", "odx@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .unwrap();
    assert!(out.status.success());
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// A server on localhost answering from `routes` (exact paths with their
/// query strings, else without them), 404 otherwise.
fn serve(routes: &Routes) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes = Arc::clone(routes);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend(buffer.iter().take(n)),
                }
            }
            let text = String::from_utf8_lossy(&request);
            let target = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let path = target.split('?').next().unwrap_or(&target).to_owned();
            let (status, body) = {
                let routes = routes.lock().unwrap();
                routes
                    .get(&target)
                    .or_else(|| routes.get(&path))
                    .cloned()
                    .unwrap_or((404, String::new()))
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

const UV_LOCK: &str = r#"version = 1

[[package]]
name = "p"
version = "1"
source = { virtual = "." }

[[package]]
name = "odoo-addon-pos-x"
version = "17.0.1.0.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "odoo-addon-web_y"
version = "17.0.1.0.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "odoo-addon-old-z"
version = "17.0.1.0.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "odoo-addon-sale-q"
version = "17.0.1.0.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "odoo16-addon-foo"
version = "16.0.1.0.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "odoo-addon-far"
version = "17.0.1.0.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "requests"
version = "2"
source = { registry = "https://pypi.org/simple" }
"#;

struct Setup {
    tmp: tempfile::TempDir,
    root: PathBuf,
    api: String,
    routes: Routes,
    env: MapEnv,
}

impl Setup {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let routes: Routes = Arc::default();
        let api = serve(&routes);
        // An aggregated repository with addon pos_agg.
        let up = tmp.path().join("up");
        write(&up.join("pos_agg/__manifest__.py"), "{}");
        write(&up.join("other/__manifest__.py"), "{}");
        git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "base"]);
        let root = tmp.path().join("project");
        write(
            &root.join("pyproject.toml"),
            &format!(
                "[tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.groups]\nproject_addons = [\"addons\"]\n\n[[tool.odx.repositories]]\nname = \"agg\"\nurl = \"{}\"\naddons = [\"pos_*\"]\nmerges = []\n",
                file_url(&up)
            ),
        );
        write(&root.join("addons/my_mod/__manifest__.py"), "{}");
        write(&root.join("uv.lock"), UV_LOCK);
        let env = MapEnv::default()
            .with(
                "ODX_CACHE_DIR",
                tmp.path().join("cache").display().to_string(),
            )
            .with(
                "ODX_CONFIG_DIR",
                tmp.path().join("config").display().to_string(),
            )
            .with("ODX_PYPI_URL", format!("{api}/pypi"))
            .with("ODX_OPENUPGRADE_URL", format!("{api}/ou/"))
            .with("ODX_ODL", "odx-test-no-such-program");
        let setup = Self {
            tmp,
            root,
            api,
            routes,
            env,
        };
        let releases = [
            ("pos-x", "17.0.1.0.0", "OCA/pos"),
            ("pos-x", "18.0.1.0.0", "OCA/pos"),
            ("web-y", "17.0.1.0.0", "OCA/web"),
            ("web-y", "18.0.1.0.0", "OCA/web-extra"),
            ("old-z", "17.0.1.0.0", "OCA/z"),
            ("new-z", "18.0.1.0.0", "OCA/z"),
            ("sale-q", "17.0.1.0.0", "OCA/sale"),
        ];
        // An addon whose repository is on no reachable forge.
        setup.route(
            "/pypi/odoo-addon-far/json",
            200,
            r#"{"releases":{"17.0.1.0.0":[]}}"#,
        );
        setup.route(
            "/pypi/odoo-addon-far/17.0.1.0.0/json",
            200,
            r#"{"info":{"home_page":"http://127.0.0.1:1/OCA/far"}}"#,
        );
        let mut versions: HashMap<&str, Vec<&str>> = HashMap::new();
        for (dist, version, repo) in releases {
            versions.entry(dist).or_default().push(version);
            setup.route(
                &format!("/pypi/odoo-addon-{dist}/{version}/json"),
                200,
                &format!(r#"{{"info":{{"home_page":"{}/{repo}"}}}}"#, setup.api),
            );
        }
        for (dist, versions) in versions {
            let releases: Vec<String> = versions.iter().map(|v| format!("\"{v}\":[]")).collect();
            setup.route(
                &format!("/pypi/odoo-addon-{dist}/json"),
                200,
                &format!(r#"{{"releases":{{{}}}}}"#, releases.join(",")),
            );
        }
        setup.route(
            "/ou/18.0/openupgrade_scripts/apriori.py",
            200,
            "renamed_modules = {\n    \"old_z\": \"new_z\",\n}\n",
        );
        setup.route("/api/v3/meta", 200, "{}");
        setup.route(
            "/api/v3/repos/OCA/sale/pulls",
            200,
            r#"[{"number":12,"title":"[18.0][MIG] sale_q: Migration to 18.0","state":"open","base":{"ref":"18.0"}},
               {"number":13,"title":"[18.0][IMP] sale_q","state":"open","base":{"ref":"18.0"}}]"#,
        );
        setup
    }

    fn route(&self, path: &str, status: u16, body: &str) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, body.to_owned()));
    }

    fn odx(&self, env: &MapEnv, args: &[&str]) -> (Exit, String, String) {
        let mut command_line = vec!["odx"];
        command_line.extend_from_slice(args);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = odx_cli::run_with_env(command_line, &OsFs, env, &self.root, &mut out, &mut err);
        (
            exit,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }
}

#[test]
fn addons_on_the_target_series() {
    let setup = Setup::new();
    let env = &setup.env;
    let (exit, _, err) = setup.odx(env, &["aggregate"]);
    assert_eq!(exit, Exit::Success, "{err}");
    let (exit, out, err) = setup.odx(env, &["upgrade", "--target", "18.0"]);
    assert_eq!(exit, Exit::Success, "{err}");
    let api = &setup.api;
    let agg = file_url(&setup.tmp.path().join("up"));
    assert_eq!(
        out,
        format!(
            "upgrade 17.0 -> 18.0\n\
             my_mod: own\n\
             pos_agg: missing in {agg}\n\
             pos_x: released in {api}/OCA/pos\n\
             web_y: released in {api}/OCA/web-extra, moved from {api}/OCA/web\n\
             old_z: released as new_z in {api}/OCA/z\n\
             sale_q: migration PR #12 in {api}/OCA/sale\n\
             foo: missing\n\
             far: missing in http://127.0.0.1:1/OCA/far\n\
             8 addons: 3 released, 0 available, 1 with a migration PR, 3 missing, 1 own\n"
        )
    );
    assert!(
        err.contains("warning: odoo-lint: odx-test-no-such-program"),
        "{err}"
    );

    let (_, out, _) = setup.odx(
        env,
        &["upgrade", "--target", "18.0", "--only-missing", "--no-lint"],
    );
    assert!(out.contains("\npos_agg: missing"), "{out}");
    assert!(!out.contains("pos_x"), "{out}");
    let (_, out, _) = setup.odx(env, &["--json", "upgrade", "--target", "18.0", "--no-lint"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["addons"][4]["renamed_to"], "new_z");
    assert_eq!(value["addons"][5]["pull_request"], 12);

    // Offline, the cached index answers; OpenUpgrade's renames are not
    // asked for, the configured ones apply.
    let (_, out, err) = setup.odx(
        env,
        &["--offline", "upgrade", "--target", "18.0", "--no-lint"],
    );
    assert!(out.contains("pos_x: released in"), "{out}");
    assert!(out.contains("old_z: missing"), "{out}");
    assert_eq!(err, "");
}

#[test]
fn target_lint_and_source_failures() {
    let setup = Setup::new();
    let env = &setup.env;
    let (exit, _, err) = setup.odx(env, &["upgrade"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("no target series"), "{err}");
    let (exit, _, _) = setup.odx(env, &["-C", "..", "upgrade", "--target", "18.0"]);
    assert_eq!(exit, Exit::Usage);
    // The target from the configuration, renames from it too; an index
    // and OpenUpgrade that fail are reported.
    let pyproject = std::fs::read_to_string(setup.root.join("pyproject.toml")).unwrap();
    std::fs::write(
        setup.root.join("pyproject.toml"),
        pyproject.replacen(
            "[tool.odx.groups]",
            "[tool.odx.upgrade]\ntarget_odoo_version = \"18.0\"\n\n[tool.odx.index]\nttl = \"0s\"\n\n[tool.odx.index.renames]\n\"18.0\" = { foo = \"bar\" }\n\n[tool.odx.index.overrides]\nbar = { \"18.0\" = \"OCA/bar\" }\n\n[tool.odx.groups]",
            1,
        ),
    )
    .unwrap();
    setup.route("/ou/18.0/openupgrade_scripts/apriori.py", 500, "");
    setup.route("/pypi/odoo-addon-sale-q/json", 500, "");
    let (exit, out, err) = setup.odx(env, &["upgrade", "--no-lint"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        out.contains(
            "foo: available as bar in https://github.com/OCA/bar
"
        ),
        "{out}"
    );
    assert!(err.contains("warning: openupgrade: "), "{err}");
    assert!(err.contains("warning: index: sale_q on 17.0: "), "{err}");
    // A project without own addons skips odoo-lint.
    std::fs::remove_dir_all(setup.root.join("addons")).unwrap();
    let (_, out, err) = setup.odx(env, &["upgrade"]);
    assert!(!out.contains("my_mod"), "{out}");
    assert!(!err.contains("odoo-lint"), "{err}");
}

#[cfg(unix)]
#[test]
fn odoo_lint_counts_the_work_on_own_addons() {
    use std::os::unix::fs::PermissionsExt as _;

    let setup = Setup::new();
    let odl = setup.tmp.path().join("odl");
    let script = |answer: &str| {
        write(
            &odl,
            &format!("#!/bin/sh\necho 'note' >&2\ncat <<'EOF'\n{answer}\nEOF\n"),
        );
        std::fs::set_permissions(&odl, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    script(
        r#"{"target":"18.0","modules":[{"name":"my_mod","path":"x","changes":3,"automatic":2,"review":1,"manual":0}]}"#,
    );
    let env = setup.env.clone().with("ODX_ODL", odl.display().to_string());
    let (_, out, err) = setup.odx(&env, &["upgrade", "--target", "18.0"]);
    assert!(
        out.contains("my_mod: own, 3 changes (2 automatic, 1 review, 0 manual)\n"),
        "{out}{err}"
    );
    script("not json");
    let (_, _, err) = setup.odx(&env, &["upgrade", "--target", "18.0"]);
    assert!(err.contains("upgrade-check: expected"), "{err}");
    assert!(err.contains(": note"), "{err}");
}

#[test]
fn organization_listings_find_what_the_package_index_does_not() {
    let setup = Setup::new();
    let routes: Routes = Arc::default();
    let forge = serve(&routes);
    for (path, body) in [
        ("/api/v1/version", "{}".to_owned()),
        (
            "/api/v1/orgs/acme/repos",
            format!(r#"[{{"full_name":"acme/tools","html_url":"{forge}/acme/tools"}}]"#),
        ),
        (
            "/api/v1/repos/acme/tools/git/trees/18.0",
            r#"{"tree":[{"path":"foo/__manifest__.py"}]}"#.to_owned(),
        ),
    ] {
        routes.lock().unwrap().insert(path.to_owned(), (200, body));
    }
    let pyproject = std::fs::read_to_string(setup.root.join("pyproject.toml")).unwrap();
    std::fs::write(
        setup.root.join("pyproject.toml"),
        pyproject.replacen(
            "[tool.odx.groups]",
            &format!(
                "[tool.odx.index]\norgs = [\"{forge}/acme\", \"http://127.0.0.1:1/dead\"]\n\n[tool.odx.groups]"
            ),
            1,
        ),
    )
    .unwrap();
    let env = &setup.env;
    let (exit, _, err) = setup.odx(env, &["aggregate"]);
    assert_eq!(exit, Exit::Success, "{err}");
    let (exit, out, err) = setup.odx(env, &["upgrade", "--target", "18.0", "--no-lint"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        out.contains(&format!("foo: available in {forge}/acme/tools\n")),
        "{out}"
    );
    assert!(
        err.contains("warning: index: listing 127.0.0.1/dead on 18.0: "),
        "{err}"
    );
}

#[test]
fn migration_plans_put_dependencies_first() {
    let setup = Setup::new();
    let up = setup.tmp.path().join("up");
    write(
        &up.join("pos_dep/__manifest__.py"),
        "{'depends': ['base', 'pos_zz', 'gone_dep']}",
    );
    write(&up.join("pos_zz/__manifest__.py"), "{}");
    write(
        &setup.root.join("addons/my_mod/__manifest__.py"),
        "{'depends': ['pos_x', 'gone_dep']}",
    );
    write(&setup.root.join("addons/my_other/__manifest__.py"), "{}");
    git(&up, &["add", "-A"]);
    git(&up, &["commit", "--quiet", "-m", "more"]);
    let env = &setup.env;
    let (exit, _, err) = setup.odx(env, &["aggregate"]);
    assert_eq!(exit, Exit::Success, "{err}");
    let (exit, out, err) = setup.odx(env, &["migrate", "plan", "--to", "18.0"]);
    assert_eq!(exit, Exit::Success, "{err}");
    let api = &setup.api;
    let agg = file_url(&up);
    assert_eq!(
        out,
        format!(
            "migration plan 17.0 -> 18.0\n\
             available on 18.0 (3): pos_x, web_y, old_z\n\
             open migration pull request: sale_q #12 in {api}/OCA/sale; review, test and contribute there\n\
             to migrate, dependencies first (5):\n  \
             pos_agg (in {agg})\n  pos_zz (in {agg})\n  pos_dep (in {agg})\n    \
             waits on gone_dep: not on 18.0, and no open migration pull request found\n  \
             foo\n  far (in http://127.0.0.1:1/OCA/far)\n\
             own addons, migrated in place (2): my_mod, my_other\n  my_mod\n    \
             waits on gone_dep: not on 18.0, and no open migration pull request found\n\
             next: in a clone of each addon's repository, `odx migrate addon <addon> --to 18.0`\n"
        )
    );
    let (_, out, _) = setup.odx(env, &["--json", "migrate", "plan", "--to", "18.0"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["to_migrate"][2]["addon"], "pos_dep");
    assert_eq!(value["pull_requests"][0]["number"], 12);
    // The target from the configuration, or none.
    let (exit, _, err) = setup.odx(env, &["migrate", "plan"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("no target series"), "{err}");
}

#[test]
fn own_addons_migrate_in_place() {
    let setup = Setup::new();
    let root = &setup.root;
    write(
        &root.join("addons/my_mod/migrations/17.0.1.0.1/pre.py"),
        "\n",
    );
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.name", "Me"]);
    git(root, &["config", "user.email", "me@example.com"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "--quiet", "-m", "project"]);
    let env = &setup.env;
    let (exit, out, err) = setup.odx(env, &["migrate", "own", "--to", "18.0"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert_eq!(
        out,
        "own addons migrated in place to 18.0:\n  addons/my_mod: [MIG] committed, 0 finding(s) left to fix by hand\n"
    );
    assert!(
        err.contains("warning: migrate: odx-test-no-such-program"),
        "{err}"
    );
    let (_, out, _) = setup.odx(
        env,
        &[
            "--json",
            "migrate",
            "own",
            "--to",
            "18.0",
            "--no-pre-commit",
        ],
    );
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["addons"][0]["committed"], false);
    // A dirty worktree, and no target series.
    write(&root.join("addons/my_mod/x.py"), "x\n");
    git(root, &["add", "-A"]);
    let (exit, _, err) = setup.odx(env, &["migrate", "own", "--to", "18.0"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("uncommitted changes"), "{err}");
    let (exit, _, _) = setup.odx(env, &["migrate", "own"]);
    assert_eq!(exit, Exit::Usage);
}

#[cfg(unix)]
#[test]
fn own_addons_list_what_is_left() {
    use std::os::unix::fs::PermissionsExt as _;
    let setup = Setup::new();
    let root = &setup.root;
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.name", "Me"]);
    git(root, &["config", "user.email", "me@example.com"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "--quiet", "-m", "project"]);
    let odl = setup.tmp.path().join("odl");
    std::fs::write(
        &odl,
        "#!/bin/sh\ncase \"$*\" in\n  *--fix*) exit 1;;\n  *) echo '{\"modules\":[{\"violations\":[{\"file_path\":\"addons/my_mod/x.py\",\"line\":2,\"code\":\"U1\",\"message\":\"m\"}]}]}';;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&odl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = setup.env.clone().with("ODX_ODL", odl.display().to_string());
    let (exit, out, err) = setup.odx(&env, &["migrate", "own", "--to", "18.0"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        out.ends_with(
            "  addons/my_mod: nothing changed, 1 finding(s) left to fix by hand\n    addons/my_mod/x.py:2: U1 m\n"
        ),
        "{out}"
    );
}
