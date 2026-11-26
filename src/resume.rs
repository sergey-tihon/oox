//! OSC 7577 lets a terminal resume a program after the terminal restarts.
//! See <https://www.superlogical.com/rex/docs/build/program-resume>.

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

const TOKEN_ENV: &str = "OSC7577_TOKEN";
const MAX_SEQUENCE_BYTES: usize = 2048;
const MAX_ARGV_ELEMENTS: usize = 64;
/// Bare name looked up on PATH at resume time, as the spec prefers; survives reinstalls.
const PROGRAM: &str = "oox";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeCommand {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
}

/// Take and remove the protocol token before any child process or thread is started.
pub fn take_token() -> Option<String> {
    let token = std::env::var_os(TOKEN_ENV);
    // SAFETY: this is called as the first statement of main, before any thread exists.
    unsafe { std::env::remove_var(TOKEN_ENV) };
    token
        .and_then(|token| token.into_string().ok())
        .filter(|token| !token.is_empty())
}

pub fn format_report(command: &ResumeCommand, token: &str, pid: u32, time: u64) -> Option<String> {
    if !is_reportable(command) {
        return None;
    }

    let mut pairs = String::from("argv=");
    for (index, argument) in command.argv.iter().enumerate() {
        if index != 0 {
            pairs.push(',');
        }
        pairs.push_str(&crate::base64_encode(argument.as_bytes()));
    }
    if let Some(cwd) = &command.cwd {
        pairs.push_str(":cwd=");
        pairs.push_str(&crate::base64_encode(cwd.as_bytes()));
    }

    let sequence = format_sequence(&pairs, token, pid, time);
    (sequence.len() <= MAX_SEQUENCE_BYTES).then_some(sequence)
}

pub fn format_clear(token: &str, pid: u32, time: u64) -> String {
    format_sequence("clear=1", token, pid, time)
}

fn format_sequence(pairs: &str, token: &str, pid: u32, time: u64) -> String {
    let body = format!("pid={pid}:time={time}:{pairs}");
    let digest = Sha256::new()
        .chain_update(body.as_bytes())
        .chain_update(b":")
        .chain_update(token.as_bytes())
        .finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut mac = String::with_capacity(64);
    for byte in digest {
        mac.push(HEX[(byte >> 4) as usize] as char);
        mac.push(HEX[(byte & 0x0f) as usize] as char);
    }
    format!("\x1b]7577;{body}:mac={mac}\x1b\\")
}

fn is_reportable(command: &ResumeCommand) -> bool {
    if command.argv.is_empty() || command.argv.len() > MAX_ARGV_ELEMENTS {
        return false;
    }

    let program = &command.argv[0];
    if program.is_empty() || contains_control(program) {
        return false;
    }
    let is_bare_name = !program.starts_with('-') && !program.contains(['/', '\\']);
    if !is_bare_name && !Path::new(program).is_absolute() {
        return false;
    }

    if command
        .argv
        .iter()
        .any(|argument| contains_control(argument))
    {
        return false;
    }
    if let Some(cwd) = &command.cwd
        && (!Path::new(cwd).is_absolute() || contains_control(cwd))
    {
        return false;
    }
    true
}

fn contains_control(text: &str) -> bool {
    text.chars().any(char::is_control)
}

fn unix_time() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
        .filter(|&seconds| seconds > 0)
}

fn absolute_utf8(path: &Path) -> Option<String> {
    std::path::absolute(path)
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
}

/// Holds the token and what the terminal currently has from this process.
pub struct Reporter {
    token: String,
    /// `--config` exactly as given on the command line; absolutized when a command is built.
    config: Option<PathBuf>,
    /// oox never changes directory, so the startup cwd is the resume cwd.
    cwd: Option<String>,
    /// `(file_path, compare_path)` the last sync was built from; `None` before the first sync.
    source: Option<(String, Option<PathBuf>)>,
    /// Whether the terminal holds a command reported by this process.
    reported: bool,
}

impl Reporter {
    pub fn new(token: String, config: Option<&Path>) -> Self {
        Self {
            token,
            config: config.map(Path::to_path_buf),
            cwd: std::env::current_dir()
                .ok()
                .and_then(|directory| directory.into_os_string().into_string().ok()),
            source: None,
            reported: false,
        }
    }

    pub fn sync(
        &mut self,
        file_path: &str,
        compare_path: Option<&Path>,
        out: &mut impl Write,
    ) -> io::Result<()> {
        if self
            .source
            .as_ref()
            .is_some_and(|(source_file, source_compare)| {
                source_file == file_path && source_compare.as_deref() == compare_path
            })
        {
            return Ok(());
        }
        let Some(time) = unix_time() else {
            return Ok(());
        };
        self.source = Some((file_path.to_owned(), compare_path.map(Path::to_path_buf)));

        let command = (|| {
            let mut argv = vec![PROGRAM.to_owned()];
            if let Some(config) = &self.config {
                argv.push("--config".to_owned());
                argv.push(absolute_utf8(config)?);
            }
            argv.push(absolute_utf8(Path::new(file_path))?);
            if let Some(compare_path) = compare_path {
                argv.push(absolute_utf8(compare_path)?);
            }
            Some(ResumeCommand {
                argv,
                cwd: self.cwd.clone(),
            })
        })();

        if let Some(sequence) = command
            .and_then(|command| format_report(&command, &self.token, std::process::id(), time))
        {
            out.write_all(sequence.as_bytes())?;
            self.reported = true;
        } else {
            out.write_all(format_clear(&self.token, std::process::id(), time).as_bytes())?;
            self.reported = false;
        }
        out.flush()
    }

    pub fn clear(&mut self, out: &mut impl Write) -> io::Result<()> {
        if !self.reported {
            return Ok(());
        }
        let Some(time) = unix_time() else {
            return Ok(());
        };
        out.write_all(format_clear(&self.token, std::process::id(), time).as_bytes())?;
        self.reported = false;
        out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "dummy-token-replace-me";
    const PID: u32 = 74877;
    const TIME: u64 = 1_795_543_200;

    fn command(argv: &[&str], cwd: Option<&str>) -> ResumeCommand {
        ResumeCommand {
            argv: argv.iter().map(|argument| (*argument).to_owned()).collect(),
            cwd: cwd.map(str::to_owned),
        }
    }

    fn written_time(bytes: &[u8]) -> u64 {
        let sequence = std::str::from_utf8(bytes).expect("OSC sequence is UTF-8");
        let body = sequence
            .strip_prefix("\x1b]7577;")
            .and_then(|sequence| sequence.strip_suffix("\x1b\\"))
            .expect("complete OSC sequence");
        body.split_once(":time=")
            .and_then(|(_, rest)| rest.split_once(':'))
            .and_then(|(time, _)| time.parse().ok())
            .expect("sequence time")
    }

    #[test]
    fn formats_spec_vectors() {
        assert_eq!(
            format_report(
                &command(
                    &[
                        "nvim",
                        "-S",
                        "/home/user/.local/state/nvim/sessions/8f2c1d.vim"
                    ],
                    None
                ),
                TOKEN,
                PID,
                TIME
            )
            .as_deref(),
            Some(
                "\x1b]7577;pid=74877:time=1795543200:argv=bnZpbQ==,LVM=,L2hvbWUvdXNlci8ubG9jYWwvc3RhdGUvbnZpbS9zZXNzaW9ucy84ZjJjMWQudmlt:mac=9dbb99bfb856749a87563839d7ce9062818d78cf25909db93f309e0cd3b1909b\x1b\\"
            )
        );
        assert_eq!(
            format_report(
                &command(&["lazygit"], Some("/home/user/src/project")),
                TOKEN,
                PID,
                TIME
            )
            .as_deref(),
            Some(
                "\x1b]7577;pid=74877:time=1795543200:argv=bGF6eWdpdA==:cwd=L2hvbWUvdXNlci9zcmMvcHJvamVjdA==:mac=35cb909805c380336efd3689a125acc774a9dec8422a1d21492ebec06ca758e1\x1b\\"
            )
        );
        assert_eq!(
            format_report(
                &command(&["resume-wrap", "ssh", "devbox"], None),
                TOKEN,
                90412,
                TIME
            )
            .as_deref(),
            Some(
                "\x1b]7577;pid=90412:time=1795543200:argv=cmVzdW1lLXdyYXA=,c3No,ZGV2Ym94:mac=e76149d0ba54a83570ffab0583867dacdfa23928ba9d18604f0badea88b62b65\x1b\\"
            )
        );
        assert_eq!(
            format_clear(TOKEN, PID, TIME),
            "\x1b]7577;pid=74877:time=1795543200:clear=1:mac=c30603369f98fef581a4098be889175b0c74f0641e49f8b25f4326cb38f97c93\x1b\\"
        );
        assert!(
            format_report(&command(&["oox", ""], None), TOKEN, PID, TIME)
                .expect("empty trailing argument is valid")
                .contains(":argv=b294,:mac=")
        );
    }

    #[test]
    fn rejects_unreportable_commands() {
        for invalid in [
            command(&[], None),
            command(&[""], None),
            command(&["./run.sh"], None),
            command(&["bin/oox"], None),
            command(&["-oox"], None),
            command(&["oox", "line\nbreak"], None),
            command(&["oox", "c1\u{9b}control"], None),
            command(&["oox"], Some("project")),
            command(&["oox"], Some("/tmp/\x1b")),
            command(&["oox", &"x".repeat(2048)], None),
        ] {
            assert_eq!(format_report(&invalid, TOKEN, PID, TIME), None);
        }
        let too_many = ResumeCommand {
            argv: std::iter::once("oox".to_owned())
                .chain(std::iter::repeat_n("x".to_owned(), 64))
                .collect(),
            cwd: None,
        };
        assert_eq!(format_report(&too_many, TOKEN, PID, TIME), None);

        #[cfg(unix)]
        assert!(
            format_report(
                &command(&["/usr/local/bin/oox"], Some("/")),
                TOKEN,
                PID,
                TIME
            )
            .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn reporter_syncs_changed_commands_and_clears() {
        let before = unix_time().expect("system time");
        let mut reporter = Reporter::new("test-token-0123456789abcdef".to_owned(), None);
        let mut output = Vec::new();

        reporter.sync("/tmp/a.pptx", None, &mut output).unwrap();
        let time = written_time(&output);
        let after = unix_time().expect("system time");
        assert!((before..=after).contains(&time));
        let expected = format_report(
            &ResumeCommand {
                argv: vec!["oox".to_owned(), "/tmp/a.pptx".to_owned()],
                cwd: reporter.cwd.clone(),
            },
            &reporter.token,
            std::process::id(),
            time,
        )
        .unwrap();
        assert_eq!(output, expected.as_bytes());

        let previous_len = output.len();
        reporter.sync("/tmp/a.pptx", None, &mut output).unwrap();
        assert_eq!(output.len(), previous_len);

        reporter
            .sync("/tmp/a.edited.pptx", None, &mut output)
            .unwrap();
        let appended = std::str::from_utf8(&output[previous_len..]).unwrap();
        let time = written_time(appended.as_bytes());
        assert_eq!(
            appended,
            format_report(
                &ResumeCommand {
                    argv: vec!["oox".to_owned(), "/tmp/a.edited.pptx".to_owned()],
                    cwd: reporter.cwd.clone(),
                },
                &reporter.token,
                std::process::id(),
                time,
            )
            .unwrap()
        );

        let mut configured = Reporter::new(
            "test-token-0123456789abcdef".to_owned(),
            Some(Path::new("/tmp/oox.toml")),
        );
        let mut config_output = Vec::new();
        configured
            .sync(
                "/tmp/a.pptx",
                Some(Path::new("/tmp/b.pptx")),
                &mut config_output,
            )
            .unwrap();
        let time = written_time(&config_output);
        assert_eq!(
            config_output,
            format_report(
                &ResumeCommand {
                    argv: [
                        "oox".to_owned(),
                        "--config".to_owned(),
                        "/tmp/oox.toml".to_owned(),
                        "/tmp/a.pptx".to_owned(),
                        "/tmp/b.pptx".to_owned(),
                    ]
                    .into(),
                    cwd: configured.cwd.clone(),
                },
                &configured.token,
                std::process::id(),
                time,
            )
            .unwrap()
            .as_bytes()
        );

        let mut clear_reporter = Reporter::new("test-token-0123456789abcdef".to_owned(), None);
        let mut clear_output = Vec::new();
        clear_reporter.clear(&mut clear_output).unwrap();
        assert!(clear_output.is_empty());
        clear_reporter
            .sync("/tmp/a.pptx", None, &mut clear_output)
            .unwrap();
        let report_len = clear_output.len();
        clear_reporter.clear(&mut clear_output).unwrap();
        let clear_sequence = std::str::from_utf8(&clear_output[report_len..]).unwrap();
        let time = written_time(clear_sequence.as_bytes());
        assert_eq!(
            clear_sequence,
            format_clear(&clear_reporter.token, std::process::id(), time)
        );
        let clear_len = clear_output.len();
        clear_reporter.clear(&mut clear_output).unwrap();
        assert_eq!(clear_output.len(), clear_len);

        let mut unreportable = Reporter::new("test-token-0123456789abcdef".to_owned(), None);
        let mut invalid_output = Vec::new();
        unreportable
            .sync("/tmp/line\nbreak.pptx", None, &mut invalid_output)
            .unwrap();
        let time = written_time(&invalid_output);
        assert_eq!(
            invalid_output,
            format_clear(&unreportable.token, std::process::id(), time).as_bytes()
        );
        let invalid_len = invalid_output.len();
        unreportable.clear(&mut invalid_output).unwrap();
        assert_eq!(invalid_output.len(), invalid_len);
    }
}
