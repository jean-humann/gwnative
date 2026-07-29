//! Command-line compatibility and native launch options.
//!
//! Guild Wars has accumulated a public command-line contract over two decades.
//! A macOS WebAssembly host cannot honour every Windows rendering or sound
//! backend switch, but it can still do the important part of compatibility:
//! accept every documented switch, translate the ones with a native equivalent,
//! and say exactly why the remainder has no effect. Silently ignoring a switch
//! is never compatibility.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

/// The operation this invocation performs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    /// Open the game window.
    Run,
    /// Download and verify the current client, then exit.
    Sync,
    /// Verify the installed client and cached game data, then exit.
    Repair,
    /// Serve the loopback origin without an AppKit window.
    Serve,
    /// List configured launch profiles.
    Profiles,
    /// List discovered mod bundles.
    Mods,
}

/// A native window-mode request corresponding to the classic client switches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowMode {
    Windowed,
    Fullscreen,
}

/// A password owned by the invocation.
///
/// The value is deliberately absent from `Debug`, and its allocation is
/// overwritten before release. Passing a password on a command line is still
/// visible to local process inspection; `--profile` and Keychain storage are
/// the preferred route.
#[derive(PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn expose(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// The classic options that can be carried into the web client or native host.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LegacyOptions {
    pub autologin: bool,
    pub email: Option<String>,
    pub password: Option<Secret>,
    pub character: Option<String>,
    pub fps: Option<u16>,
    pub window_mode: Option<WindowMode>,
    pub mute: bool,
    pub diagnostics: bool,
    pub performance: bool,
    pub log: bool,
    pub bitmap_screenshots: bool,
    pub fully_detailed_models: bool,
    pub fully_qualified_names: bool,
    pub mock_steam_deck: bool,
    pub no_patch_ui: bool,
    pub no_shaders: bool,
    pub no_ui: bool,
    pub old_fov: bool,
    pub reset_preferences: bool,
    pub reset_map: bool,
    pub stress_runs: Option<u32>,
}

/// Why a recognised option is not an exact implementation of the Windows
/// client behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Translated,
    Unsupported,
    NoKnownEffect,
}

/// One compatibility decision to report before launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub option: String,
    pub kind: NoticeKind,
    pub message: String,
}

/// Fully parsed launch intent.
#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    pub profile: Option<String>,
    pub new_instance: bool,
    pub host_port: Option<u16>,
    pub cache_root: Option<PathBuf>,
    pub web_root: Option<PathBuf>,
    pub mod_dir: Option<PathBuf>,
    pub modfile: Option<PathBuf>,
    pub image_path: Option<PathBuf>,
    pub jobs: Option<usize>,
    pub offline: bool,
    pub no_update: bool,
    pub no_prefetch: bool,
    pub devtools: bool,
    pub verbose: bool,
    pub legacy: LegacyOptions,
    pub notices: Vec<Notice>,
}

impl Default for Invocation {
    fn default() -> Self {
        Self {
            command: Command::Run,
            profile: None,
            new_instance: false,
            host_port: None,
            cache_root: None,
            web_root: None,
            mod_dir: None,
            modfile: None,
            image_path: None,
            jobs: None,
            offline: false,
            no_update: false,
            no_prefetch: false,
            devtools: false,
            verbose: false,
            legacy: LegacyOptions::default(),
            notices: Vec::new(),
        }
    }
}

impl Invocation {
    /// The launch options needed inside the generated client's realm.
    ///
    /// This value is injected at document start and is never served by the
    /// loopback origin. Native filesystem locations, host ports and mod paths
    /// stay on the Rust side.
    pub fn client_json(&self) -> serde_json::Value {
        let credentials = self
            .legacy
            .email
            .as_ref()
            .zip(self.legacy.password.as_ref().and_then(Secret::expose))
            .map(|(username, password)| {
                serde_json::json!({
                    "username": username,
                    "password": password,
                })
            });
        serde_json::json!({
            "profile": self.profile.as_deref().unwrap_or("default"),
            "autologin": self.legacy.autologin,
            "email": self.legacy.email,
            "credentials": credentials,
            "character": self.legacy.character,
            "fps": self.legacy.fps,
            "mute": self.legacy.mute,
            "diagnostics": self.legacy.diagnostics,
            "performance": self.legacy.performance,
            "bitmapScreenshots": self.legacy.bitmap_screenshots,
            "fullyDetailedModels": self.legacy.fully_detailed_models,
            "fullyQualifiedNames": self.legacy.fully_qualified_names,
            "mockSteamDeck": self.legacy.mock_steam_deck,
            "noPatchUi": self.legacy.no_patch_ui,
            "noShaders": self.legacy.no_shaders,
            "noUi": self.legacy.no_ui,
            "oldFov": self.legacy.old_fov,
            "resetPreferences": self.legacy.reset_preferences,
            "resetMap": self.legacy.reset_map,
            "stressRuns": self.legacy.stress_runs,
        })
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
  repair              verify and repair installed game data
  serve               serve the local origin without a window
  profiles            list launch profiles
  mods                list discovered mod bundles

Native options:
  --profile NAME      use an isolated launch profile
  --new-instance      allow another isolated profile instance
  --host-port PORT    choose the loopback origin port
  -d, --dir PATH      override the web-client directory
  -c, --cache PATH    override the game-data cache
  -m, --mods PATH     discover mod bundles beneath PATH
  -modfile PATH       load an explicit .gwmod session manifest
  -j, --jobs COUNT    bound parallel preparation work
  -i, --image PATH    use a local game image
  --offline           forbid launch-time network refreshes
  --no-update         skip client and application update checks
  --no-prefetch       disable speculative game-data fetches
  --no-browser        serve without opening an AppKit window
  --debug, --devtools enable Web Inspector support
  -v, --verbose       enable host request and socket tracing

Guild Wars compatibility:
  -autologin  -email VALUE  -password VALUE  -character NAME
  -image      -repair       -windowed        -windowedfullscreen
  -fps VALUE  -mute         -nosound         -diag  -log  -perf
  -bmp        -fqdn         -lodfull         -mock SteamDeck
  -nopatchui  -noshaders    -noui            -oldfov
  -prefresetlocal           -resetmap        -stress COUNT

All other switches documented by the official Guild Wars wiki are recognised
and produce a platform or compatibility explanation.

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
            "-h" | "--help" | "help" => return Err(answer(USAGE)),
            "-V" | "--version" | "version" => {
                return Err(answer(&format!("gwnative {}", env!("CARGO_PKG_VERSION"))));
            }
            "run" => set_command(&mut invocation, &mut command_seen, Command::Run, option)?,
            "sync" => set_command(&mut invocation, &mut command_seen, Command::Sync, option)?,
            "repair" => set_command(&mut invocation, &mut command_seen, Command::Repair, option)?,
            "serve" => set_command(&mut invocation, &mut command_seen, Command::Serve, option)?,
            "profiles" => set_command(
                &mut invocation,
                &mut command_seen,
                Command::Profiles,
                option,
            )?,
            "mods" => set_command(&mut invocation, &mut command_seen, Command::Mods, option)?,

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
            "-c" | "--cache" | "--cache-root" => {
                let value = path_value(&args, &mut index, option, inline)?;
                set_once(&mut invocation.cache_root, value, option)?;
            }
            "-m" | "--mods" | "--mod-dir" => {
                let value = path_value(&args, &mut index, option, inline)?;
                set_once(&mut invocation.mod_dir, value, option)?;
            }
            "-modfile" | "--modfile" => {
                let value = path_value(&args, &mut index, option, inline)?;
                set_once(&mut invocation.modfile, value, option)?;
            }
            "-i" | "--image" => {
                let value = path_value(&args, &mut index, option, inline)?;
                set_once(&mut invocation.image_path, value, option)?;
            }
            "-j" | "--jobs" => {
                let value = parse_number::<usize>(
                    take_value(&args, &mut index, option, inline)?,
                    option,
                    1,
                    256,
                )?;
                set_once(&mut invocation.jobs, value, option)?;
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

            "-autologin" => {
                no_inline(option, inline, || invocation.legacy.autologin = true)?;
            }
            "-email" => {
                let value = nonempty(take_value(&args, &mut index, option, inline)?, option)?;
                set_once(&mut invocation.legacy.email, value.to_owned(), option)?;
            }
            "-password" => {
                let value = nonempty(take_value(&args, &mut index, option, inline)?, option)?;
                set_once(
                    &mut invocation.legacy.password,
                    Secret(value.as_bytes().to_vec()),
                    option,
                )?;
                invocation.notices.push(Notice {
                    option: option.to_owned(),
                    kind: NoticeKind::Translated,
                    message: "the password is invocation-only; use --profile to keep credentials in Keychain".into(),
                });
            }
            "-character" => {
                let value = nonempty(take_value(&args, &mut index, option, inline)?, option)?;
                set_once(&mut invocation.legacy.character, value.to_owned(), option)?;
            }
            "-fps" => {
                let value = parse_number::<u16>(
                    take_value(&args, &mut index, option, inline)?,
                    option,
                    1,
                    1_000,
                )?;
                set_once(&mut invocation.legacy.fps, value, option)?;
            }
            "-stress" => {
                let value = parse_number::<u32>(
                    take_value(&args, &mut index, option, inline)?,
                    option,
                    1,
                    100_000,
                )?;
                set_once(&mut invocation.legacy.stress_runs, value, option)?;
            }
            "-mock" => {
                let value = take_value(&args, &mut index, option, inline)?;
                if !value.eq_ignore_ascii_case("SteamDeck") {
                    return Err(value_error(option, "expected SteamDeck"));
                }
                invocation.legacy.mock_steam_deck = true;
            }
            "-windowed" => {
                set_once(
                    &mut invocation.legacy.window_mode,
                    WindowMode::Windowed,
                    option,
                )?;
            }
            "-windowedfullscreen" => {
                set_once(
                    &mut invocation.legacy.window_mode,
                    WindowMode::Fullscreen,
                    option,
                )?;
            }
            "-image" => {
                set_command(&mut invocation, &mut command_seen, Command::Sync, option)?;
                translated(
                    &mut invocation,
                    option,
                    "downloads and verifies the complete current client image",
                );
            }
            "-repair" => {
                set_command(&mut invocation, &mut command_seen, Command::Repair, option)?;
            }
            "-update" => {
                set_command(&mut invocation, &mut command_seen, Command::Sync, option)?;
                translated(
                    &mut invocation,
                    option,
                    "checks and installs current client artifacts; application updates remain separately consented",
                );
            }
            "-uninstall" => {
                return Err(answer(
                    "gwnative does not remove player data from a command-line switch.\n\
                     Move Guild Wars.app to Trash, then remove \
                     ~/Library/Application Support/gwnative only if you also want \
                     to delete downloaded game data and settings.",
                ));
            }
            "-mute" | "-nosound" => {
                no_inline(option, inline, || invocation.legacy.mute = true)?;
            }
            "-diag" => invocation.legacy.diagnostics = true,
            "-perf" => invocation.legacy.performance = true,
            "-log" => invocation.legacy.log = true,
            "-bmp" => invocation.legacy.bitmap_screenshots = true,
            "-fqdn" => invocation.legacy.fully_qualified_names = true,
            "-lodfull" => invocation.legacy.fully_detailed_models = true,
            "-nopatchui" => invocation.legacy.no_patch_ui = true,
            "-noshaders" => invocation.legacy.no_shaders = true,
            "-noui" => invocation.legacy.no_ui = true,
            "-oldfov" => invocation.legacy.old_fov = true,
            "-prefresetlocal" => invocation.legacy.reset_preferences = true,
            "-resetmap" => invocation.legacy.reset_map = true,

            "-dsound" | "-sndasio" | "-sndwinmm" => unsupported(
                &mut invocation,
                option,
                "the WebAssembly client uses Web Audio; Windows sound backends do not apply",
            ),
            "-dx8" => unsupported(
                &mut invocation,
                option,
                "the WebAssembly client renders through WebGL and WebKit/Metal, not DirectX 8",
            ),
            "-mce" => unsupported(
                &mut invocation,
                option,
                "Windows Media Center integration is unavailable on macOS",
            ),
            "-newauth" | "-oldauth" => unsupported(
                &mut invocation,
                option,
                "authentication selection is owned by ArenaNet's current WebAssembly client",
            ),
            "-authsrv" | "-exit" | "-map" | "-port" | "-sndfastbuf" => {
                invocation.notices.push(Notice {
                    option: option.to_owned(),
                    kind: NoticeKind::NoKnownEffect,
                    message:
                        "the official Guild Wars documentation records no known usable behaviour"
                            .into(),
                });
            }
            _ => return Err(usage_error(raw)),
        }
    }

    if invocation.offline && invocation.command == Command::Sync {
        return Err(value_error(
            "--offline",
            "cannot be combined with sync, -image, or -update",
        ));
    }
    if invocation.new_instance && invocation.profile.is_none() {
        return Err(value_error(
            "--new-instance",
            "requires --profile so storage and credentials remain isolated",
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
    let valid = (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && value != "."
        && value != "..";
    if valid {
        Ok(())
    } else {
        Err(value_error(
            "--profile",
            "must be 1–64 ASCII letters, numbers, dots, underscores, or hyphens",
        ))
    }
}

fn translated(invocation: &mut Invocation, option: &str, message: &str) {
    invocation.notices.push(Notice {
        option: option.to_owned(),
        kind: NoticeKind::Translated,
        message: message.to_owned(),
    });
}

fn unsupported(invocation: &mut Invocation, option: &str, message: &str) {
    invocation.notices.push(Notice {
        option: option.to_owned(),
        kind: NoticeKind::Unsupported,
        message: message.to_owned(),
    });
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
            ("repair", Command::Repair),
            ("serve", Command::Serve),
            ("profiles", Command::Profiles),
            ("mods", Command::Mods),
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
    }

    #[test]
    fn native_values_accept_separate_and_inline_forms() {
        let parsed = parse_str(&[
            "--profile=iron",
            "--host-port",
            "38113",
            "--cache=/tmp/cache",
            "--modfile",
            "session.json",
            "--jobs=8",
            "--offline",
            "--debug",
        ])
        .unwrap();
        assert_eq!(parsed.profile.as_deref(), Some("iron"));
        assert_eq!(parsed.host_port, Some(38113));
        assert_eq!(parsed.cache_root, Some(PathBuf::from("/tmp/cache")));
        assert_eq!(parsed.modfile, Some(PathBuf::from("session.json")));
        assert_eq!(parsed.jobs, Some(8));
        assert!(parsed.offline);
        assert!(parsed.devtools);
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
            parse_str(&["--new-instance", "--profile", "second"])
                .unwrap()
                .new_instance
        );
    }

    #[test]
    fn official_credentials_and_character_are_retained_but_redacted() {
        let parsed = parse_str(&[
            "-autologin",
            "-email",
            "player@example.test",
            "-password=hunter2",
            "-character",
            "Devona",
        ])
        .unwrap();
        assert!(parsed.legacy.autologin);
        assert_eq!(parsed.legacy.email.as_deref(), Some("player@example.test"));
        assert_eq!(
            parsed.legacy.password.as_ref().and_then(Secret::expose),
            Some("hunter2")
        );
        assert_eq!(parsed.legacy.character.as_deref(), Some("Devona"));
        let debug = format!("{parsed:?}");
        assert!(!debug.contains("hunter2"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn official_host_switches_are_translated() {
        let parsed = parse_str(&[
            "-image",
            "-fps=144",
            "-mute",
            "-diag",
            "-perf",
            "-windowedfullscreen",
        ])
        .unwrap();
        assert_eq!(parsed.command, Command::Sync);
        assert_eq!(parsed.legacy.fps, Some(144));
        assert!(parsed.legacy.mute);
        assert!(parsed.legacy.diagnostics);
        assert!(parsed.legacy.performance);
        assert_eq!(parsed.legacy.window_mode, Some(WindowMode::Fullscreen));
    }

    #[test]
    fn every_documented_platform_switch_is_recognised() {
        for option in [
            "-dsound",
            "-dx8",
            "-mce",
            "-newauth",
            "-oldauth",
            "-sndasio",
            "-sndwinmm",
            "-authsrv",
            "-exit",
            "-map",
            "-port",
            "-sndfastbuf",
        ] {
            let parsed = parse_str(&[option]).unwrap();
            assert_eq!(parsed.notices.len(), 1, "{option}");
        }
    }

    #[test]
    fn all_stateful_official_switches_are_recognised() {
        let parsed = parse_str(&[
            "-bmp",
            "-fqdn",
            "-lodfull",
            "-mock",
            "SteamDeck",
            "-nopatchui",
            "-noshaders",
            "-noui",
            "-oldfov",
            "-prefresetlocal",
            "-resetmap",
            "-stress",
            "4",
        ])
        .unwrap();
        assert!(parsed.legacy.bitmap_screenshots);
        assert!(parsed.legacy.fully_qualified_names);
        assert!(parsed.legacy.fully_detailed_models);
        assert!(parsed.legacy.mock_steam_deck);
        assert!(parsed.legacy.no_patch_ui);
        assert!(parsed.legacy.no_shaders);
        assert!(parsed.legacy.no_ui);
        assert!(parsed.legacy.old_fov);
        assert!(parsed.legacy.reset_preferences);
        assert!(parsed.legacy.reset_map);
        assert_eq!(parsed.legacy.stress_runs, Some(4));
    }

    #[test]
    fn invalid_values_and_conflicts_fail_closed() {
        for args in [
            &["-fps", "0"][..],
            &["-fps", "fast"][..],
            &["-mock", "Phone"][..],
            &["--jobs", "0"][..],
            &["--host-port", "70000"][..],
            &["--offline", "sync"][..],
            &["-windowed", "-windowedfullscreen"][..],
            &["--profile", "one", "--profile", "two"][..],
        ] {
            assert!(parse_str(args).unwrap_err().failed, "{args:?}");
        }
    }

    #[test]
    fn uninstall_is_an_explanation_not_a_destructive_action() {
        let exit = parse_str(&["-uninstall"]).unwrap_err();
        assert!(!exit.failed);
        assert!(exit.message.contains("Trash"));
        assert!(exit.message.contains("Application Support"));
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
