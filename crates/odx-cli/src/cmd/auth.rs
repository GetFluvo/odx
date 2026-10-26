//! `odx auth`: forge tokens in the system keyring.

use std::fmt::Write as _;
use std::io::{IsTerminal as _, Read as _, Write as _};

use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::secrets::secrets;
use odx_forge::{ForgeKind, RepoUrl, token_source};

use super::Report;
use super::aggregate::prepare;
use crate::Context;
use crate::cli::{AuthCommand, GlobalArgs};

/// The token on standard input: one pasted line on a terminal, after a
/// prompt, else everything piped in.
#[cfg_attr(coverage_nightly, coverage(off))]
fn read_token(host: &str) -> String {
    let mut token = String::new();
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        let mut err = std::io::stderr();
        let _ = write!(err, "Paste the token for {host} and press Enter: ");
        let _ = err.flush();
        let _ = stdin.read_line(&mut token);
    } else {
        let _ = stdin.lock().read_to_string(&mut token);
    }
    token
}

/// `odx auth`.
pub(crate) fn run_auth(command: &AuthCommand, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    let store = secrets(ctx.env);
    let mut report = Report::default();
    match command {
        AuthCommand::Login { host } => {
            let token = read_token(host);
            let token = token.trim();
            if token.is_empty() {
                return Report::error(
                    Exit::Usage,
                    format!(
                        "pass the token for {host} on standard input, for example `odx auth login {host} < token.txt`"
                    ),
                );
            }
            match store.set(host, token) {
                Ok(()) => {
                    let _ = writeln!(
                        report.out,
                        "stored the token for {host} in {}",
                        store.describe()
                    );
                }
                Err(e) => return Report::error(Exit::Failure, e),
            }
        }
        AuthCommand::Logout { host } => match store.delete(host) {
            Ok(true) => {
                let _ = writeln!(
                    report.out,
                    "removed the token for {host} from {}",
                    store.describe()
                );
            }
            Ok(false) => {
                let _ = writeln!(
                    report.out,
                    "no token stored for {host} in {}",
                    store.describe()
                );
            }
            Err(e) => return Report::error(Exit::Failure, e),
        },
        AuthCommand::Status => {
            let prepared = match prepare(global, ctx) {
                Ok(prepared) => prepared,
                Err(report) => return report,
            };
            let config = &prepared.project.config;
            let mut hosts: Vec<String> = config
                .repositories
                .iter()
                .map(|repo| repo.url.as_str())
                .chain(config.packages.values().filter_map(|p| p.url.as_deref()))
                .chain(config.mirror.iter().map(|mirror| mirror.url.as_str()))
                .filter_map(RepoUrl::parse)
                .map(|repo| repo.host)
                .collect();
            hosts.sort();
            hosts.dedup();
            for host in hosts {
                let source = token_source(&host, ForgeKind::guess(&host), ctx.env, &OsRunner)
                    .map_or_else(
                        || "no token".to_owned(),
                        |(_, source)| format!("token from {source}"),
                    );
                let _ = writeln!(report.out, "{host}: {source}");
            }
        }
    }
    report
}
