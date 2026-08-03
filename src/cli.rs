//! Native commands and launch options, decided before any resources are opened.
//!
//! Unknown arguments are refused rather than ignored. Legacy Guild Wars command-
//! line compatibility is intentionally layered in a separate change so the
//! profile-isolation foundation remains independently reviewable.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

/// The operation this invocation performs.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Command {
    /// Open the game window.
    #[default]
    Run,
    /// Download and verify the current client, then exit.
    Sync,
    /// Serve the loopback origin without an AppKit window.
    Serve,
    /// Produce an unsigned, reviewable certificate candidate from the four
    /// official artifacts in `GWNATIVE_WEB_ROOT`.
    Certify,
    /// List configured launch profiles.
    Profiles,
}

/// Fully parsed native launch intent.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    pub profile: Option<String>,
    pub new_instance: bool,
    pub host_port: Option<u16>,
    pub web_root: Option<PathBuf>,
    pub offline: bool,
    pub no_update: bool,
    pub no_prefetch: bool,
    pub devtools: bool,
    pub verbose: bool,
}

impl Invocation {
    /// Whether this launch may schedule automatic client/application refreshes.
    /// Manual checks remain an explicit player action.
    pub fn automatic_updates_allowed(&self) -> bool {
        !self.offline && !self.no_update
    }
}

/// What to print and how to exit when parsing does not produce an invocation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Exit {
    pub message: String,
    /// Usage failures go to stderr with status 2; answered questions go to
    /// stdout with status 0.
    pub failed: bool,
}

const USAGE: &str = "\
Guild Wars — a native macOS host for the Guild Wars client.

Usage: gwnative [command] [options]

Commands:
  run                 open the game window (default)
  sync                download and verify the current client
  serve               serve the local origin without a window
  certify             print an artifact-family certificate candidate
  profiles            list launch profiles

Native options:
  --profile NAME      use an isolated launch profile
  --new-instance      allow another isolated profile instance
  --host-port PORT    override the profile origin (bypasses its isolation)
  -d, --dir PATH      override the profile web root (can bypass isolation)
  --offline           forbid launch-time network and automatic update refreshes
  --no-update         skip automatic client and application update checks
  --no-prefetch       disable speculative game-data fetches
  --no-browser        serve without opening an AppKit window
  --debug, --devtools enable Web Inspector support
  -v, --verbose       enable host request and socket tracing

General:
  -h, --help          print this and exit
  -V, --version       print the version and exit";

/// Parse arguments after the executable name.
pub fn parse<I, S>(args: I) -> Result<Invocation, Exit>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let args = args
        .into_iter()
        .map(Into::into)
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| usage_error(&arg.to_string_lossy()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut invocation = Invocation::default();
    let mut command_seen = false;
    let mut index = 0usize;

    while index < args.len() {
        let raw = &args[index];
        index += 1;

        // Launch Services may supply this historic process serial number.
        if raw.starts_with("-psn_") {
            continue;
        }

        let (option, inline) = raw
            .split_once('=')
            .map_or((raw.as_str(), None), |(name, value)| (name, Some(value)));

        match option {
            "-h" | "--help" | "help" => {
                no_inline(option, inline, || {})?;
                return Err(answer(USAGE));
            }
            "-V" | "--version" | "version" => {
                no_inline(option, inline, || {})?;
                return Err(answer(&format!("gwnative {}", env!("CARGO_PKG_VERSION"))));
            }
            "run" | "sync" | "serve" | "certify" | "profiles" => {
                no_inline(option, inline, || {})?;
                let command = match option {
                    "run" => Command::Run,
                    "sync" => Command::Sync,
                    "serve" => Command::Serve,
                    "certify" => Command::Certify,
                    "profiles" => Command::Profiles,
                    _ => unreachable!(),
                };
                set_command(&mut invocation, &mut command_seen, command, option)?;
            }
            "--profile" => {
                let value = take_value(&args, &mut index, option, inline)?;
                validate_profile(value)?;
                set_once(&mut invocation.profile, value.to_owned(), option)?;
            }
            "--new-instance" => no_inline(option, inline, || invocation.new_instance = true)?,
            "--host-port" | "-p" | "--port" => {
                let value = parse_number::<u16>(
                    take_value(&args, &mut index, option, inline)?,
                    option,
                    1,
                    u16::MAX,
                )?;
                set_once(&mut invocation.host_port, value, option)?;
            }
            "-d" | "--dir" => {
                let value = path_value(&args, &mut index, option, inline)?;
                set_once(&mut invocation.web_root, value, option)?;
            }
            "--offline" => no_inline(option, inline, || invocation.offline = true)?,
            "--no-update" => no_inline(option, inline, || invocation.no_update = true)?,
            "--no-prefetch" => no_inline(option, inline, || invocation.no_prefetch = true)?,
            "--no-browser" => {
                no_inline(option, inline, || {})?;
                set_command(&mut invocation, &mut command_seen, Command::Serve, option)?;
            }
            "--debug" | "--devtools" => {
                no_inline(option, inline, || invocation.devtools = true)?;
            }
            "-v" | "--verbose" => {
                no_inline(option, inline, || invocation.verbose = true)?;
            }
            _ => return Err(usage_error(raw)),
        }
    }

    if invocation.offline && invocation.command == Command::Sync {
        return Err(value_error("--offline", "cannot be combined with sync"));
    }
    if invocation.new_instance
        && invocation
            .profile
            .as_deref()
            .is_none_or(|profile| profile == "default")
    {
        return Err(value_error(
            "--new-instance",
            "requires a non-default --profile so storage and credentials remain isolated",
        ));
    }
    Ok(invocation)
}

fn set_command(
    invocation: &mut Invocation,
    seen: &mut bool,
    command: Command,
    shown: &str,
) -> Result<(), Exit> {
    if *seen {
        return Err(value_error(shown, "only one command may be selected"));
    }
    invocation.command = command;
    *seen = true;
    Ok(())
}

fn set_once<T>(slot: &mut Option<T>, value: T, option: &str) -> Result<(), Exit> {
    if slot.is_some() {
        return Err(value_error(option, "may only be supplied once"));
    }
    *slot = Some(value);
    Ok(())
}

fn take_value<'a>(
    args: &'a [String],
    index: &mut usize,
    option: &str,
    inline: Option<&'a str>,
) -> Result<&'a str, Exit> {
    if let Some(value) = inline {
        return nonempty(value, option);
    }
    let Some(value) = args.get(*index) else {
        return Err(value_error(option, "requires a value"));
    };
    *index += 1;
    nonempty(value, option)
}

fn path_value(
    args: &[String],
    index: &mut usize,
    option: &str,
    inline: Option<&str>,
) -> Result<PathBuf, Exit> {
    let value = take_value(args, index, option, inline)?;
    if value.as_bytes().contains(&0) {
        return Err(value_error(option, "path contains a NUL byte"));
    }
    Ok(PathBuf::from(value))
}

fn nonempty<'a>(value: &'a str, option: &str) -> Result<&'a str, Exit> {
    if value.is_empty() {
        Err(value_error(option, "requires a non-empty value"))
    } else {
        Ok(value)
    }
}

fn parse_number<T>(value: &str, option: &str, minimum: T, maximum: T) -> Result<T, Exit>
where
    T: std::str::FromStr + PartialOrd + fmt::Display + Copy,
{
    let parsed = value.parse::<T>().map_err(|_| {
        value_error(
            option,
            &format!("expected a number from {minimum} through {maximum}"),
        )
    })?;
    if parsed < minimum || parsed > maximum {
        return Err(value_error(
            option,
            &format!("expected a number from {minimum} through {maximum}"),
        ));
    }
    Ok(parsed)
}

fn no_inline(option: &str, inline: Option<&str>, apply: impl FnOnce()) -> Result<(), Exit> {
    if inline.is_some() {
        return Err(value_error(option, "does not take a value"));
    }
    apply();
    Ok(())
}

fn validate_profile(value: &str) -> Result<(), Exit> {
    if crate::profile::valid_id(value) {
        Ok(())
    } else {
        Err(value_error(
            "--profile",
            "must be 1–64 ASCII letters, numbers, dots, underscores, or hyphens",
        ))
    }
}

fn answer(message: &str) -> Exit {
    Exit {
        message: message.to_owned(),
        failed: false,
    }
}

fn usage_error(argument: &str) -> Exit {
    Exit {
        message: format!("gwnative: unrecognised argument \"{argument}\"\n\n{USAGE}"),
        failed: true,
    }
}

fn value_error(option: &str, reason: &str) -> Exit {
    Exit {
        message: format!("gwnative: {option} {reason}\n\n{USAGE}"),
        failed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(args: &[&str]) -> Result<Invocation, Exit> {
        parse(args.iter().copied())
    }

    #[test]
    fn no_arguments_is_a_windowed_run() {
        assert_eq!(parse_str(&[]).unwrap().command, Command::Run);
    }

    #[test]
    fn native_commands_are_exclusive() {
        for (argument, command) in [
            ("run", Command::Run),
            ("sync", Command::Sync),
            ("serve", Command::Serve),
            ("certify", Command::Certify),
            ("profiles", Command::Profiles),
        ] {
            assert_eq!(parse_str(&[argument]).unwrap().command, command);
        }
        assert!(parse_str(&["sync", "serve"]).unwrap_err().failed);
    }

    #[test]
    fn version_and_help_answer_without_running() {
        for flag in ["-V", "--version", "version"] {
            let exit = parse_str(&[flag]).unwrap_err();
            assert!(exit.message.starts_with("gwnative "));
            assert!(!exit.failed);
        }
        for flag in ["-h", "--help", "help"] {
            let exit = parse_str(&[flag]).unwrap_err();
            assert!(exit.message.contains("Usage: gwnative"));
            assert!(!exit.failed);
        }
        for flag in ["--help=yes", "--version=yes"] {
            assert!(parse_str(&[flag]).unwrap_err().failed);
        }
    }

    #[test]
    fn native_values_accept_separate_and_inline_forms() {
        let parsed = parse_str(&[
            "--profile=iron",
            "--host-port",
            "38113",
            "--dir=./web",
            "--offline",
            "--no-update",
            "--no-prefetch",
            "--debug",
            "--verbose",
        ])
        .unwrap();
        assert_eq!(parsed.profile.as_deref(), Some("iron"));
        assert_eq!(parsed.host_port, Some(38113));
        assert_eq!(parsed.web_root, Some(PathBuf::from("./web")));
        assert!(parsed.offline);
        assert!(parsed.no_update);
        assert!(parsed.no_prefetch);
        assert!(parsed.devtools);
        assert!(parsed.verbose);
    }

    #[test]
    fn profile_names_are_safe_directory_components() {
        for invalid in ["", ".", "..", "a/b", "a b", "../default", "éowyn"] {
            assert!(parse_str(&["--profile", invalid]).unwrap_err().failed);
        }
        for valid in ["default", "iron-man", "account_2", "v1.0"] {
            assert_eq!(
                parse_str(&["--profile", valid]).unwrap().profile.as_deref(),
                Some(valid)
            );
        }
    }

    #[test]
    fn isolated_instances_require_a_profile() {
        assert!(parse_str(&["--new-instance"]).unwrap_err().failed);
        assert!(
            parse_str(&["--profile", "default", "--new-instance"])
                .unwrap_err()
                .failed
        );
        assert!(
            parse_str(&["--new-instance", "--profile", "second"])
                .unwrap()
                .new_instance
        );
    }

    #[test]
    fn invalid_values_and_conflicts_fail_closed() {
        for args in [
            &["--host-port", "0"][..],
            &["--host-port", "70000"][..],
            &["--offline", "sync"][..],
            &["--profile", "one", "--profile", "two"][..],
        ] {
            assert!(parse_str(args).unwrap_err().failed, "{args:?}");
        }
    }

    #[test]
    fn valueless_native_options_refuse_inline_values() {
        for option in [
            "--new-instance",
            "--offline",
            "--no-update",
            "--no-prefetch",
            "--no-browser",
            "--debug",
            "--devtools",
            "--verbose",
        ] {
            let inline = format!("{option}=yes");
            assert!(parse([inline]).unwrap_err().failed, "{option}");
        }
    }

    #[test]
    fn offline_and_no_update_suppress_automatic_refreshes() {
        assert!(parse_str(&[]).unwrap().automatic_updates_allowed());
        assert!(
            !parse_str(&["--offline"])
                .unwrap()
                .automatic_updates_allowed()
        );
        assert!(
            !parse_str(&["--no-update"])
                .unwrap()
                .automatic_updates_allowed()
        );
    }

    #[test]
    fn unknown_arguments_are_refused() {
        let exit = parse_str(&["--sync"]).unwrap_err();
        assert!(exit.failed);
        assert!(exit.message.contains("--sync"));
    }

    #[test]
    fn process_serial_number_is_ignored() {
        assert_eq!(
            parse_str(&["-psn_0_12345", "serve"]).unwrap().command,
            Command::Serve
        );
    }
}
