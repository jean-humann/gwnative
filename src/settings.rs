//! What the player chose, kept across launches.
//!
//! One small JSON file in the support directory. Every field is read by
//! something that already exists here: `renderScale` is what the client asks
//! for through `emscripten_get_device_pixel_ratio`, `touchMode` selects which
//! gesture translation `input.js` installs, `showDiagnostics` opens the log pane
//! at boot, `dataStrategy` records the answer to the launcher's one question,
//! `autoCheckUpdates`, `autoInstallUpdates` and `lastUpdateCheckAt` describe
//! update intent and cadence, `compatibilityNoticeSeenFor` is which exact
//! runtime pair has already been recorded, and `nativeCursor` plus
//! `targetReadout` select the two optional enhancements. Nothing is stored for a
//! feature this app does not have — a settings file whose fields nothing reads
//! is a migration burden that never bought anything.
//!
//! The reader is deliberately lopsided: an unknown *field* is ignored, an
//! unknown *value* is refused. A file written by a later build should still
//! yield everything this build understands, but a `touchMode` of `"maybe"`
//! means the file cannot be trusted about touch, and quietly substituting a
//! default there would leave the player's input behaving in a way no setting of
//! theirs explains. A `formatVersion` this build does not know is refused
//! outright rather than reinterpreted, and [`Store::open`] then moves the file
//! aside intact instead of overwriting a shape it could not read.
//!
//! Two fields are readable by the page and not writable by it — see
//! [`PATCHABLE`]. One describes the file itself; the other is the host's record
//! of a request the host made, and a page that could write it could tell this
//! build a check had just happened and never be asked to make another.

use std::cmp::Reverse;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The shape this build writes. Bumped only when a field changes meaning —
/// adding one does not, because a missing field already reads as its default.
const FORMAT: u32 = 1;

/// How many `settings.json.corrupt-<epoch>` files to keep.
///
/// A backup exists so a lost setting can be recovered by hand. Nothing reads
/// the fourth-oldest one, and without a bound they accumulate for the life of
/// the profile.
const CORRUPT_BACKUPS_KEPT: usize = 3;

/// The render scales the client is asked to draw at, as a device pixel ratio.
///
/// A set rather than a range: these three are exactly representable, so
/// equality is the right test, and an arbitrary ratio would let a typo in the
/// file cost a fifth of the frame rate with nothing on screen to explain it.
const RENDER_SCALES: [f64; 3] = [1.0, 1.5, 2.0];

/// What a mouse gesture is turned into before the client sees it.
///
/// Not a preference so much as a repair. ArenaNet's build registers no
/// `dblclick` handler, and the Emscripten mouse event it does read carries no
/// click count — so *there is no path by which a double click can reach the
/// game as a double click*. The client's own double-tap detector is on its
/// touch path, which is why [`Dbltap`](Self::Dbltap) exists and why it is the
/// default: without it, picking an item up and equipping it are simply not
/// things a player can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TouchMode {
    /// Nothing is translated, and double clicking does not work. Kept for
    /// diagnosing whether a problem is this file's fault.
    Off,
    /// A macOS double click — its speed and distance as the player has set them
    /// system-wide — is replayed as the pair of taps the client recognises.
    /// Single clicks, drags and the right button are untouched.
    Dbltap,
    /// Every left-button gesture becomes a touch and the mouse event is
    /// withheld from the client.
    Translate,
    /// Both: the client sees the touch and the mouse event.
    Augment,
}

/// Whether the player wants the whole 4.2 GB on disk or only what a session
/// touches. `None` means they have not been asked yet, which is what makes the
/// launcher's question a first-run question rather than a recurring one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataStrategy {
    Quick,
    Full,
}

/// Not `Copy`, deliberately: [`Settings::compatibility_notice_seen_for`] is a
/// 64-character hash, and the alternative — a fixed-width array with a hex codec
/// either side of it — would be machinery bought to keep a struct copyable that
/// is read a handful of times a session.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub render_scale: f64,
    pub touch_mode: TouchMode,
    pub show_diagnostics: bool,
    pub data_strategy: Option<DataStrategy>,
    /// Whether a launch may look for a newer build. Off unless the player turns
    /// it on: see [`crate::release`] for why the check is asked for rather than
    /// volunteered.
    pub auto_check_updates: bool,
    /// Whether a found update may install itself, without being asked each
    /// time. Only meaningful in a bundle carrying Sparkle — the fallback check
    /// has nothing to install with, and the settings panel does not offer the
    /// switch on a build that cannot honour it.
    ///
    /// This field and the one above are app-global. A selected profile sees them
    /// through [`ScopedStore`], but they live in the global `updates.json`
    /// because Sparkle's preferences and the application update itself are also
    /// app-global. [`crate::updater`] documents which way the copy runs.
    pub auto_install_updates: bool,
    /// When the last check — automatic or from the menu — got an answer, in
    /// seconds since the epoch. What makes an opted-in launch ask once a day
    /// rather than once a launch.
    pub last_update_check_at: Option<u64>,
    /// The domain-separated SHA-256 of the selected runtime, exact generated
    /// JavaScript/WebAssembly pair, transform ABI and selected output this
    /// profile has already been warned about. Per compatibility attempt rather
    /// than a boolean because a changed pair or fixed transform deserves its
    /// own record.
    pub compatibility_notice_seen_for: Option<String>,
    /// Draw the game's own cursor art with the page's compositor instead of
    /// letting the client draw it into the frame. See [`crate::wasm`] for what
    /// has to happen to the client for this to be possible at all.
    pub native_cursor: bool,
    /// Show what the player has targeted, read out of the game each tick.
    pub target_readout: bool,
}

impl Settings {
    /// Whether this launch needs the enhanced client module.
    ///
    /// Derived rather than stored, and deliberately so. A `enhancementsEnabled`
    /// field could say no while a tool it governs said yes, and nothing in the
    /// file would say which of the two the session actually obeyed. Asking the
    /// tools directly means the question cannot have two answers.
    pub fn enhancements_enabled(&self) -> bool {
        self.native_cursor || self.target_readout
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // The client draws at the ratio it is given and the compositor
            // scales the result. 2 is a Retina panel's native ratio, so it is
            // the one that does not resample.
            render_scale: 2.0,
            // See TouchMode. Off would ship a game whose inventory cannot be
            // used, so the repair is on unless it is turned off deliberately.
            touch_mode: TouchMode::Dbltap,
            show_diagnostics: false,
            data_strategy: None,
            auto_check_updates: false,
            auto_install_updates: false,
            last_update_check_at: None,
            compatibility_notice_seen_for: None,
            // On: a cursor the compositor draws is the one thing in this app
            // that makes the game feel like it is running at the display's
            // refresh rate rather than the client's.
            native_cursor: true,
            // Off: it puts a panel over the game, which is a change to what the
            // player sees and not one to volunteer.
            target_readout: false,
        }
    }
}

/// Every field optional, so serde's own type checking does the work of
/// rejecting `"showDiagnostics": "yes"` while an absent field stays absent.
/// Unknown fields are ignored, which is serde's default and is the intent.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
    format_version: Option<u32>,
    render_scale: Option<f64>,
    touch_mode: Option<TouchMode>,
    show_diagnostics: Option<bool>,
    data_strategy: Option<DataStrategy>,
    auto_check_updates: Option<bool>,
    auto_install_updates: Option<bool>,
    last_update_check_at: Option<u64>,
    compatibility_notice_seen_for: Option<String>,
    native_cursor: Option<bool>,
    target_readout: Option<bool>,
}

/// The names a patch may carry.
///
/// Two fields the file holds are absent, and for one reason each.
/// `formatVersion` describes the file, and a page that could set it could make
/// the next launch unable to read its own settings. `lastUpdateCheckAt` is the
/// host's record of a request the host made — a page that could write it could
/// suppress the daily check indefinitely by claiming one had just happened. Both
/// are still *read* from the file, because a launch has to be able to load what
/// the previous one wrote; see [`ScopedStore::remember_update_check`] for the one way
/// the second is set.
///
/// `compatibilityNoticeSeenFor` is here rather than beside them because the page
/// records the selected runtime-pair state after rendering the same state in
/// Settings. What stops it being abused is the shape check below: the value must
/// be a client hash, and only the exact value injected for a later launch can
/// suppress its diagnostic record.
const PATCHABLE: [&str; 9] = [
    "renderScale",
    "touchMode",
    "showDiagnostics",
    "dataStrategy",
    "autoCheckUpdates",
    "autoInstallUpdates",
    "compatibilityNoticeSeenFor",
    "nativeCursor",
    "targetReadout",
];

/// Whether `value` is the shape [`digest`](crate::wasm) writes: 64 lowercase hex
/// characters.
///
/// Checked rather than taken on trust because the field is compared for equality
/// against this launch's runtime-pair hash, and a value that can never match is a
/// notice that reappears at every launch with nothing on screen to explain why.
/// The failure is loud here instead.
fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Fold whatever `raw` says over `base`.
///
/// `null` for `dataStrategy` is a value, not an omission — it is how the
/// launcher's question gets asked again — so presence is taken from the object
/// rather than from the deserialized `Option`, which cannot tell the two apart.
fn merge(base: Settings, raw: &serde_json::Value) -> Result<Settings, String> {
    let object = raw
        .as_object()
        .ok_or_else(|| "settings must be an object".to_owned())?;
    let wire: Wire = serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
    if let Some(version) = wire.format_version
        && version != FORMAT
    {
        return Err(format!("settings formatVersion {version} is not readable"));
    }

    let mut out = base;
    if let Some(scale) = wire.render_scale {
        if !RENDER_SCALES.contains(&scale) {
            return Err(format!("renderScale {scale} is not one of 1, 1.5, 2"));
        }
        out.render_scale = scale;
    }
    if let Some(mode) = wire.touch_mode {
        out.touch_mode = mode;
    }
    if let Some(show) = wire.show_diagnostics {
        out.show_diagnostics = show;
    }
    if object.contains_key("dataStrategy") {
        out.data_strategy = wire.data_strategy;
    }
    if let Some(auto) = wire.auto_check_updates {
        out.auto_check_updates = auto;
    }
    if let Some(auto) = wire.auto_install_updates {
        out.auto_install_updates = auto;
    }
    if let Some(at) = wire.last_update_check_at {
        out.last_update_check_at = Some(at);
    }
    // `null` here is "warn me about this build again", which is a value and not
    // an omission — same reason `dataStrategy` is read off the object.
    if object.contains_key("compatibilityNoticeSeenFor") {
        if let Some(seen) = &wire.compatibility_notice_seen_for
            && !is_sha256(seen)
        {
            return Err("compatibilityNoticeSeenFor must be null or a client sha256".to_owned());
        }
        out.compatibility_notice_seen_for = wire.compatibility_notice_seen_for;
    }
    if let Some(on) = wire.native_cursor {
        out.native_cursor = on;
    }
    if let Some(on) = wire.target_readout {
        out.target_readout = on;
    }
    Ok(out)
}

/// Read a whole settings file: anything it does not say takes its default.
pub fn parse(raw: &serde_json::Value) -> Result<Settings, String> {
    merge(Settings::default(), raw)
}

/// Apply a patch: anything it does not mention keeps the value it had.
///
/// Unlike [`parse`], an unknown field is an error. A page sending `renderscale`
/// has a bug, and answering 200 to it would hide the bug behind a setting that
/// silently never changes.
pub fn patch(current: Settings, raw: &serde_json::Value) -> Result<Settings, String> {
    let object = raw
        .as_object()
        .ok_or_else(|| "a settings patch must be an object".to_owned())?;
    if let Some(unknown) = object.keys().find(|key| !PATCHABLE.contains(&key.as_str())) {
        return Err(format!("unknown setting {unknown:?}"));
    }
    merge(current, raw)
}

/// The settings file, and the one live copy of what is in it.
///
/// Held in memory because every read is on the boot path or answering the page,
/// and re-reading a file to answer "what is the render scale" would put a
/// syscall between the client and its first frame for no gain. The file is only
/// touched when something changes.
pub struct Store {
    path: PathBuf,
    current: Mutex<Settings>,
}

/// Move local preferences aside for `-prefresetlocal`.
///
/// The command asks for defaults, not irreversible deletion. Keeping the last
/// file beside the new one makes the operation recoverable without allowing a
/// backup to accumulate on every invocation.
pub fn reset(path: &Path) -> std::io::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let backup = path.with_extension("json.reset");
    match fs::remove_file(&backup) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(path, backup)?;
    Ok(true)
}

impl Store {
    /// Load `path`, or start from defaults.
    ///
    /// A file that cannot be read is moved aside rather than deleted or
    /// overwritten: it is the only copy of choices the player made, and the
    /// case that produces it — a build that wrote a shape this one does not
    /// understand — is exactly the case where they may want it back.
    pub fn open(path: PathBuf) -> Self {
        let current = match load(&path) {
            Ok(settings) => settings,
            Err(reason) => {
                match set_aside(&path) {
                    Some(backup) => note!(
                        "[settings] {} is unreadable ({reason}); kept as {}",
                        path.display(),
                        backup.display()
                    ),
                    None => note!("[settings] {} is unreadable: {reason}", path.display()),
                }
                Settings::default()
            }
        };
        Self {
            path,
            current: Mutex::new(current),
        }
    }

    pub fn get(&self) -> Settings {
        self.current.lock().unwrap().clone()
    }

    /// Fold a patch in and write the result.
    ///
    /// The in-memory copy is updated only once the file is on disk, so a page
    /// that is told its change was saved can rely on the next launch agreeing.
    pub fn apply(&self, raw: &serde_json::Value) -> Result<Settings, String> {
        let mut current = self.current.lock().unwrap();
        let next = patch(current.clone(), raw)?;
        if next != *current {
            save(&self.path, &next).map_err(|e| format!("could not save settings: {e}"))?;
            *current = next.clone();
        }
        Ok(next)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSettings {
    format_version: u32,
    auto_check_updates: bool,
    auto_install_updates: bool,
    last_update_check_at: Option<u64>,
}

impl UpdateSettings {
    fn from_legacy(settings: &Settings) -> Self {
        Self {
            format_version: FORMAT,
            auto_check_updates: settings.auto_check_updates,
            auto_install_updates: settings.auto_install_updates,
            last_update_check_at: settings.last_update_check_at,
        }
    }
}

/// The application-wide updater state, deliberately separate from every full
/// profile settings file. Concurrent profiles may both update this one small
/// record, but neither can write a stale copy of the default profile's render or
/// input settings over a newer choice.
pub struct UpdateStore {
    path: PathBuf,
    current: Mutex<UpdateSettings>,
}

const UPDATE_LOCK_PATIENCE: Duration = Duration::from_secs(5);

impl UpdateStore {
    pub fn open(path: PathBuf, legacy: &Settings) -> Self {
        let fallback = UpdateSettings::from_legacy(legacy);
        let store = Self {
            path,
            current: Mutex::new(fallback),
        };
        store.initialize();
        store
    }

    #[cfg(test)]
    fn memory(legacy: &Settings) -> Self {
        Self {
            path: PathBuf::new(),
            current: Mutex::new(UpdateSettings::from_legacy(legacy)),
        }
    }

    fn acquire_lock(&self) -> Result<Option<crate::instance::Instance>, String> {
        if self.path.as_os_str().is_empty() {
            return Ok(None);
        }
        crate::instance::acquire(&self.path.with_extension("lock"), UPDATE_LOCK_PATIENCE)
            .map(Some)
            .map_err(|error| format!("could not lock application update settings: {error}"))
    }

    fn initialize(&self) {
        let Ok(_lock) = self.acquire_lock() else {
            note!("[settings] application update preferences could not be locked");
            return;
        };
        let mut current = self.current.lock().unwrap();
        match load_updates(&self.path) {
            Ok(Some(on_disk)) => *current = on_disk,
            Ok(None) => {
                if let Err(error) = save_updates(&self.path, &current) {
                    note!("[settings] app update preferences could not be migrated: {error}");
                }
            }
            Err(reason) => {
                match set_aside(&self.path) {
                    Some(backup) => note!(
                        "[settings] {} is unreadable ({reason}); kept as {}",
                        self.path.display(),
                        backup.display()
                    ),
                    None => note!("[settings] {} is unreadable: {reason}", self.path.display()),
                }
                if let Err(error) = save_updates(&self.path, &current) {
                    note!("[settings] app update preferences could not be restored: {error}");
                }
            }
        }
    }

    fn get(&self) -> UpdateSettings {
        let Ok(_lock) = self.acquire_lock() else {
            note!("[settings] application update preferences could not be refreshed");
            return *self.current.lock().unwrap();
        };
        let mut current = self.current.lock().unwrap();
        match load_updates(&self.path) {
            Ok(Some(on_disk)) => *current = on_disk,
            Ok(None) if self.path.as_os_str().is_empty() => {}
            Ok(None) => note!("[settings] application update preferences disappeared"),
            Err(error) => {
                note!("[settings] application update preferences are unreadable: {error}")
            }
        }
        *current
    }

    fn update_preferences(
        &self,
        decide: impl FnOnce((bool, bool)) -> (bool, bool),
    ) -> Result<(bool, bool), String> {
        let _lock = self.acquire_lock()?;
        let mut current = self.current.lock().unwrap();
        if let Some(on_disk) = load_updates(&self.path)? {
            *current = on_disk;
        }
        let (check, install) = decide((current.auto_check_updates, current.auto_install_updates));
        let next = UpdateSettings {
            auto_check_updates: check,
            auto_install_updates: install,
            ..*current
        };
        if next == *current {
            return Ok((check, install));
        }
        save_updates(&self.path, &next)
            .map_err(|error| format!("could not save update preferences: {error}"))?;
        *current = next;
        Ok((check, install))
    }

    fn apply_preferences(&self, check: Option<bool>, install: Option<bool>) -> Result<(), String> {
        self.update_preferences(|current| {
            (check.unwrap_or(current.0), install.unwrap_or(current.1))
        })
        .map(|_| ())
    }

    fn remember_check(&self, at: u64) {
        let _lock = match self.acquire_lock() {
            Ok(lock) => lock,
            Err(error) => {
                note!("[settings] the update-check time was not saved: {error}");
                return;
            }
        };
        let mut current = self.current.lock().unwrap();
        match load_updates(&self.path) {
            Ok(Some(on_disk)) => *current = on_disk,
            Ok(None) => {}
            Err(error) => {
                note!("[settings] the update-check time was not saved: {error}");
                return;
            }
        }
        if current.last_update_check_at == Some(at) {
            return;
        }
        let next = UpdateSettings {
            last_update_check_at: Some(at),
            ..*current
        };
        match save_updates(&self.path, &next) {
            Ok(()) => *current = next,
            Err(error) => note!("[settings] the update-check time was not saved: {error}"),
        }
    }
}

/// A profile's settings with the application-wide update preferences overlaid.
///
/// Rendering, input, diagnostics and enhancements belong to the selected
/// profile. Application updates do not: Sparkle persists its switches in the
/// bundle's standard user defaults, shared by every process and profile. The
/// small update-settings file is therefore the durable app-global owner of those
/// switches and the last-check timestamp. Keeping that split here prevents each
/// caller from inventing a subtly different merge.
pub struct ScopedStore {
    profile: Arc<Store>,
    updates: Arc<UpdateStore>,
}

impl ScopedStore {
    pub fn new(profile: Arc<Store>, updates: Arc<UpdateStore>) -> Self {
        Self { profile, updates }
    }

    /// A single-file scope, used by the default profile and unit tests.
    #[cfg(test)]
    pub fn single(store: Store) -> Self {
        let updates = Arc::new(UpdateStore::memory(&store.get()));
        let store = Arc::new(store);
        Self::new(store, updates)
    }

    pub fn get(&self) -> Settings {
        let mut profile = self.profile.get();
        let updates = self.updates.get();
        profile.auto_check_updates = updates.auto_check_updates;
        profile.auto_install_updates = updates.auto_install_updates;
        profile.last_update_check_at = updates.last_update_check_at;
        profile
    }

    /// Apply profile-owned fields to the profile and update-owned fields to the
    /// global store. Validate the combined patch first so a bad field cannot
    /// produce a partial write.
    pub fn apply(&self, raw: &serde_json::Value) -> Result<Settings, String> {
        let _validated = patch(self.get(), raw)?;
        let object = raw
            .as_object()
            .expect("settings::patch accepted only an object");
        let mut profile_patch = serde_json::Map::new();
        let mut update_patch = serde_json::Map::new();
        for (name, value) in object {
            if matches!(name.as_str(), "autoCheckUpdates" | "autoInstallUpdates") {
                update_patch.insert(name.clone(), value.clone());
            } else {
                profile_patch.insert(name.clone(), value.clone());
            }
        }
        if !update_patch.is_empty() {
            self.updates.apply_preferences(
                update_patch
                    .contains_key("autoCheckUpdates")
                    .then_some(_validated.auto_check_updates),
                update_patch
                    .contains_key("autoInstallUpdates")
                    .then_some(_validated.auto_install_updates),
            )?;
        }
        if !profile_patch.is_empty() {
            self.profile
                .apply(&serde_json::Value::Object(profile_patch))?;
        }
        Ok(self.get())
    }

    pub fn remember_update_check(&self, at: u64) {
        self.updates.remember_check(at);
    }

    /// Settle the application-wide updater switches while holding their
    /// cross-process lock.
    ///
    /// The closure runs synchronously under the lock, so callers may reconcile
    /// Sparkle's shared `NSUserDefaults` and return the exact values that must
    /// be mirrored to `updates.json` without another profile interleaving a
    /// stale read or write. Callers must never carry this lock across an async
    /// dispatch; dispatch first, then enter here on the destination thread.
    pub fn reconcile_update_preferences(
        &self,
        decide: impl FnOnce((bool, bool)) -> (bool, bool),
    ) -> Result<(bool, bool), String> {
        self.updates.update_preferences(decide)
    }
}

fn load_updates(path: &Path) -> Result<Option<UpdateSettings>, String> {
    let body = match fs::read(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let updates: UpdateSettings =
        serde_json::from_slice(&body).map_err(|error| error.to_string())?;
    if updates.format_version != FORMAT {
        return Err(format!(
            "update settings formatVersion {} is not readable",
            updates.format_version
        ));
    }
    Ok(Some(updates))
}

fn save_updates(path: &Path, updates: &UpdateSettings) -> std::io::Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(updates)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(&body)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Seconds since the epoch, or 0 on a clock that will not answer.
///
/// Zero rather than a failure because every caller is deciding how long ago
/// something was, and a clock that cannot be read is one whose answers are
/// worthless — 0 makes "long ago" the conclusion, which is the safe end of a
/// decision about whether to make one network request.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn load(path: &Path) -> Result<Settings, String> {
    let body = match fs::read(path) {
        Ok(body) => body,
        // Never launched, or a fresh profile. Not a failure, and nothing to
        // move aside.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(e) => return Err(e.to_string()),
    };
    let raw: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    parse(&raw)
}

/// Same rename-in discipline as a chunk: a launch that read a half-written file
/// would find it corrupt and move aside settings that were never damaged.
fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Stored<'a> {
        format_version: u32,
        #[serde(flatten)]
        settings: &'a Settings,
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(&Stored {
        format_version: FORMAT,
        settings,
    })?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Move an unreadable file to `settings.json.corrupt-<epoch-ms>` and prune the
/// older backups. `None` if there was nothing to move or it could not be moved,
/// both of which leave the caller doing the same thing: start from defaults.
fn set_aside(path: &Path) -> Option<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    let backup = with_suffix(path, &format!("corrupt-{stamp}"));
    fs::rename(path, &backup).ok()?;
    prune_backups(path);
    Some(backup)
}

fn prune_backups(path: &Path) {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return;
    };
    let prefix = format!("{name}.corrupt-");
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    // The epoch is in the name, so ordering costs no `stat`, and only names
    // this module writes are ever considered for removal.
    let mut backups: Vec<(u128, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name();
            let stamp = file.to_str()?.strip_prefix(&prefix)?.parse().ok()?;
            Some((stamp, entry.path()))
        })
        .collect();
    // Newest first, so `skip` below keeps the most recent and deletes the tail.
    backups.sort_unstable_by_key(|(stamp, _)| Reverse(*stamp));
    for (_, stale) in backups.into_iter().skip(CORRUPT_BACKUPS_KEPT) {
        let _ = fs::remove_file(stale);
    }
}

/// `settings.json` + `.corrupt-17…` — appended to the whole name rather than
/// replacing the extension, so the backup of `settings.json` is not
/// `settings.corrupt-17…` and out of the prefix scan above.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::TempDir;
    use serde_json::json;

    /// The guard comes back with the path: dropping it removes the directory,
    /// so a test that binds only the path would delete its own scratch space
    /// before touching it.
    fn scratch(name: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new(&format!("settings-{name}"));
        let path = dir.0.join("settings.json");
        (dir, path)
    }

    /// The client has no double-click of its own, so this default is not a
    /// preference — it is whether a fresh install can equip an item. It shipped
    /// as `Off` once and the game arrived unable to use its own inventory, with
    /// nothing on screen to say why.
    #[test]
    fn a_fresh_profile_can_double_click() {
        assert_eq!(Settings::default().touch_mode, TouchMode::Dbltap);
        assert_eq!(parse(&json!({})).unwrap().touch_mode, TouchMode::Dbltap);
    }

    #[test]
    fn an_empty_object_is_every_default() {
        assert_eq!(parse(&json!({})).unwrap(), Settings::default());
    }

    #[test]
    fn an_unknown_field_is_ignored_but_an_unknown_value_is_not() {
        let forward =
            parse(&json!({"touchMode": "augment", "tuchMode": "off", "fromALaterBuild": true}));
        assert_eq!(forward.unwrap().touch_mode, TouchMode::Augment);

        assert!(parse(&json!({"touchMode": "maybe"})).is_err());
        assert!(parse(&json!({"showDiagnostics": "yes"})).is_err());
        assert!(parse(&json!({"renderScale": 1.25})).is_err());
        assert!(parse(&json!([])).is_err());
    }

    #[test]
    fn a_format_this_build_cannot_read_is_refused_rather_than_reinterpreted() {
        assert!(parse(&json!({"formatVersion": FORMAT, "renderScale": 1.5})).is_ok());
        let later = parse(&json!({"formatVersion": FORMAT + 1, "renderScale": 1.5}));
        assert!(
            later.is_err(),
            "a later format must not be read as this one"
        );
    }

    #[test]
    fn a_patch_keeps_what_it_does_not_mention_and_refuses_what_it_misspells() {
        let current = Settings {
            render_scale: 1.5,
            touch_mode: TouchMode::Dbltap,
            show_diagnostics: true,
            data_strategy: Some(DataStrategy::Full),
            ..Settings::default()
        };
        let after = patch(current.clone(), &json!({"showDiagnostics": false})).unwrap();
        assert_eq!(
            after,
            Settings {
                show_diagnostics: false,
                ..current.clone()
            }
        );
        assert!(patch(current.clone(), &json!({"renderscale": 1})).is_err());
        assert!(patch(current, &json!({"formatVersion": 1})).is_err());
    }

    #[test]
    fn null_clears_the_data_strategy_but_omitting_it_does_not() {
        let asked = Settings {
            data_strategy: Some(DataStrategy::Quick),
            ..Settings::default()
        };
        assert_eq!(
            patch(asked.clone(), &json!({"dataStrategy": null}))
                .unwrap()
                .data_strategy,
            None,
        );
        assert_eq!(
            patch(asked, &json!({"renderScale": 1}))
                .unwrap()
                .data_strategy,
            Some(DataStrategy::Quick),
        );
    }

    /// A launch that asks GitHub about itself is a launch doing something on
    /// the player's behalf that they did not ask for at that moment, so the
    /// answer to "may it" has to be theirs and has to start as no.
    #[test]
    fn nothing_is_checked_for_updates_until_a_player_says_so() {
        assert!(!Settings::default().auto_check_updates);
        assert_eq!(Settings::default().last_update_check_at, None);
        assert!(!parse(&json!({})).unwrap().auto_check_updates);
        let on = patch(Settings::default(), &json!({"autoCheckUpdates": true})).unwrap();
        assert!(on.auto_check_updates);
    }

    /// The stronger of the two, and the one a profile written before it existed
    /// has no opinion about — so its default has to be the cautious answer
    /// rather than "whatever the other switch says".
    #[test]
    fn nothing_installs_itself_until_a_player_says_so() {
        assert!(!Settings::default().auto_install_updates);
        assert!(!parse(&json!({})).unwrap().auto_install_updates);
        // The shape of every profile this build inherits: checking was asked
        // for once, installing was never on offer.
        let inherited = parse(&json!({"autoCheckUpdates": true})).unwrap();
        assert!(inherited.auto_check_updates);
        assert!(!inherited.auto_install_updates);

        let both = patch(inherited, &json!({"autoInstallUpdates": true})).unwrap();
        assert!(both.auto_install_updates);
        assert!(patch(both, &json!({"autoInstallUpdates": "yes"})).is_err());
    }

    /// The direction that makes this file a mirror rather than a second
    /// opinion. Sparkle's own update window can turn either switch on, and the
    /// settings panel has to show what it did — see [`crate::updater`] for why
    /// the updater is the one holding the truth.
    #[test]
    fn what_the_updater_decided_is_what_the_panel_reads_back() {
        let (_dir, path) = scratch("update-preferences");
        let first = UpdateStore::open(path.clone(), &Settings::default());
        let second = UpdateStore::open(path, &Settings::default());
        assert!(!first.get().auto_check_updates);
        assert!(!first.get().auto_install_updates);

        first.apply_preferences(Some(true), Some(false)).unwrap();
        assert!(second.get().auto_check_updates);
        assert!(!second.get().auto_install_updates);

        // Separate process-style stores reload under the cross-process lock;
        // the decision closure sees the latest pair and keeps both the external
        // reconciliation and JSON mirror in the same transaction.
        second
            .update_preferences(|current| {
                assert_eq!(current, (true, false));
                (current.0, true)
            })
            .unwrap();
        assert!(first.get().auto_check_updates);
        assert!(first.get().auto_install_updates);
        first.apply_preferences(Some(false), None).unwrap();
        assert!(!first.get().auto_check_updates);
        assert!(first.get().auto_install_updates);
        assert!(!second.get().auto_check_updates);
        assert!(second.get().auto_install_updates);
    }

    /// The two host-owned fields. A page that could write either could tell
    /// this build that a check had just happened, or that a client artifact had
    /// been warned about when it had not — and both are how a notice comes to
    /// be silently suppressed for good.
    #[test]
    fn a_page_cannot_write_the_hosts_own_bookkeeping() {
        let now = Settings::default();
        assert!(patch(now.clone(), &json!({"lastUpdateCheckAt": 1})).is_err());
        assert!(patch(now, &json!({"formatVersion": 1})).is_err());
        // Still read from the file, because a launch has to load what the
        // previous one wrote.
        assert_eq!(
            parse(&json!({"lastUpdateCheckAt": 1_700_000_000_u64}))
                .unwrap()
                .last_update_check_at,
            Some(1_700_000_000),
        );
    }

    /// The acknowledgement is compared for equality against this launch's
    /// client hash, so a value that cannot be one is a notice that comes back
    /// every launch with nothing to explain it.
    #[test]
    fn the_acknowledged_build_has_to_look_like_a_build() {
        let hash = "a".repeat(64);
        let seen = patch(
            Settings::default(),
            &json!({ "compatibilityNoticeSeenFor": hash }),
        )
        .unwrap();
        assert_eq!(seen.compatibility_notice_seen_for.as_deref(), Some(&*hash));
        // Cleared back to "warn me again", which is a value rather than an
        // omission.
        assert_eq!(
            patch(seen, &json!({"compatibilityNoticeSeenFor": null}))
                .unwrap()
                .compatibility_notice_seen_for,
            None,
        );

        for refused in [
            "",
            "ABCDEF",
            &"A".repeat(64),
            &"g".repeat(64),
            &"a".repeat(63),
        ] {
            assert!(
                patch(
                    Settings::default(),
                    &json!({ "compatibilityNoticeSeenFor": refused })
                )
                .is_err(),
                "{refused:?} is not a client sha256"
            );
        }
    }

    /// The launch reads one question off two switches, and the answer decides
    /// which client module is served. A stored master flag would be a third
    /// place for the same fact to live, and the one that could disagree.
    #[test]
    fn whether_the_tools_are_wanted_is_read_off_the_tools_themselves() {
        // The default profile wants them: the native cursor is on.
        assert!(Settings::default().enhancements_enabled());

        let off = patch(Settings::default(), &json!({"nativeCursor": false})).unwrap();
        assert!(!off.enhancements_enabled(), "no tool is on");
        let readout = patch(off.clone(), &json!({"targetReadout": true})).unwrap();
        assert!(readout.enhancements_enabled(), "one tool is enough");
        assert!(!off.target_readout, "the default readout is off");

        assert!(patch(Settings::default(), &json!({"nativeCursor": "yes"})).is_err());

        let (_dir, path) = scratch("tools");
        assert!(
            Store::open(path).get().native_cursor,
            "a profile that has never been written still gets the default",
        );
    }

    /// Written by the host and by nothing else, and readable by the next
    /// launch — which is the whole of what makes the check daily rather than
    /// per-launch.
    #[test]
    fn the_time_of_a_check_survives_the_launch_that_made_it() {
        let (_dir, path) = scratch("update-check");
        let store = UpdateStore::open(path.clone(), &Settings::default());
        assert_eq!(store.get().last_update_check_at, None);

        store.remember_check(1_700_000_000);
        assert_eq!(store.get().last_update_check_at, Some(1_700_000_000));
        assert_eq!(
            UpdateStore::open(path, &Settings::default())
                .get()
                .last_update_check_at,
            Some(1_700_000_000)
        );
    }

    #[test]
    fn what_was_saved_is_what_the_next_launch_reads() {
        let (_dir, path) = scratch("roundtrip");
        let store = Store::open(path.clone());
        assert_eq!(store.get(), Settings::default());

        let saved = store
            .apply(&json!({"dataStrategy": "full", "renderScale": 1}))
            .unwrap();
        assert_eq!(saved.data_strategy, Some(DataStrategy::Full));
        assert_eq!(Store::open(path).get(), saved);
    }

    #[test]
    fn scoped_profiles_share_only_application_update_preferences() {
        let (_profile_dir, profile_path) = scratch("scoped-profile");
        let (_other_dir, other_path) = scratch("scoped-other");
        let (_updates_dir, updates_path) = scratch("scoped-updates");
        let updates = Arc::new(UpdateStore::open(updates_path, &Settings::default()));
        let first = ScopedStore::new(Arc::new(Store::open(profile_path)), Arc::clone(&updates));
        first
            .apply(&json!({"renderScale": 1, "autoCheckUpdates": true}))
            .unwrap();

        let second = ScopedStore::new(Arc::new(Store::open(other_path)), updates);
        assert_eq!(first.get().render_scale, 1.0);
        assert_eq!(second.get().render_scale, 2.0);
        assert!(first.get().auto_check_updates);
        assert!(second.get().auto_check_updates);

        second.remember_update_check(1_700_000_000);
        assert_eq!(
            first.get().last_update_check_at,
            Some(1_700_000_000),
            "application update cadence is shared"
        );
    }

    #[test]
    fn default_profile_settings_and_global_updates_survive_separately() {
        let (_profile_dir, profile_path) = scratch("scoped-default");
        let (_updates_dir, updates_path) = scratch("scoped-default-updates");
        let updates = Arc::new(UpdateStore::open(
            updates_path.clone(),
            &Settings::default(),
        ));
        let scoped = ScopedStore::new(Arc::new(Store::open(profile_path.clone())), updates);
        let saved = scoped
            .apply(&json!({"renderScale": 1.5, "autoInstallUpdates": true}))
            .unwrap();
        assert_eq!(Store::open(profile_path).get().render_scale, 1.5);
        assert!(
            load_updates(&updates_path)
                .unwrap()
                .unwrap()
                .auto_install_updates
        );
        assert_eq!(scoped.get(), saved);
    }

    #[test]
    fn an_unreadable_file_is_kept_rather_than_overwritten() {
        let (_dir, path) = scratch("corrupt");
        fs::write(&path, b"{not json").unwrap();

        let store = Store::open(path.clone());
        assert_eq!(
            store.get(),
            Settings::default(),
            "a bad file must not block a launch"
        );

        let parent = path.parent().unwrap();
        let backups: Vec<_> = fs::read_dir(parent)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1, "the original must still exist");
        assert_eq!(fs::read(backups[0].path()).unwrap(), b"{not json");
    }

    #[test]
    fn only_the_newest_corrupt_backups_are_kept() {
        let (_dir, path) = scratch("prune");
        for stamp in [1u64, 5, 3, 4, 2] {
            fs::write(with_suffix(&path, &format!("corrupt-{stamp}")), b"x").unwrap();
        }
        // A name that is not one of ours, and one whose stamp is not a number.
        fs::write(path.with_file_name("notes.txt"), b"x").unwrap();
        fs::write(with_suffix(&path, "corrupt-later"), b"x").unwrap();

        prune_backups(&path);

        let mut left: Vec<String> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "notes.txt",
                "settings.json.corrupt-3",
                "settings.json.corrupt-4",
                "settings.json.corrupt-5",
                "settings.json.corrupt-later",
            ],
        );
    }
}
