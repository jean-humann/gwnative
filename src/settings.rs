//! What the player chose, kept across launches.
//!
//! One small JSON file in the support directory. Four fields, and every one of
//! them is read by something that already exists here: `renderScale` is what
//! the client asks for through `emscripten_get_device_pixel_ratio`, `touchMode`
//! selects which gesture translation `input.js` installs, `showDiagnostics`
//! opens the log pane at boot, and `dataStrategy` records the answer to the
//! launcher's one question. Nothing is stored for a feature this app does not
//! have — a settings file whose fields nothing reads is a migration burden that
//! never bought anything.
//!
//! The reader is deliberately lopsided: an unknown *field* is ignored, an
//! unknown *value* is refused. A file written by a later build should still
//! yield everything this build understands, but a `touchMode` of `"maybe"`
//! means the file cannot be trusted about touch, and quietly substituting a
//! default there would leave the player's input behaving in a way no setting of
//! theirs explains. A `formatVersion` this build does not know is refused
//! outright rather than reinterpreted, and [`Store::open`] then moves the file
//! aside intact instead of overwriting a shape it could not read.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TouchMode {
    Off,
    Dbltap,
    Translate,
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

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub render_scale: f64,
    pub touch_mode: TouchMode,
    pub show_diagnostics: bool,
    pub data_strategy: Option<DataStrategy>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // The client draws at the ratio it is given and the compositor
            // scales the result. 2 is a Retina panel's native ratio, so it is
            // the one that does not resample.
            render_scale: 2.0,
            touch_mode: TouchMode::Off,
            show_diagnostics: false,
            data_strategy: None,
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
}

/// The names a patch may carry. `formatVersion` is not among them: it describes
/// the file, and a page that could set it could make the next launch unable to
/// read its own settings.
const PATCHABLE: [&str; 4] = [
    "renderScale",
    "touchMode",
    "showDiagnostics",
    "dataStrategy",
];

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
        *self.current.lock().unwrap()
    }

    /// Fold a patch in and write the result.
    ///
    /// The in-memory copy is updated only once the file is on disk, so a page
    /// that is told its change was saved can rely on the next launch agreeing.
    pub fn apply(&self, raw: &serde_json::Value) -> Result<Settings, String> {
        let mut current = self.current.lock().unwrap();
        let next = patch(*current, raw)?;
        if next != *current {
            save(&self.path, &next).map_err(|e| format!("could not save settings: {e}"))?;
            *current = next;
        }
        Ok(next)
    }
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
    backups.sort_unstable_by(|left, right| right.0.cmp(&left.0));
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

    #[test]
    fn an_empty_object_is_every_default() {
        assert_eq!(parse(&json!({})).unwrap(), Settings::default());
    }

    #[test]
    fn an_unknown_field_is_ignored_but_an_unknown_value_is_not() {
        let forward =
            parse(&json!({"touchMode": "augment", "tuchMode": "off", "nativeCursor": true}));
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
        };
        let after = patch(current, &json!({"showDiagnostics": false})).unwrap();
        assert_eq!(
            after,
            Settings {
                show_diagnostics: false,
                ..current
            }
        );
        assert!(patch(current, &json!({"renderscale": 1})).is_err());
        assert!(patch(current, &json!({"formatVersion": 1})).is_err());
    }

    #[test]
    fn null_clears_the_data_strategy_but_omitting_it_does_not() {
        let asked = Settings {
            data_strategy: Some(DataStrategy::Quick),
            ..Settings::default()
        };
        assert_eq!(
            patch(asked, &json!({"dataStrategy": null}))
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
