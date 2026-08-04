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
use std::ops::Deref;
use std::path::PathBuf;

/// Own every UTF-8 command-line allocation until parsing is complete, then
/// overwrite it. `-password VALUE` otherwise leaves the source `String`
/// behind after copying its bytes into [`Secret`].
struct WipingArgs(Vec<String>);

impl Deref for WipingArgs {
    type Target = [String];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for WipingArgs {
    fn drop(&mut self) {
        for value in &mut self.0 {
            crate::log::wipe_string(value);
        }
    }
}

fn wipe_os_string(value: OsString) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let mut bytes = value.into_vec();
        crate::log::wipe(&mut bytes);
    }
    #[cfg(not(unix))]
    drop(value);
}

/// The operation this invocation performs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    /// Open the game window.
    Run,
    /// Download and verify the current client, then exit.
    Sync,
    /// Verify and refill cached game data without changing the client.
    Repair,
    /// Serve the loopback origin without an AppKit window.
    Serve,
    /// Produce an unsigned, reviewable certificate candidate from the four
    /// official artifacts in `GWNATIVE_WEB_ROOT`.
    Certify,
    /// List configured launch profiles.
    Profiles,
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
    #[cfg(test)]
    fn expose(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }

    fn take(&mut self) -> Option<String> {
        String::from_utf8(std::mem::take(&mut self.0)).ok()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        crate::log::wipe(&mut self.0);
    }
}

/// The classic options that can be carried into the web client or native host.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LegacyOptions {
    pub email: Option<String>,
    pub password: Option<Secret>,
    pub character: Option<String>,
    pub fps: Option<u16>,
    pub window_mode: Option<WindowMode>,
    pub mute: bool,
    pub diagnostics: bool,
    pub performance: bool,
    pub no_patch_ui: bool,
    pub reset_preferences: bool,
}

impl Drop for LegacyOptions {
    fn drop(&mut self) {
        if let Some(email) = self.email.as_mut() {
            crate::log::wipe_string(email);
        }
    }
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
    pub image_path: Option<PathBuf>,
    pub jobs: Option<usize>,
    /// Whether the classic `-image` operation should populate the whole game
    /// image after synchronising the small client artifacts.
    pub full_image: bool,
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
            image_path: None,
            jobs: None,
            full_image: false,
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
    /// Move invocation-only credentials to the protected host route.
    ///
    /// They must not be serialized into the document-start launch JSON: a
    /// `WKUserScript` is retained for the lifetime of the web view and every
    /// later navigation. The page already fetches saved credentials from the
    /// one-shot host capability, so invocation credentials use that path too.
    pub fn take_credentials(&mut self) -> Option<(String, String)> {
        if !matches!(self.command, Command::Run | Command::Serve)
            || self.legacy.email.is_none()
            || self.legacy.password.is_none()
        {
            if let Some(mut username) = self.legacy.email.take() {
                crate::log::wipe_string(&mut username);
            }
            // `Secret::drop` wipes an incomplete or command-inapplicable value.
            self.legacy.password = None;
            return None;
        }
        let username = self.legacy.email.take()?;
        let password = self.legacy.password.as_mut()?.take()?;
        self.legacy.password = None;
        Some((username, password))
    }

    /// Whether this launch may schedule automatic client/application refreshes.
    /// Manual checks remain an explicit player action.
    pub fn automatic_updates_allowed(&self) -> bool {
        !self.offline && !self.no_update
    }

    /// Whether command routing must open the game-data snapshot.
    ///
    /// An explicit local image is itself a snapshot operation. Keeping this
    /// decision beside parsing prevents an accepted option from being skipped
    /// by a command-specific early return.
    pub fn needs_snapshot(&self) -> bool {
        match self.command {
            Command::Run | Command::Repair | Command::Serve => true,
            Command::Sync => self.full_image || self.image_path.is_some(),
            Command::Certify | Command::Profiles => false,
        }
    }

    /// The launch options needed inside the generated client's realm.
    ///
    /// This value is injected at document start and is never served by the
    /// loopback origin. Native filesystem locations and host ports stay on the
    /// Rust side.
    pub fn client_json(&self) -> serde_json::Value {
        serde_json::json!({
            "profile": self.profile.as_deref().unwrap_or("default"),
            "fps": self.legacy.fps,
            "mute": self.legacy.mute,
            "diagnostics": self.legacy.diagnostics,
            "performance": self.legacy.performance,
            "noPatchUi": self.legacy.no_patch_ui,
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
  certify             print an artifact-family certificate candidate
  profiles            list launch profiles

Native options:
  --profile NAME      use an isolated launch profile
  --new-instance      allow another isolated profile instance
  --host-port PORT    override the profile origin (bypasses its isolation)
  -d, --dir PATH      override the profile web root (can bypass isolation)
  -c, --cache PATH    override the game-data cache
  -j, --jobs COUNT    bound -image or repair workers (1–32)
  -i, --image PATH    use a local game image
  --offline           forbid launch-time network and automatic update refreshes
  --no-update         skip automatic client and application update checks
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
    let mut collected = WipingArgs(Vec::new());
    for raw in args.into_iter() {
        let raw: OsString = raw.into();
        match raw.into_string() {
            Ok(value) => collected.0.push(value),
            Err(value) => {
                wipe_os_string(value);
                return Err(value_error("argument", "is not valid Unicode"));
            }
        }
    }
    let args = collected;

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
            "run" | "sync" | "repair" | "serve" | "certify" | "profiles" => {
                no_inline(option, inline, || {})?;
                let command = match option {
                    "run" => Command::Run,
                    "sync" => Command::Sync,
                    "repair" => Command::Repair,
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
            "-c" | "--cache" | "--cache-root" => {
                let value = path_value(&args, &mut index, option, inline)?;
                set_once(&mut invocation.cache_root, value, option)?;
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
                    32,
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
                no_inline(option, inline, || {})?;
                unsupported(
                    &mut invocation,
                    option,
                    "credentials are prefilled, but the current WebAssembly client exposes no supported automatic login-submission hook",
                );
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
                unsupported(
                    &mut invocation,
                    option,
                    "the current WebAssembly client exposes no supported character-selection launch hook",
                );
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
                let _milliseconds = match inline {
                    Some(value) => parse_number::<u32>(value, option, 0, 100_000)?,
                    None if args
                        .get(index)
                        .and_then(|value| value.parse::<u32>().ok())
                        .is_some() =>
                    {
                        let value = &args[index];
                        index += 1;
                        parse_number::<u32>(value, option, 0, 100_000)?
                    }
                    None => 0,
                };
                unsupported(
                    &mut invocation,
                    option,
                    "the Windows stress harness is not part of the WebAssembly client",
                );
            }
            "-mock" => {
                let value = take_value(&args, &mut index, option, inline)?;
                if !value.eq_ignore_ascii_case("SteamDeck") {
                    return Err(value_error(option, "expected SteamDeck"));
                }
                unsupported(
                    &mut invocation,
                    option,
                    "the WebAssembly client exposes no supported platform-simulation hook",
                );
            }
            "-windowed" => {
                no_inline(option, inline, || {})?;
                set_once(
                    &mut invocation.legacy.window_mode,
                    WindowMode::Windowed,
                    option,
                )?;
            }
            "-windowedfullscreen" => {
                no_inline(option, inline, || {})?;
                set_once(
                    &mut invocation.legacy.window_mode,
                    WindowMode::Fullscreen,
                    option,
                )?;
            }
            "-image" => {
                no_inline(option, inline, || {})?;
                set_command(&mut invocation, &mut command_seen, Command::Sync, option)?;
                invocation.full_image = true;
                translated(
                    &mut invocation,
                    option,
                    "downloads and verifies the complete current client and game image",
                );
            }
            "-repair" => {
                no_inline(option, inline, || {})?;
                set_command(&mut invocation, &mut command_seen, Command::Repair, option)?;
            }
            "-update" => {
                no_inline(option, inline, || {})?;
                set_command(&mut invocation, &mut command_seen, Command::Sync, option)?;
                translated(
                    &mut invocation,
                    option,
                    "checks and installs current client artifacts; application updates remain separately consented",
                );
            }
            "-uninstall" => {
                no_inline(option, inline, || {})?;
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
            "-diag" => no_inline(option, inline, || invocation.legacy.diagnostics = true)?,
            "-perf" => no_inline(option, inline, || invocation.legacy.performance = true)?,
            "-log" => {
                no_inline(option, inline, || invocation.verbose = true)?;
                translated(
                    &mut invocation,
                    option,
                    "enables native HTTP and socket-size tracing",
                );
            }
            "-bmp" => unsupported_flag(
                &mut invocation,
                option,
                inline,
                "the WebAssembly client exposes no screenshot-format launch hook",
            )?,
            "-fqdn" => unsupported_flag(
                &mut invocation,
                option,
                inline,
                "authentication routing is owned by the current client and restricted native network bridge",
            )?,
            "-lodfull" => unsupported_flag(
                &mut invocation,
                option,
                inline,
                "the WebAssembly client exposes no supported model-detail launch hook",
            )?,
            "-nopatchui" => no_inline(option, inline, || invocation.legacy.no_patch_ui = true)?,
            "-noshaders" => unsupported_flag(
                &mut invocation,
                option,
                inline,
                "the WebGL client cannot run without its shaders",
            )?,
            "-noui" => unsupported_flag(
                &mut invocation,
                option,
                inline,
                "the WebAssembly client exposes no supported user-interface suppression hook",
            )?,
            "-oldfov" => unsupported_flag(
                &mut invocation,
                option,
                inline,
                "the WebAssembly client exposes no supported field-of-view launch hook",
            )?,
            "-prefresetlocal" => no_inline(option, inline, || {
                invocation.legacy.reset_preferences = true
            })?,
            "-resetmap" => unsupported_flag(
                &mut invocation,
                option,
                inline,
                "map state belongs to the current client and has no separately certified reset operation",
            )?,

            "-dsound" | "-sndasio" | "-sndwinmm" => unsupported_flag(
                &mut invocation,
                option,
                inline,
                "the WebAssembly client uses Web Audio; Windows sound backends do not apply",
            )?,
            "-dx8" => unsupported_flag(
                &mut invocation,
                option,
                inline,
                "the WebAssembly client renders through WebGL and WebKit/Metal, not DirectX 8",
            )?,
            "-mce" => unsupported_flag(
                &mut invocation,
                option,
                inline,
                "Windows Media Center integration is unavailable on macOS",
            )?,
            "-newauth" | "-oldauth" => unsupported_flag(
                &mut invocation,
                option,
                inline,
                "authentication selection is owned by ArenaNet's current WebAssembly client",
            )?,
            "-authsrv" | "-exit" | "-map" | "-port" | "-sndfastbuf" => {
                no_inline(option, inline, || no_known_effect(&mut invocation, option))?;
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
    if invocation.jobs.is_some()
        && !(matches!(invocation.command, Command::Sync | Command::Repair)
            && invocation.needs_snapshot())
    {
        return Err(value_error(
            "--jobs",
            "is only meaningful with -image, sync --image PATH, or repair",
        ));
    }
    if matches!(invocation.command, Command::Certify | Command::Profiles) {
        let ignored = invocation
            .cache_root
            .as_ref()
            .map(|_| "--cache")
            .or_else(|| invocation.image_path.as_ref().map(|_| "--image"))
            .or(invocation.no_prefetch.then_some("--no-prefetch"));
        if let Some(option) = ignored {
            return Err(value_error(option, "does not apply to certify or profiles"));
        }
    }
    if invocation.command == Command::Sync && !invocation.needs_snapshot() {
        if invocation.cache_root.is_some() {
            return Err(value_error(
                "--cache",
                "requires -image or --image PATH when used with sync",
            ));
        }
        if invocation.no_prefetch {
            return Err(value_error(
                "--no-prefetch",
                "requires -image or --image PATH when used with sync",
            ));
        }
    }
    if invocation.legacy.email.is_some() != invocation.legacy.password.is_some() {
        let option = if invocation.legacy.email.is_some() {
            "-email"
        } else {
            "-password"
        };
        unsupported(
            &mut invocation,
            option,
            "invocation credentials require both -email and -password in the current client",
        );
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

fn unsupported_flag(
    invocation: &mut Invocation,
    option: &str,
    inline: Option<&str>,
    message: &str,
) -> Result<(), Exit> {
    no_inline(option, inline, || unsupported(invocation, option, message))
}

fn no_known_effect(invocation: &mut Invocation, option: &str) {
    invocation.notices.push(Notice {
        option: option.to_owned(),
        kind: NoticeKind::NoKnownEffect,
        message: "the official Guild Wars documentation records no known usable behaviour".into(),
    });
}

fn answer(message: &str) -> Exit {
    Exit {
        message: message.to_owned(),
        failed: false,
    }
}

fn usage_error(argument: &str) -> Exit {
    let argument = if let Some((option, _)) = argument
        .split_once('=')
        .filter(|(name, _)| name.starts_with('-'))
    {
        format!("{option}=<value omitted>")
    } else if argument.starts_with('-') {
        "<unknown option omitted>".to_owned()
    } else {
        "<argument omitted>".to_owned()
    };
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
    }

    #[test]
    fn native_values_accept_separate_and_inline_forms() {
        let parsed = parse_str(&[
            "--profile=iron",
            "--host-port",
            "38113",
            "--cache=/tmp/cache",
            "--offline",
            "--debug",
        ])
        .unwrap();
        assert_eq!(parsed.profile.as_deref(), Some("iron"));
        assert_eq!(parsed.host_port, Some(38113));
        assert_eq!(parsed.cache_root, Some(PathBuf::from("/tmp/cache")));
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
    fn official_credentials_and_character_are_retained_but_redacted() {
        let mut parsed = parse_str(&[
            "-autologin",
            "-email",
            "player@example.test",
            "-password=hunter2",
            "-character",
            "Devona",
        ])
        .unwrap();
        assert_eq!(parsed.legacy.email.as_deref(), Some("player@example.test"));
        assert_eq!(
            parsed.legacy.password.as_ref().and_then(Secret::expose),
            Some("hunter2")
        );
        assert_eq!(parsed.legacy.character.as_deref(), Some("Devona"));
        assert!(parsed.notices.iter().any(|notice| {
            notice.option == "-autologin" && notice.kind == NoticeKind::Unsupported
        }));
        let debug = format!("{parsed:?}");
        assert!(!debug.contains("hunter2"));
        assert!(debug.contains("<redacted>"));
        let client = parsed.client_json();
        assert!(client.get("credentials").is_none());
        let credentials = parsed.take_credentials().unwrap();
        assert_eq!(credentials.0, "player@example.test");
        assert_eq!(credentials.1, "hunter2");
        assert!(client.get("autologin").is_none());
        assert!(client.get("mockSteamDeck").is_none());
    }

    #[test]
    fn partial_invocation_credentials_are_explained_not_injected() {
        for args in [
            &["-email", "player@example.test"][..],
            &["-password", "secret"][..],
        ] {
            let parsed = parse_str(args).unwrap();
            assert!(
                parsed
                    .notices
                    .iter()
                    .any(|notice| notice.kind == NoticeKind::Unsupported)
            );
            assert!(parsed.client_json().get("credentials").is_none());
        }
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
        assert!(parsed.full_image);
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
        assert!(parsed.legacy.no_patch_ui);
        assert!(parsed.legacy.reset_preferences);
        assert_eq!(parsed.notices.len(), 9);
        assert!(
            parsed
                .notices
                .iter()
                .all(|notice| notice.kind == NoticeKind::Unsupported)
        );
    }

    #[test]
    fn every_valueless_compatibility_switch_refuses_an_inline_value() {
        for option in [
            "-autologin",
            "-windowed",
            "-windowedfullscreen",
            "-update",
            "-uninstall",
            "-mute",
            "-nosound",
            "-diag",
            "-perf",
            "-log",
            "-bmp",
            "-fqdn",
            "-lodfull",
            "-nopatchui",
            "-noshaders",
            "-noui",
            "-oldfov",
            "-prefresetlocal",
            "-resetmap",
            "-dsound",
            "-sndasio",
            "-sndwinmm",
            "-dx8",
            "-mce",
            "-newauth",
            "-oldauth",
            "-authsrv",
            "-exit",
            "-map",
            "-port",
            "-sndfastbuf",
        ] {
            let inline = format!("{option}=unexpected");
            let exit = parse([inline]).unwrap_err();
            assert!(exit.failed, "{option}");
            assert!(exit.message.contains("does not take a value"), "{option}");
        }
    }

    #[test]
    fn stress_accepts_the_official_optional_zero_count() {
        for args in [&["-stress"][..], &["-stress", "0"][..], &["-stress=10"][..]] {
            let parsed = parse_str(args).unwrap();
            assert_eq!(parsed.notices.len(), 1);
            assert_eq!(parsed.notices[0].kind, NoticeKind::Unsupported);
        }
    }

    #[test]
    fn worker_bounds_only_apply_to_full_image_operations() {
        assert_eq!(parse_str(&["-image", "--jobs=8"]).unwrap().jobs, Some(8));
        assert_eq!(
            parse_str(&["sync", "--image", "/tmp/Gw.dat", "--jobs=6"])
                .unwrap()
                .jobs,
            Some(6)
        );
        assert_eq!(parse_str(&["repair", "--jobs=4"]).unwrap().jobs, Some(4));
        assert!(parse_str(&["--jobs=8"]).unwrap_err().failed);
    }

    #[test]
    fn every_accepted_image_path_reaches_snapshot_routing() {
        for command in ["run", "sync", "repair", "serve"] {
            let parsed = parse_str(&[command, "--image", "/tmp/Gw.dat"]).unwrap();
            assert!(parsed.image_path.is_some(), "{command}");
            assert!(parsed.needs_snapshot(), "{command}");
        }
        for command in ["certify", "profiles"] {
            assert!(
                parse_str(&[command, "--image", "/tmp/Gw.dat"])
                    .unwrap_err()
                    .failed,
                "{command} must fail instead of ignoring the image"
            );
        }
    }

    #[test]
    fn unknown_inline_values_are_not_echoed() {
        let error = parse_str(&["--password=credential-canary"]).unwrap_err();
        assert!(!error.message.contains("credential-canary"));
        assert!(error.message.contains("--password=<value omitted>"));

        let error = parse_str(&["credential-canary"]).unwrap_err();
        assert!(!error.message.contains("credential-canary"));
        assert!(error.message.contains("<argument omitted>"));
    }

    #[test]
    fn cache_options_never_hide_behind_an_early_return() {
        for args in [
            &["sync", "--cache", "/tmp/chunks"][..],
            &["sync", "--no-prefetch"][..],
            &["profiles", "--cache", "/tmp/chunks"][..],
            &["certify", "--no-prefetch"][..],
        ] {
            assert!(parse_str(args).unwrap_err().failed, "{args:?}");
        }
        assert!(
            parse_str(&["sync", "--image", "/tmp/Gw.dat", "--cache", "/tmp/chunks"])
                .unwrap()
                .needs_snapshot()
        );
    }

    #[test]
    fn invalid_values_and_conflicts_fail_closed() {
        for args in [
            &["-fps", "0"][..],
            &["-fps", "fast"][..],
            &["-mock", "Phone"][..],
            &["--jobs", "0"][..],
            &["-image", "--jobs", "33"][..],
            &["-image=unexpected"][..],
            &["-repair=unexpected"][..],
            &["--host-port", "70000"][..],
            &["--offline", "sync"][..],
            &["-windowed", "-windowedfullscreen"][..],
            &["--profile", "one", "--profile", "two"][..],
        ] {
            assert!(parse_str(args).unwrap_err().failed, "{args:?}");
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
        assert!(!exit.message.contains("--sync"));
        assert!(exit.message.contains("<unknown option omitted>"));
    }

    #[test]
    fn process_serial_number_is_ignored() {
        assert_eq!(
            parse_str(&["-psn_0_12345", "serve"]).unwrap().command,
            Command::Serve
        );
    }
}
