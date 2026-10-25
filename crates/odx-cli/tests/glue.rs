//! `odx glue` on a project that installs an addon package, with glue in a
//! local clone, in the cache and in Odoo itself.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::{Path, PathBuf};
use std::process::Command;

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "odx")
        .env("GIT_AUTHOR_EMAIL", "odx@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// A repository on branch 17.0 with `addons` (folder, manifest), committed.
fn repository(dir: &Path, addons: &[(&str, &str)]) -> String {
    for (addon, manifest) in addons {
        write(&dir.join(addon).join("__manifest__.py"), manifest);
    }
    git(dir, &["init", "--quiet", "--initial-branch=17.0"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", "addons"]);
    git(dir, &["rev-parse", "HEAD"])
}

/// `url`'s repository `dir` as the cache has it.
fn cache(cache: &Path, url: &str, dir: &Path) {
    let repo = cache.join("repos").join(odx_git::url_hash(url));
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--bare"]);
    git(
        &repo,
        &[
            "fetch",
            "--quiet",
            dir.to_str().unwrap(),
            "+refs/heads/*:refs/odx/heads/*",
        ],
    );
    write(&repo.with_extension("url"), &format!("{url}\n"));
}

/// Odoo, cached as OCB so that it is found without a lockfile too, with
/// `sale` depending on `sales_team`; glue between `pos_x` and `sales_team` in
/// another cached repository; things in the cache that are no repository;
/// and `web_url` again, spelled differently. The commit of Odoo.
fn odoo_and_more(tmp: &Path, cache_dir: &Path, web_url: &str) -> String {
    let odoo = tmp.join("up/odoo");
    let rev = repository(
        &odoo,
        &[
            ("addons/sale", "{'depends': ['sales_team']}"),
            ("odoo/addons/base", "{'depends': []}"),
        ],
    );
    cache(cache_dir, ODOO, &odoo);
    let team = tmp.join("up/team");
    repository(
        &team,
        &[("pos_x_team", &glue("'pos_x', 'sales_team'", "True"))],
    );
    cache(cache_dir, &file_url(&team), &team);
    // Things in the cache that are no repository.
    write(&cache_dir.join("repos/stray.fetched"), "1");
    std::fs::create_dir_all(cache_dir.join("repos/no-url")).unwrap();
    // The same repository twice, spelled differently.
    std::fs::create_dir_all(cache_dir.join("repos/again")).unwrap();
    write(
        &cache_dir.join("repos/again.url"),
        &format!("{web_url}.git\n"),
    );
    rev
}

/// A manifest of glue with `depends` and `auto_install`.
fn glue(depends: &str, auto: &str) -> String {
    format!("{{'depends': [{depends}], 'auto_install': {auto}}}")
}

const ODOO: &str = "https://github.com/OCA/OCB";

struct Project {
    tmp: tempfile::TempDir,
    root: PathBuf,
    env: MapEnv,
    pos: String,
}

impl Project {
    fn new(extra: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        // A clone of pos: glue with sale (that also needs pos_extra),
        // glue with an addon the project lacks, ignored and uninstallable
        // glue, and a broken manifest.
        let pos = tmp.path().join("up/pos");
        repository(
            &pos,
            &[
                ("pos_x", "{'depends': ['point_of_sale']}"),
                (
                    "pos_x_sale",
                    &glue("'pos_x', 'sale', 'pos_extra'", "['pos_x', 'sale']"),
                ),
                ("pos_x_web", &glue("'pos_x', 'web_q'", "True")),
                ("pos_quiet", &glue("'pos_x'", "True")),
                (
                    "pos_old",
                    "{'depends': ['pos_x'], 'auto_install': True, 'installable': False}",
                ),
                ("pos_broken", "{"),
            ],
        );
        std::fs::create_dir_all(tmp.path().join("clones")).unwrap();
        git(
            &tmp.path().join("clones"),
            &["clone", "--quiet", &file_url(&pos), "pos"],
        );
        // Glue in another repository, which the cache has; a clone of it
        // is there too.
        let web = tmp.path().join("up/web");
        repository(
            &web,
            &[("web_pos_x", &glue("'pos_x', 'point_of_sale'", "True"))],
        );
        let web_url = file_url(&web);
        let cache_dir = tmp.path().join("cache");
        cache(&cache_dir, &web_url, &web);
        // A clone with its upstream as a second remote, and one with a
        // fork listed before `origin`.
        let clones = tmp.path().join("clones");
        git(&clones, &["clone", "--quiet", &web_url, "web"]);
        git(&clones.join("web"), &["remote", "rename", "origin", "fork"]);
        git(&clones.join("web"), &["remote", "add", "origin", &web_url]);
        git(
            &clones.join("pos"),
            &["remote", "add", "fork", "https://example.com/me/pos.git"],
        );
        let rev = odoo_and_more(tmp.path(), &cache_dir, &web_url);

        let root = tmp.path().join("project");
        write(
            &root.join("pyproject.toml"),
            &format!(
                "[project]\nname = \"p\"\nversion = \"1\"\ndependencies = [\"odoo-addon-pos-x\"]\n\n\
                 [tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.layout]\nproject_addons = [\"addons\"]\n{extra}"
            ),
        );
        write(
            &root.join("addons/my_shop/__manifest__.py"),
            "{'depends': ['sale', 'pos_x']}",
        );
        write(
            &root.join("odx.lock"),
            &format!("version = 1\n\n[odoo]\nurl = \"{ODOO}\"\nrev = \"{rev}\"\n"),
        );
        write(
            &tmp.path().join("config/config.toml"),
            &format!(
                "[clones]\ndirs = [\"{}\"]\n",
                tmp.path()
                    .join("clones")
                    .display()
                    .to_string()
                    .replace('\\', "/")
            ),
        );
        let env = MapEnv::default()
            .with("ODX_CACHE_DIR", cache_dir.display().to_string())
            .with(
                "ODX_CONFIG_DIR",
                tmp.path().join("config").display().to_string(),
            );
        Self {
            pos: file_url(&pos),
            tmp,
            root,
            env,
        }
    }

    fn odx(&self, args: &[&str]) -> (Exit, String, String) {
        let mut command_line = vec!["odx"];
        command_line.extend_from_slice(args);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = odx_cli::run_with_env(
            command_line,
            &OsFs,
            &self.env,
            &self.root,
            &mut out,
            &mut err,
        );
        (
            exit,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }
}

#[test]
fn glue_with_what_the_project_installs_is_found() {
    let project = Project::new("\n[tool.odx.glue]\nignore = { pos_quiet = \"not used here\" }\n");
    let (exit, out, err) = project.odx(&["glue"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    assert!(
        out.starts_with("glue addons Odoo would install with what the project has (17.0):\n"),
        "{out}"
    );
    let pos = &project.pos;
    assert!(
        out.contains(&format!(
            "  pos_x_sale ({pos}, with pos_x, sale)\n    \
             odx add {pos}@17.0 --package odoo-addon-pos-x-sale --subdirectory pos_x_sale\n    \
             also needs: pos_extra\n"
        )),
        "{out}"
    );
    assert!(out.contains("  pos_x_team ("), "{out}");
    assert!(out.contains("  web_pos_x ("), "{out}");
    assert!(
        out.ends_with("ignored: pos_quiet (not used here)\n"),
        "{out}"
    );
    for absent in ["pos_x_web", "pos_old", "pos_broken"] {
        assert!(!out.contains(absent), "{absent}: {out}");
    }

    // odx lock says so too.
    let (exit, out, err) = project.odx(&["lock", "--no-push"]);
    assert_eq!(exit, Exit::Success, "{out}{err}");
    assert!(
        err.contains("warning: glue: Odoo would install pos_x_team, web_pos_x, pos_x_sale with what the project has"),
        "{err}"
    );

    let (exit, out, err) = project.odx(&["glue", "--fail"]);
    assert_eq!(exit, Exit::Failure, "{out}");
    assert_eq!(err, "error: 3 glue addons missing\n");
    let (_, out, _) = project.odx(&["--json", "glue"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["scanned"], 4);
    assert_eq!(value["ignored"][0]["reason"], "not used here");
    let sale = value["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["addon"] == "pos_x_sale")
        .unwrap();
    assert_eq!(sale["missing"][0], "pos_extra");
    assert_eq!(sale["triggers"], serde_json::json!(["pos_x", "sale"]));
}

#[test]
fn nothing_missing_and_no_project() {
    let project = Project::new(
        "\n[tool.odx.glue]\nignore = { pos_quiet = \"-\", pos_x_sale = \"-\", pos_x_team = \"-\", web_pos_x = \"-\" }\n",
    );
    // Without a lockfile, Odoo's addons come from the cached OCB.
    std::fs::remove_file(project.root.join("odx.lock")).unwrap();
    let (exit, out, _) = project.odx(&["glue", "--fail"]);
    assert_eq!(
        (exit, out.lines().next()),
        (
            Exit::Success,
            Some("no glue addons missing (4 repositories with 17.0 scanned)")
        )
    );
    let elsewhere = project.tmp.path().display().to_string();
    let (exit, _, err) = project.odx(&["-C", &elsewhere, "glue"]);
    assert_eq!(exit, Exit::Usage, "{err}");
}
