//! `odx verify` for Odoo.sh projects with submodules.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::Path;
use std::process::Command;

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "protocol.file.allow=always"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "odx")
        .env("GIT_AUTHOR_EMAIL", "odx@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn odx(root: &Path, env: &MapEnv, args: &[&str]) -> (Exit, String, String) {
    let mut command_line = vec!["odx"];
    command_line.extend_from_slice(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = odx_cli::run_with_env(command_line, &OsFs, env, root, &mut out, &mut err);
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn submodules_must_pin_the_locked_result() {
    let tmp = tempfile::tempdir().unwrap();
    let up = tmp.path().join("up");
    std::fs::create_dir_all(&up).unwrap();
    git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
    std::fs::write(up.join("a.txt"), "a").unwrap();
    git(&up, &["add", "-A"]);
    git(&up, &["commit", "--quiet", "-m", "base"]);
    let root = tmp.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet"]);
    let url = up.display().to_string().replace('\\', "/");
    std::fs::write(
        root.join("pyproject.toml"),
        format!(
            "[tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.platform]\nprofile = \"odoo-sh\"\n\n\
             [[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{url}\"\ntarget_dir = \"vendor/pos\"\nmerges = []\n"
        ),
    )
    .unwrap();
    let env = MapEnv::default().with(
        "ODX_CACHE_DIR",
        tmp.path().join("cache").display().to_string(),
    );
    let (exit, out, err) = odx(&root, &env, &["lock"]);
    assert_eq!(exit, Exit::Success, "{out}{err}");
    git(&root, &["submodule", "add", "--quiet", &url, "vendor/pos"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "--quiet", "-m", "pin"]);
    let (exit, out, err) = odx(&root, &env, &["verify"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");

    // The submodule moves to a commit that is not the locked result.
    std::fs::write(up.join("b.txt"), "b").unwrap();
    git(&up, &["add", "-A"]);
    git(&up, &["commit", "--quiet", "-m", "newer"]);
    let pos = root.join("vendor/pos");
    git(&pos, &["pull", "--quiet", "origin", "17.0"]);
    git(&root, &["commit", "--quiet", "-am", "bump"]);
    let (exit, _, err) = odx(&root, &env, &["verify"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("pos: submodule vendor/pos pins "), "{err}");
    assert!(err.contains("which is not the locked result"), "{err}");

    // A submodule that .gitmodules names but nothing pins.
    git(
        &root,
        &[
            "config",
            "-f",
            ".gitmodules",
            "submodule.ghost.path",
            "vendor/ghost",
        ],
    );
    git(
        &root,
        &["config", "-f", ".gitmodules", "submodule.ghost.url", &url],
    );
    git(&root, &["commit", "--quiet", "-am", "ghost"]);
    let (_, _, err) = odx(&root, &env, &["verify"]);
    assert!(
        err.contains("vendor/ghost: submodule in .gitmodules without a committed commit"),
        "{err}"
    );

    // Odoo.sh installs requirements from public PyPI only.
    let (exit, out, err) = odx(&root, &env, &["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Success, "{out}{err}");
    let text = std::fs::read_to_string(root.join("requirements.txt")).unwrap();
    std::fs::write(
        root.join("requirements.txt"),
        format!("{text}private @ git+https://x/private.git\n-e ./local\nrequests==2.31.0  # git+ in a comment is fine\n"),
    )
    .unwrap();
    let (_, _, err) = odx(&root, &env, &["verify", "--requirements"]);
    assert!(
        err.contains(
            "requirements.txt: `private @ git+https://x/private.git` is not a public PyPI package"
        ),
        "{err}"
    );
    assert!(
        err.contains("`-e ./local` is not a public PyPI package"),
        "{err}"
    );
    assert!(!err.contains("requests==2.31.0  # git+"), "{err}");
}
