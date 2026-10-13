//! The user-level scheduled job `odx daemon install` sets up: a systemd user
//! timer, a launchd agent or a Task Scheduler task that runs `odx sync
//! --all`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The name of the systemd units and of the scheduled task.
pub const JOB_NAME: &str = "odx-sync";

/// The label of the launchd agent.
pub const LAUNCHD_LABEL: &str = "com.getfluvo.odx.sync";

/// An OS scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheduler {
    /// A systemd user timer (Linux).
    Systemd,
    /// A launchd agent (macOS).
    Launchd,
    /// A Task Scheduler task (Windows).
    TaskScheduler,
}

impl Scheduler {
    /// The scheduler of the platform odx was built for.
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    #[must_use]
    pub const fn native() -> Self {
        Self::Systemd
    }

    /// The scheduler of the platform odx was built for.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub const fn native() -> Self {
        Self::Launchd
    }

    /// The scheduler of the platform odx was built for.
    #[cfg(windows)]
    #[must_use]
    pub const fn native() -> Self {
        Self::TaskScheduler
    }

    /// A scheduler by name: `systemd`, `launchd` or `schtasks`.
    ///
    /// ```
    /// use odx_sync::schedule::Scheduler;
    ///
    /// assert_eq!(Scheduler::parse("launchd"), Some(Scheduler::Launchd));
    /// assert_eq!(Scheduler::parse("cron"), None);
    /// ```
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "systemd" => Some(Self::Systemd),
            "launchd" => Some(Self::Launchd),
            "schtasks" => Some(Self::TaskScheduler),
            _ => None,
        }
    }

    /// What the job is, for people.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Systemd => "systemd user timer",
            Self::Launchd => "launchd agent",
            Self::TaskScheduler => "Task Scheduler task",
        }
    }
}

/// What the scheduled job runs and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The odx executable.
    pub exe: String,
    /// Seconds between runs.
    pub interval: u64,
    /// Longest run in seconds; the scheduler stops it a little later.
    pub max_duration: u64,
    /// Low CPU and IO priority.
    pub nice: bool,
    /// Do not start (and stop) on battery power, where the scheduler can.
    pub pause_on_battery: bool,
}

/// A file the scheduler reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobFile {
    /// Where it goes.
    pub path: PathBuf,
    /// Its text.
    pub text: String,
    /// Whether it is written as UTF-16 (Task Scheduler XML) rather than
    /// UTF-8.
    pub utf16: bool,
}

impl JobFile {
    /// The bytes to write.
    ///
    /// ```
    /// use odx_sync::schedule::JobFile;
    ///
    /// let file = JobFile { path: "t.xml".into(), text: "a".into(), utf16: true };
    /// assert_eq!(file.bytes(), [0xFF, 0xFE, b'a', 0]);
    /// ```
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        if self.utf16 {
            std::iter::once(0xFEFF_u16)
                .chain(self.text.encode_utf16())
                .flat_map(u16::to_le_bytes)
                .collect()
        } else {
            self.text.clone().into_bytes()
        }
    }
}

/// A scheduler command: the program (`systemctl`, `launchctl` or
/// `schtasks`) and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The program.
    pub program: &'static str,
    /// Its arguments.
    pub args: Vec<String>,
    /// Whether a failure stops the operation.
    pub required: bool,
}

fn step(program: &'static str, args: &[&str], required: bool) -> Step {
    Step {
        program,
        args: args.iter().map(|&arg| arg.to_owned()).collect(),
        required,
    }
}

/// The files of the job, in `dir`: the systemd user unit directory, the
/// launch agents directory, or the odx configuration directory for the task
/// definition.
#[must_use]
pub fn files(scheduler: Scheduler, job: &Job, dir: &Path) -> Vec<JobFile> {
    match scheduler {
        Scheduler::Systemd => vec![
            JobFile {
                path: dir.join(format!("{JOB_NAME}.service")),
                text: systemd_service(job),
                utf16: false,
            },
            JobFile {
                path: dir.join(format!("{JOB_NAME}.timer")),
                text: systemd_timer(job),
                utf16: false,
            },
        ],
        Scheduler::Launchd => vec![JobFile {
            path: dir.join(format!("{LAUNCHD_LABEL}.plist")),
            text: launchd_plist(job),
            utf16: false,
        }],
        Scheduler::TaskScheduler => vec![JobFile {
            path: dir.join(format!("{JOB_NAME}.xml")),
            text: task_xml(job),
            utf16: true,
        }],
    }
}

/// The commands that register the job once its files are written.
#[must_use]
pub fn install_steps(scheduler: Scheduler, files: &[JobFile]) -> Vec<Step> {
    let first = files
        .first()
        .map(|file| file.path.display().to_string())
        .unwrap_or_default();
    let timer = format!("{JOB_NAME}.timer");
    match scheduler {
        Scheduler::Systemd => vec![
            step("systemctl", &["--user", "daemon-reload"], true),
            step("systemctl", &["--user", "enable", "--now", &timer], true),
        ],
        Scheduler::Launchd => vec![
            step("launchctl", &["unload", "-w", &first], false),
            step("launchctl", &["load", "-w", &first], true),
        ],
        Scheduler::TaskScheduler => vec![step(
            "schtasks",
            &["/Create", "/F", "/TN", JOB_NAME, "/XML", &first],
            true,
        )],
    }
}

/// The commands that unregister the job before its files are removed.
#[must_use]
pub fn uninstall_steps(scheduler: Scheduler, files: &[JobFile]) -> Vec<Step> {
    let first = files
        .first()
        .map(|file| file.path.display().to_string())
        .unwrap_or_default();
    let timer = format!("{JOB_NAME}.timer");
    match scheduler {
        Scheduler::Systemd => vec![step(
            "systemctl",
            &["--user", "disable", "--now", &timer],
            false,
        )],
        Scheduler::Launchd => vec![step("launchctl", &["unload", "-w", &first], false)],
        Scheduler::TaskScheduler => {
            vec![step("schtasks", &["/Delete", "/F", "/TN", JOB_NAME], false)]
        }
    }
}

/// The command that succeeds while the job is scheduled.
#[must_use]
pub fn status_step(scheduler: Scheduler) -> Step {
    match scheduler {
        Scheduler::Systemd => step(
            "systemctl",
            &[
                "--user",
                "is-active",
                "--quiet",
                &format!("{JOB_NAME}.timer"),
            ],
            true,
        ),
        Scheduler::Launchd => step("launchctl", &["list", LAUNCHD_LABEL], true),
        Scheduler::TaskScheduler => step("schtasks", &["/Query", "/TN", JOB_NAME], true),
    }
}

/// `exe` as one word of a systemd command line.
fn systemd_word(exe: &str) -> String {
    let escaped = exe
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

fn systemd_service(job: &Job) -> String {
    let mut text = String::from("[Unit]\nDescription=odx background sync\n");
    if job.pause_on_battery {
        text.push_str("ConditionACPower=true\n");
    }
    let _ = write!(
        text,
        "\n[Service]\nType=oneshot\nExecStart={} sync --all\nTimeoutStartSec={}\n",
        systemd_word(&job.exe),
        job.max_duration + 300
    );
    if job.nice {
        text.push_str("Nice=19\nCPUSchedulingPolicy=idle\nIOSchedulingClass=idle\n");
    }
    text
}

fn systemd_timer(job: &Job) -> String {
    format!(
        "[Unit]\nDescription=Run odx sync --all every {interval}s\n\n[Timer]\nOnActiveSec=300\nOnUnitActiveSec={interval}\nRandomizedDelaySec=60\n\n[Install]\nWantedBy=timers.target\n",
        interval = job.interval
    )
}

/// `text` with the XML special characters escaped.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn launchd_plist(job: &Job) -> String {
    let mut text = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>Label</key>\n\t<string>{LAUNCHD_LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{}</string>\n\t\t<string>sync</string>\n\t\t<string>--all</string>\n\t</array>\n\
         \t<key>StartInterval</key>\n\t<integer>{}</integer>\n\
         \t<key>RunAtLoad</key>\n\t<false/>\n",
        xml(&job.exe),
        job.interval
    );
    if job.nice {
        text.push_str(
            "\t<key>ProcessType</key>\n\t<string>Background</string>\n\
             \t<key>LowPriorityIO</key>\n\t<true/>\n\
             \t<key>Nice</key>\n\t<integer>10</integer>\n",
        );
    }
    text.push_str("</dict>\n</plist>\n");
    text
}

/// Whole minutes, at least one, as an XML duration.
fn minutes(seconds: u64) -> String {
    format!("PT{}M", (seconds / 60).max(1))
}

fn task_xml(job: &Job) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
         <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         \x20 <RegistrationInfo>\n    <Description>odx background sync</Description>\n  </RegistrationInfo>\n\
         \x20 <Triggers>\n    <TimeTrigger>\n      <Repetition>\n        <Interval>{interval}</Interval>\n        <StopAtDurationEnd>false</StopAtDurationEnd>\n      </Repetition>\n      <StartBoundary>2026-01-01T00:00:00</StartBoundary>\n      <Enabled>true</Enabled>\n    </TimeTrigger>\n  </Triggers>\n\
         \x20 <Principals>\n    <Principal id=\"Author\">\n      <LogonType>InteractiveToken</LogonType>\n      <RunLevel>LeastPrivilege</RunLevel>\n    </Principal>\n  </Principals>\n\
         \x20 <Settings>\n    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n    <DisallowStartIfOnBatteries>{battery}</DisallowStartIfOnBatteries>\n    <StopIfGoingOnBatteries>{battery}</StopIfGoingOnBatteries>\n    <StartWhenAvailable>true</StartWhenAvailable>\n    <ExecutionTimeLimit>{limit}</ExecutionTimeLimit>\n    <Priority>{priority}</Priority>\n  </Settings>\n\
         \x20 <Actions Context=\"Author\">\n    <Exec>\n      <Command>{exe}</Command>\n      <Arguments>sync --all</Arguments>\n    </Exec>\n  </Actions>\n\
         </Task>\n",
        interval = minutes(job.interval),
        battery = job.pause_on_battery,
        limit = minutes(job.max_duration + 300),
        priority = if job.nice { 7 } else { 5 },
        exe = xml(&job.exe),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(nice: bool, pause_on_battery: bool) -> Job {
        Job {
            exe: "/opt/odx & co/\"odx\" 100%".to_owned(),
            interval: 1800,
            max_duration: 1200,
            nice,
            pause_on_battery,
        }
    }

    fn texts(scheduler: Scheduler, job: &Job) -> String {
        files(scheduler, job, Path::new("dir"))
            .iter()
            .fold(String::new(), |mut all, file| {
                let _ = write!(all, "--- {}\n{}", file.path.display(), file.text);
                all
            })
    }

    #[test]
    fn systemd_units() {
        assert_eq!(
            texts(Scheduler::Systemd, &job(true, true)),
            format!(
                "--- {}\n[Unit]\nDescription=odx background sync\nConditionACPower=true\n\n[Service]\nType=oneshot\nExecStart=\"/opt/odx & co/\\\"odx\\\" 100%%\" sync --all\nTimeoutStartSec=1500\nNice=19\nCPUSchedulingPolicy=idle\nIOSchedulingClass=idle\n\
                 --- {}\n[Unit]\nDescription=Run odx sync --all every 1800s\n\n[Timer]\nOnActiveSec=300\nOnUnitActiveSec=1800\nRandomizedDelaySec=60\n\n[Install]\nWantedBy=timers.target\n",
                Path::new("dir").join("odx-sync.service").display(),
                Path::new("dir").join("odx-sync.timer").display()
            )
        );
        let plain = texts(Scheduler::Systemd, &job(false, false));
        assert!(
            !plain.contains("Nice") && !plain.contains("ACPower"),
            "{plain}"
        );
    }

    #[test]
    fn launchd_agent() {
        let text = texts(Scheduler::Launchd, &job(true, false));
        assert!(
            text.contains("<string>/opt/odx &amp; co/&quot;odx&quot; 100%</string>"),
            "{text}"
        );
        assert!(
            text.contains("<key>StartInterval</key>\n\t<integer>1800</integer>"),
            "{text}"
        );
        assert!(text.contains("<key>LowPriorityIO</key>"), "{text}");
        assert!(text.ends_with("</dict>\n</plist>\n"), "{text}");
        assert!(!texts(Scheduler::Launchd, &job(false, false)).contains("Nice"));
    }

    #[test]
    fn task_definition() {
        let file = files(Scheduler::TaskScheduler, &job(true, true), Path::new("dir"));
        let text = &file[0].text;
        assert!(file[0].utf16);
        assert!(text.contains("<Interval>PT30M</Interval>"), "{text}");
        assert!(
            text.contains("<ExecutionTimeLimit>PT25M</ExecutionTimeLimit>"),
            "{text}"
        );
        assert!(
            text.contains("<DisallowStartIfOnBatteries>true</DisallowStartIfOnBatteries>"),
            "{text}"
        );
        assert!(text.contains("<Priority>7</Priority>"), "{text}");
        assert!(
            text.contains("<Command>/opt/odx &amp; co/&quot;odx&quot; 100%</Command>"),
            "{text}"
        );
        let quick = Job {
            interval: 10,
            ..job(false, false)
        };
        let text = texts(Scheduler::TaskScheduler, &quick);
        assert!(text.contains("<Interval>PT1M</Interval>"), "{text}");
        assert!(text.contains("<Priority>5</Priority>"), "{text}");
        let bytes = file[0].bytes();
        assert_eq!(&bytes[..4], [0xFF, 0xFE, b'<', 0]);
        assert_eq!(
            files(Scheduler::Systemd, &quick, Path::new("d"))[0].bytes()[0],
            b'['
        );
    }

    #[test]
    fn steps() {
        let names = |steps: Vec<Step>| {
            steps
                .into_iter()
                .map(|step| format!("{} {} {}", step.program, step.args.join(" "), step.required))
                .collect::<Vec<_>>()
        };
        let dir = Path::new("d");
        for (scheduler, install, uninstall, status) in [
            (
                Scheduler::Systemd,
                vec![
                    "systemctl --user daemon-reload true".to_owned(),
                    "systemctl --user enable --now odx-sync.timer true".to_owned(),
                ],
                vec!["systemctl --user disable --now odx-sync.timer false".to_owned()],
                "systemctl --user is-active --quiet odx-sync.timer true",
            ),
            (
                Scheduler::Launchd,
                vec![
                    format!(
                        "launchctl unload -w {} false",
                        dir.join("com.getfluvo.odx.sync.plist").display()
                    ),
                    format!(
                        "launchctl load -w {} true",
                        dir.join("com.getfluvo.odx.sync.plist").display()
                    ),
                ],
                vec![format!(
                    "launchctl unload -w {} false",
                    dir.join("com.getfluvo.odx.sync.plist").display()
                )],
                "launchctl list com.getfluvo.odx.sync true",
            ),
            (
                Scheduler::TaskScheduler,
                vec![format!(
                    "schtasks /Create /F /TN odx-sync /XML {} true",
                    dir.join("odx-sync.xml").display()
                )],
                vec!["schtasks /Delete /F /TN odx-sync false".to_owned()],
                "schtasks /Query /TN odx-sync true",
            ),
        ] {
            let job_files = files(scheduler, &job(true, false), dir);
            assert_eq!(names(install_steps(scheduler, &job_files)), install);
            assert_eq!(names(uninstall_steps(scheduler, &job_files)), uninstall);
            assert_eq!(names(vec![status_step(scheduler)]), [status]);
            assert!(scheduler.describe().ends_with(['r', 't', 'k']));
        }
        assert_eq!(
            names(install_steps(Scheduler::Launchd, &[]))[1],
            "launchctl load -w  true"
        );
        assert_eq!(Scheduler::parse("systemd"), Some(Scheduler::Systemd));
        assert_eq!(Scheduler::parse("schtasks"), Some(Scheduler::TaskScheduler));
        assert!(Scheduler::native().describe().contains(' '));
    }
}
