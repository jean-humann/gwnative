//! What this invocation is, decided before anything is opened.
//!
//! Three runs share one executable, and until now the rule was "the second
//! argument, if it happens to be one of two words". Anything else — a typo, a
//! flag every other program on the system answers — fell through to the branch
//! that opens a window, so `gwnative --version` launched the game and printed
//! nothing. A published binary that answers `--version` by starting a 4 GB
//! download is not a small papercut.
//!
//! Unknown arguments are refused rather than ignored, because ignoring them is
//! how `--sync` silently does nothing while looking like it worked.

use std::ffi::OsString;

/// The run this invocation is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    /// Open the window. What a double-click means, and the only one that does.
    Run,
    /// Download the client and exit.
    Sync,
    /// Serve the origin without a window, so the snapshot range path can be
    /// exercised from curl or a test.
    Serve,
}

/// What to print and what to exit with, when the answer is not a run.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Exit {
    pub message: String,
    /// stderr and a non-zero status for a usage error; stdout and 0 for a
    /// question that was answered. `--help` on a terminal is not an error and
    /// should not need `2>&1` to read.
    pub failed: bool,
}

const USAGE: &str = "\
Guild Wars — a native macOS host for the Guild Wars client.

Usage: gwnative [command]

Commands:
  (none)         open the window
  sync           download the client and exit
  serve          run the origin without a window

Options:
  -h, --help     print this and exit
  -V, --version  print the version and exit";

/// Decide from the arguments after the executable's own.
///
/// `Ok` is a run; `Err` is something to print before exiting.
pub fn parse<I, S>(args: I) -> Result<Command, Exit>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut command = Command::Run;
    let mut seen = false;

    for arg in args {
        let arg = arg.into();
        let Some(arg) = arg.to_str() else {
            return Err(usage_error(&arg.to_string_lossy()));
        };

        // Launch Services has passed a process serial number to bundled
        // applications at various points in macOS's history. Nothing here wants
        // it, but refusing it would mean an app that opens from a terminal and
        // not from the Dock — the one failure this function must not cause.
        if arg.starts_with("-psn_") {
            continue;
        }

        match arg {
            "-h" | "--help" | "help" => {
                return Err(Exit {
                    message: USAGE.to_string(),
                    failed: false,
                });
            }
            "-V" | "--version" | "version" => {
                return Err(Exit {
                    message: format!("gwnative {}", env!("CARGO_PKG_VERSION")),
                    failed: false,
                });
            }
            _ if seen => return Err(usage_error(arg)),
            "sync" => (command, seen) = (Command::Sync, true),
            "serve" => (command, seen) = (Command::Serve, true),
            _ => return Err(usage_error(arg)),
        }
    }

    Ok(command)
}

fn usage_error(arg: &str) -> Exit {
    Exit {
        message: format!("gwnative: unrecognised argument \"{arg}\"\n\n{USAGE}"),
        failed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(args: &[&str]) -> Result<Command, Exit> {
        parse(args.iter().copied())
    }

    #[test]
    fn no_arguments_is_a_windowed_run() {
        assert_eq!(parse_str(&[]), Ok(Command::Run));
    }

    #[test]
    fn the_two_commands_are_still_the_two_commands() {
        assert_eq!(parse_str(&["sync"]), Ok(Command::Sync));
        assert_eq!(parse_str(&["serve"]), Ok(Command::Serve));
    }

    /// The regression this module exists for: it used to open a window.
    #[test]
    fn version_prints_and_does_not_run() {
        for flag in ["-V", "--version", "version"] {
            let exit = parse_str(&[flag]).unwrap_err();
            assert_eq!(
                exit.message,
                format!("gwnative {}", env!("CARGO_PKG_VERSION"))
            );
            assert!(!exit.failed, "{flag} is a question, not a mistake");
        }
    }

    #[test]
    fn help_goes_to_stdout_with_a_zero_status() {
        for flag in ["-h", "--help", "help"] {
            let exit = parse_str(&[flag]).unwrap_err();
            assert!(exit.message.contains("Usage: gwnative"));
            assert!(!exit.failed);
        }
    }

    #[test]
    fn an_unknown_argument_is_refused_rather_than_ignored() {
        let exit = parse_str(&["--sync"]).unwrap_err();
        assert!(exit.failed);
        assert!(exit.message.contains("--sync"));
        // Saying what is wrong without saying what is right is half a message.
        assert!(exit.message.contains("Usage: gwnative"));
    }

    #[test]
    fn a_second_command_is_a_mistake_and_not_the_last_one_wins() {
        assert!(parse_str(&["sync", "serve"]).unwrap_err().failed);
    }

    /// Refusing this would mean an app that opens from a terminal and not from
    /// the Dock, which is a worse bug than the one being fixed.
    #[test]
    fn a_process_serial_number_is_ignored() {
        assert_eq!(parse_str(&["-psn_0_12345"]), Ok(Command::Run));
        assert_eq!(parse_str(&["-psn_0_12345", "serve"]), Ok(Command::Serve));
    }

    #[test]
    fn help_wins_over_a_command_so_it_answers_rather_than_runs() {
        assert!(!parse_str(&["sync", "--help"]).unwrap_err().failed);
    }
}
