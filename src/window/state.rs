//! What is saved between launches, and what a saved frame means today.
//!
//! Restoring a frame is not merely storing four numbers and setting them back.
//! The display the window was on may be gone, may have moved relative to the
//! others, or may have changed resolution, and a frame that made sense on
//! Friday can put the titlebar under the menu bar or entirely off-screen on
//! Monday — at which point the window cannot be dragged back, because the part
//! you drag it by is what is missing. So a saved frame is a *request*, and
//! [`fit`] decides what it means on the displays that exist now.
//!
//! What is stored is the last *normal* frame, never the zoomed or full-screen
//! one, plus which of the three the window was in. Storing the full-screen frame
//! would restore a window the size of the display and then leave it that size
//! when the player exits full screen, which is how a window ends up with no way
//! back to a usable size.

use std::path::Path;

use objc2_app_kit::NSScreen;
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use serde::{Deserialize, Serialize};

/// The shape this build writes. Same contract as `settings`: a version this
/// build does not know is refused rather than reinterpreted, and a refused file
/// costs a default window rather than a window placed from a shape nothing
/// understood.
const FORMAT: u32 = 1;

/// The window a first launch gets, before it has been anywhere.
const DEFAULT_SIZE: (f64, f64) = (1280.0, 800.0);

/// Never restore a window smaller than this, whatever the file says. Below it
/// the client's canvas is unusable and the launcher's buttons do not fit, and a
/// window nobody can read is indistinguishable from a window that did not open.
const MIN_SIZE: (f64, f64) = (800.0, 600.0);

/// Refuse a stored size outside this. Not a real display's limit — a bound past
/// which the number is evidence of a corrupt file rather than of a large
/// monitor.
const MAX_EDGE: f64 = 32_768.0;

/// How much of the work area a default window leaves clear, so it reads as a
/// window rather than as something that failed to go full screen.
const DEFAULT_MARGIN: f64 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Mode {
    Normal,
    Maximized,
    Fullscreen,
}

/// An AppKit frame: origin at the bottom-left, y increasing upwards.
///
/// Stored in AppKit's own coordinates rather than converted to a screen-relative
/// or top-left form, because every producer and consumer here is AppKit and a
/// conversion is one more place to have the sign wrong.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Bounds {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

impl Bounds {
    pub(super) fn from_rect(rect: NSRect) -> Self {
        Self {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        }
    }

    pub(super) fn to_rect(self) -> NSRect {
        NSRect::new(
            NSPoint::new(self.x, self.y),
            NSSize::new(self.width, self.height),
        )
    }

    /// Area shared with `other`. Zero when they do not touch, which is how a
    /// window on a display that has since been unplugged is recognised.
    fn overlap(self, other: Bounds) -> f64 {
        let width = (self.x + self.width).min(other.x + other.width) - self.x.max(other.x);
        let height = (self.y + self.height).min(other.y + other.height) - self.y.max(other.y);
        width.max(0.0) * height.max(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct State {
    pub(super) bounds: Bounds,
    pub(super) mode: Mode,
}

/// Every field optional so serde's type checking does the rejecting, and a
/// `formatVersion` from a later build is caught before any of it is believed.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
    format_version: Option<u32>,
    bounds: Option<Bounds>,
    mode: Option<Mode>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Written {
    format_version: u32,
    bounds: Bounds,
    mode: Mode,
}

/// Read a stored state, or nothing.
///
/// A file that cannot be read is removed rather than left to be re-read and
/// re-rejected on every launch: it has already failed to mean anything, and the
/// next save writes a version that does.
pub(super) fn load(path: &Path) -> Option<State> {
    let text = std::fs::read_to_string(path).ok()?;
    match parse(&text) {
        Ok(state) => Some(state),
        Err(reason) => {
            note!("[window] ignoring {}: {reason}", path.display());
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

fn parse(text: &str) -> Result<State, String> {
    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if let Some(version) = wire.format_version
        && version != FORMAT
    {
        return Err(format!("formatVersion {version} is not readable"));
    }
    let bounds = wire.bounds.ok_or_else(|| "no bounds".to_owned())?;
    let mode = wire.mode.ok_or_else(|| "no mode".to_owned())?;
    let sane = [bounds.x, bounds.y, bounds.width, bounds.height]
        .iter()
        .all(|value| value.is_finite());
    if !sane || bounds.width <= 0.0 || bounds.height <= 0.0 {
        return Err(format!("bounds {bounds:?} are not a rectangle"));
    }
    if bounds.width > MAX_EDGE || bounds.height > MAX_EDGE {
        return Err(format!("bounds {bounds:?} are implausibly large"));
    }
    Ok(State { bounds, mode })
}

/// Write `state`, atomically enough that a crash mid-write cannot leave a file
/// that parses as something else.
pub(super) fn save(path: &Path, state: State) {
    let written = Written {
        format_version: FORMAT,
        bounds: state.bounds,
        mode: state.mode,
    };
    let Ok(json) = serde_json::to_vec(&written) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, &json).is_ok() {
        let _ = std::fs::rename(&temporary, path);
    }
}

/// Place `state` on the displays that exist now.
///
/// The window goes to whichever work area it overlaps most. If it overlaps
/// none — the display it was on is gone — it is centred on `primary` at its
/// remembered size rather than being dragged to the nearest edge, because a
/// window that reappears in the middle of a screen reads as "the app opened"
/// and a window jammed into a corner reads as a bug.
pub(super) fn fit(state: State, areas: &[Bounds], primary: Bounds) -> State {
    let best = areas
        .iter()
        .map(|area| (*area, state.bounds.overlap(*area)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .filter(|(_, overlap)| *overlap > 0.0);
    let area = best.map_or(primary, |(area, _)| area);

    let width = state.bounds.width.max(MIN_SIZE.0).min(area.width);
    let height = state.bounds.height.max(MIN_SIZE.1).min(area.height);
    let (x, y) = match best {
        Some(_) => (
            state.bounds.x.clamp(area.x, area.x + area.width - width),
            state.bounds.y.clamp(area.y, area.y + area.height - height),
        ),
        None => (
            area.x + (area.width - width) / 2.0,
            area.y + (area.height - height) / 2.0,
        ),
    };
    State {
        bounds: Bounds {
            x: x.round(),
            y: y.round(),
            width: width.round(),
            height: height.round(),
        },
        mode: state.mode,
    }
}

/// The window a profile with no stored state gets: centred on the primary work
/// area, at the default size or as much of it as fits.
pub(super) fn default_state(primary: Bounds) -> State {
    let width = DEFAULT_SIZE
        .0
        .min((primary.width - DEFAULT_MARGIN).max(MIN_SIZE.0.min(primary.width)));
    let height = DEFAULT_SIZE
        .1
        .min((primary.height - DEFAULT_MARGIN).max(MIN_SIZE.1.min(primary.height)));
    State {
        bounds: Bounds {
            x: (primary.x + (primary.width - width) / 2.0).round(),
            y: (primary.y + (primary.height - height) / 2.0).round(),
            width: width.round(),
            height: height.round(),
        },
        mode: Mode::Normal,
    }
}

/// Every connected display's usable area, and the primary one's.
///
/// `visibleFrame`, not `frame`: it excludes the menu bar and the Dock, so a
/// window placed inside it is a window whose titlebar can be grabbed.
pub(super) fn work_areas(mtm: MainThreadMarker) -> (Vec<Bounds>, Bounds) {
    let screens = NSScreen::screens(mtm);
    let areas: Vec<Bounds> = screens
        .iter()
        .map(|screen| Bounds::from_rect(screen.visibleFrame()))
        .collect();
    // `screens[0]` is the screen with the menu bar, which is what "primary"
    // means for placement. A machine with no screens at all is not a machine
    // running this, but it costs one line not to panic on it.
    let primary = areas.first().copied().unwrap_or(Bounds {
        x: 0.0,
        y: 0.0,
        width: DEFAULT_SIZE.0,
        height: DEFAULT_SIZE.1,
    });
    (areas, primary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: f64, y: f64, width: f64, height: f64) -> Bounds {
        Bounds {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_window_on_a_display_that_is_gone_comes_back_in_the_middle_of_one_that_is_not() {
        let laptop = area(0.0, 0.0, 1512.0, 916.0);
        // Where a second display used to be, far to the right.
        let stored = State {
            bounds: area(3000.0, 100.0, 1200.0, 800.0),
            mode: Mode::Normal,
        };

        let fitted = fit(stored, &[laptop], laptop);
        assert!(
            fitted.bounds.overlap(laptop) > 0.0,
            "must land somewhere visible: {fitted:?}"
        );
        // Centred, not shoved against the right edge.
        assert_eq!(fitted.bounds.x, ((1512.0 - 1200.0) / 2.0f64).round());
        assert_eq!(fitted.bounds.width, 1200.0);
        assert_eq!(fitted.mode, Mode::Normal);
    }

    #[test]
    fn a_window_that_still_overlaps_keeps_where_it_was() {
        let laptop = area(0.0, 0.0, 1512.0, 916.0);
        let stored = State {
            bounds: area(200.0, 50.0, 1000.0, 700.0),
            mode: Mode::Maximized,
        };
        let fitted = fit(stored, &[laptop], laptop);
        assert_eq!(fitted.bounds, stored.bounds);
        // The mode survives the fit: a maximized window comes back maximized
        // over the frame it would return to.
        assert_eq!(fitted.mode, Mode::Maximized);
    }

    #[test]
    fn a_window_bigger_than_the_display_is_cut_down_to_it() {
        let small = area(0.0, 0.0, 1280.0, 720.0);
        let stored = State {
            bounds: area(-200.0, -100.0, 2560.0, 1440.0),
            mode: Mode::Normal,
        };
        let fitted = fit(stored, &[small], small);
        assert_eq!(fitted.bounds.width, 1280.0);
        assert_eq!(fitted.bounds.height, 720.0);
        assert_eq!(fitted.bounds.x, 0.0);
        assert_eq!(fitted.bounds.y, 0.0);
    }

    #[test]
    fn the_display_with_the_most_of_the_window_wins() {
        let left = area(0.0, 0.0, 1512.0, 916.0);
        let right = area(1512.0, 0.0, 2560.0, 1440.0);
        // Mostly on the right-hand display.
        let stored = State {
            bounds: area(1400.0, 100.0, 1000.0, 700.0),
            mode: Mode::Normal,
        };
        let fitted = fit(stored, &[left, right], left);
        assert_eq!(fitted.bounds.x, 1512.0, "clamped into the right display");
        assert!(fitted.bounds.overlap(right) > fitted.bounds.overlap(left));
    }

    #[test]
    fn a_file_that_cannot_be_believed_is_refused_rather_than_repaired() {
        assert!(parse("{}").is_err(), "no bounds and no mode");
        assert!(
            parse(r#"{"formatVersion":2,"bounds":{"x":0,"y":0,"width":800,"height":600},"mode":"normal"}"#)
                .is_err(),
            "a later format is not reinterpreted"
        );
        assert!(
            parse(r#"{"bounds":{"x":0,"y":0,"width":0,"height":600},"mode":"normal"}"#).is_err(),
            "a zero-width window is not a window"
        );
        assert!(
            parse(r#"{"bounds":{"x":0,"y":0,"width":1e9,"height":600},"mode":"normal"}"#).is_err(),
            "an implausible edge is corruption, not a monitor"
        );
        assert!(
            parse(r#"{"bounds":{"x":0,"y":0,"width":800,"height":600},"mode":"minimized"}"#)
                .is_err(),
            "a mode this build does not have is not silently a default"
        );
        // No `formatVersion` is the shape the first build wrote, and it is the
        // same shape, so it still restores a window.
        let ok =
            parse(r#"{"bounds":{"x":10,"y":20,"width":800,"height":600},"mode":"fullscreen"}"#)
                .expect("v0 and v1 agree");
        assert_eq!(ok.mode, Mode::Fullscreen);
        assert_eq!(ok.bounds.x, 10.0);
    }

    #[test]
    fn what_is_written_is_what_comes_back() {
        let dir = std::env::temp_dir().join(format!(
            "gwnative-window-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("window.json");

        assert_eq!(load(&path), None, "nothing stored yet");
        let state = State {
            bounds: area(12.0, 34.0, 1280.0, 800.0),
            mode: Mode::Maximized,
        };
        save(&path, state);
        assert_eq!(load(&path), Some(state));

        // And a file that cannot be read does not survive to be re-read.
        std::fs::write(&path, b"{").unwrap();
        assert_eq!(load(&path), None);
        assert!(!path.exists(), "a refused file is removed, not kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_default_window_fits_the_display_it_is_centred_on() {
        // A 13-inch laptop: the default 1280x800 fits with room to spare.
        let laptop = area(0.0, 0.0, 1512.0, 916.0);
        let state = default_state(laptop);
        assert_eq!(state.mode, Mode::Normal);
        assert_eq!(state.bounds.width, 1280.0);
        assert_eq!(state.bounds.height, 800.0);
        assert!(state.bounds.x > 0.0 && state.bounds.y > 0.0);

        // A display smaller than the default: the window shrinks to fit rather
        // than hanging off the bottom.
        let tiny = area(0.0, 0.0, 1024.0, 640.0);
        let state = default_state(tiny);
        assert!(state.bounds.width <= tiny.width, "{state:?}");
        assert!(state.bounds.height <= tiny.height, "{state:?}");
        assert!(state.bounds.x >= 0.0 && state.bounds.y >= 0.0, "{state:?}");
    }
}
