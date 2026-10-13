//! `odx auth` with a file store, through the binary (tokens come on stdin).

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::Path;

use assert_cmd::Command;

fn odx(dir: &Path, secrets: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let output = Command::cargo_bin("odx")
        .unwrap()
        .current_dir(dir)
        .env("ODX_SECRETS_FILE", secrets)
        .env("ODX_CACHE_DIR", dir.join("cache"))
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env_remove("GITLAB_TOKEN")
        .env_remove("GITEA_TOKEN")
        .env_remove("FORGEJO_TOKEN")
        .env("PATH", "")
        .args(args)
        .write_stdin(stdin)
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn login_status_logout() {
    let tmp = tempfile::tempdir().unwrap();
    let secrets = tmp.path().join("secrets.json");
    std::fs::write(
        tmp.path().join("pyproject.toml"),
        "[tool.odx]\nodoo_version = \"17.0\"\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"https://gitlab.example/oca/pos.git\"\nmerges = []\n\n\
         [tool.odx.mirror]\nurl = \"https://forge.example/odoo/vault.git\"\nref_style = \"tags\"\n\n\
         [tool.odx.packages]\nodoo-addon-x = { url = \"https://forge.example/odoo/x.git\", branch = \"17.0\" }\n",
    )
    .unwrap();
    let (code, out, _) = odx(
        tmp.path(),
        &secrets,
        &["auth", "login", "forge.example"],
        "  s3cret\n",
    );
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "stored the token for forge.example in {}\n",
            secrets.display()
        )
    );
    let (_, out, _) = odx(tmp.path(), &secrets, &["auth", "status"], "");
    assert_eq!(
        out,
        format!(
            "forge.example: token from {}\ngitlab.example: no token\n",
            secrets.display()
        )
    );
    let stored = std::fs::read_to_string(&secrets).unwrap();
    assert!(stored.contains("\"forge.example\": \"s3cret\""), "{stored}");
    let (_, out, _) = odx(
        tmp.path(),
        &secrets,
        &["auth", "logout", "forge.example"],
        "",
    );
    assert_eq!(
        out,
        format!(
            "removed the token for forge.example from {}\n",
            secrets.display()
        )
    );
    let (_, out, _) = odx(
        tmp.path(),
        &secrets,
        &["auth", "logout", "forge.example"],
        "",
    );
    assert_eq!(
        out,
        format!(
            "no token stored for forge.example in {}\n",
            secrets.display()
        )
    );
}

#[test]
fn auth_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let secrets = tmp.path().join("secrets.json");
    let (code, _, err) = odx(
        tmp.path(),
        &secrets,
        &["auth", "login", "forge.example"],
        "\n",
    );
    assert_eq!(code, 2);
    assert!(
        err.contains("pass the token for forge.example on standard input"),
        "{err}"
    );
    let (code, _, err) = odx(tmp.path(), &secrets, &["auth", "status"], "");
    assert_eq!(code, 2);
    assert!(err.contains("pyproject.toml"), "{err}");
    // A store that cannot be read or written.
    std::fs::write(&secrets, "not json").unwrap();
    let (code, _, err) = odx(
        tmp.path(),
        &secrets,
        &["auth", "login", "forge.example"],
        "t",
    );
    assert_eq!(code, 1);
    assert!(err.contains("secrets.json"), "{err}");
    let (code, _, _) = odx(
        tmp.path(),
        &secrets,
        &["auth", "logout", "forge.example"],
        "",
    );
    assert_eq!(code, 1);
}
