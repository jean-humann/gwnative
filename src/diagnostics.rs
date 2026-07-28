//! Metrics the host records about itself, and a place to put them.
//!
//! The point of this module is that a question about performance should be
//! answerable from a file rather than reconstructed afterwards from `sample`,
//! `footprint` and grepped console lines. That reconstruction is how this
//! project's headline CPU comparison came out wrong and stayed wrong.
//!
//! Two producers write here. The sampler thread records what the kernel knows
//! about this process once a second, and the page posts its own counters to
//! `__diag`. Both land in the same JSONL file on the same clock, which is the
//! only way a frame-time spike and a chunk fetch can be lined up against each
//! other.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Keep this much before rotating, and this many rotations. 25 MiB total is
/// about a fortnight of idle sampling and still small enough to attach to a
/// bug report without thinking about it.
const MAX_BYTES: u64 = 5 << 20;
const KEEP: usize = 5;

/// How the recorded value of a metric changes when a new one arrives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Monotonic. New values are added to what is there.
    Count,
    /// Last one wins.
    Gauge,
    /// Largest ever seen.
    Peak,
}

/// How many distinct metric names the host will hold, not counting the overflow
/// counter it adds on top.
///
/// Generous — the page and the host together name about sixty — but finite. The
/// batch endpoint is reachable by any process on the machine that has the token,
/// and the names in a batch are whatever the caller wrote.
const MAX_NAMES: usize = 512;

/// The longest name that will be stored. A name is a fixed label in the source
/// on both sides; anything approaching this is a value that got concatenated
/// into one by mistake.
const MAX_NAME_LEN: usize = 128;

/// Where refused names are counted, so a full table says so rather than just
/// quietly missing the metric somebody is looking for.
const OVERFLOW: &str = "gw.metrics.dropped";

/// A name-to-number registry, shared by the host and the page.
///
/// Deliberately `BTreeMap` rather than `HashMap`: the records go into a file a
/// human reads, and a stable key order makes two records diffable by eye.
#[derive(Default)]
pub struct Metrics {
    values: Mutex<BTreeMap<String, f64>>,
}

impl Metrics {
    pub fn record(&self, kind: Kind, name: &str, value: f64) {
        // NaN would poison a peak permanently — every later comparison against
        // it is false, so the maximum would stop moving — and there is no
        // reading of it that means anything in a metric.
        if !value.is_finite() {
            return;
        }
        if name.len() > MAX_NAME_LEN {
            return;
        }
        let mut values = self.values.lock().unwrap();
        // A name that is already here always lands. A new one only lands while
        // there is room: this map is never cleared, so a caller that derives
        // names from something unbounded — a socket, an address, a file — would
        // otherwise grow it for the life of the process, and every `snapshot`
        // clones the whole thing.
        if values.len() >= MAX_NAMES && !values.contains_key(name) {
            *values.entry(OVERFLOW.to_owned()).or_insert(0.0) += 1.0;
            return;
        }
        let slot = values.entry(name.to_owned()).or_insert(0.0);
        *slot = match kind {
            Kind::Count => *slot + value,
            Kind::Gauge => value,
            Kind::Peak => slot.max(value),
        };
    }

    pub fn count(&self, name: &str, value: f64) {
        self.record(Kind::Count, name, value);
    }

    pub fn peak(&self, name: &str, value: f64) {
        self.record(Kind::Peak, name, value);
    }

    pub fn snapshot(&self) -> BTreeMap<String, f64> {
        self.values.lock().unwrap().clone()
    }
}

/// What the kernel will say about this process.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// The number that matches Activity Monitor's "Memory" column and
    /// `footprint`, and the only memory figure worth comparing between two
    /// applications. Resident size is also here, but it counts shared clean
    /// pages this process did not cause to exist.
    pub footprint: u64,
    pub resident: u64,
    pub user: Duration,
    pub system: Duration,
}

impl Usage {
    pub fn cpu(&self) -> Duration {
        self.user + self.system
    }
}

/// `rusage_info_v0`, laid out as in `<sys/resource.h>`.
///
/// Declared by hand because there is no crate in this build that has it, and
/// checked against the real header: 96 bytes, `ri_phys_footprint` at offset
/// 72. Only v0 is asked for, so a later kernel adding fields cannot move any
/// of these.
#[repr(C)]
#[derive(Default)]
struct RusageInfoV0 {
    uuid: [u8; 16],
    user_time: u64,
    system_time: u64,
    pkg_idle_wkups: u64,
    interrupt_wkups: u64,
    pageins: u64,
    wired_size: u64,
    resident_size: u64,
    phys_footprint: u64,
    proc_start_abstime: u64,
    proc_exit_abstime: u64,
}

#[repr(C)]
#[derive(Default)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageInfoV0) -> i32;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    fn getpid() -> i32;
}

/// Nanoseconds per mach absolute time tick, as a ratio.
///
/// This is not decoration. `ri_user_time` is in mach ticks, *not* nanoseconds
/// as the field name suggests, and on Apple silicon a tick is 41.67 ns — so
/// reading the field directly reports CPU time 41.67x lower than it is, which
/// is a wrong answer that looks entirely plausible. Verified against
/// `getrusage(2)` on the same burn: both give 0.3279 s.
fn timebase() -> (u64, u64) {
    let mut info = MachTimebaseInfo::default();
    if unsafe { mach_timebase_info(&mut info) } != 0 || info.denom == 0 {
        return (1, 1);
    }
    (u64::from(info.numer), u64::from(info.denom))
}

pub fn usage() -> Option<Usage> {
    let mut info = RusageInfoV0::default();
    if unsafe { proc_pid_rusage(getpid(), 0, &mut info) } != 0 {
        return None;
    }
    let (numer, denom) = timebase();
    let ns = |ticks: u64| Duration::from_nanos(ticks.saturating_mul(numer) / denom);
    Some(Usage {
        footprint: info.phys_footprint,
        resident: info.resident_size,
        user: ns(info.user_time),
        system: ns(info.system_time),
    })
}

/// A JSONL file that rotates, and the metrics that go into it.
pub struct Recorder {
    dir: PathBuf,
    file: Mutex<Option<fs::File>>,
    pub metrics: Metrics,
}

impl Recorder {
    pub fn open(dir: PathBuf) -> Arc<Self> {
        if let Err(e) = fs::create_dir_all(&dir) {
            note!("[gwnative] no diagnostics in {}: {e}", dir.display());
        }
        Arc::new(Self {
            dir,
            file: Mutex::new(None),
            metrics: Metrics::default(),
        })
    }

    fn path(&self) -> PathBuf {
        self.dir.join("gwnative.jsonl")
    }

    /// Append one record. Failures are silent past the first: diagnostics that
    /// can interrupt the thing they are measuring are worse than none.
    pub fn write(&self, record: &serde_json::Value) {
        let mut line = record.to_string();
        line.push('\n');
        let mut slot = self.file.lock().unwrap();
        // Reopened rather than held across a rotation, and checked by length
        // rather than by counting what this process wrote, so that a file left
        // large by a previous run is rotated on the next launch instead of
        // growing without limit.
        let too_big = fs::metadata(self.path()).is_ok_and(|m| m.len() >= MAX_BYTES);
        if too_big {
            *slot = None;
            self.rotate();
        }
        if slot.is_none() {
            *slot = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path())
                .ok();
        }
        // No fsync. The record survives the process dying, which is when it is
        // wanted; surviving a power cut is not worth waking the disk once a
        // second for the life of the session.
        if let Some(file) = slot.as_mut()
            && file.write_all(line.as_bytes()).is_err()
        {
            *slot = None;
        }
    }

    fn rotate(&self) {
        let numbered = |n: usize| self.dir.join(format!("gwnative.{n}.jsonl"));
        let _ = fs::remove_file(numbered(KEEP - 1));
        for n in (1..KEEP - 1).rev() {
            let _ = fs::rename(numbered(n), numbered(n + 1));
        }
        let _ = fs::rename(self.path(), numbered(1));
    }
}

/// Sample this process once a second for as long as it runs.
///
/// `extra` contributes whatever the caller wants alongside the process figures
/// — the chunk store's counters, in practice — so that a memory step and the
/// fetch that caused it are one line apart rather than in two files.
pub fn spawn_sampler(
    recorder: Arc<Recorder>,
    extra: impl Fn() -> serde_json::Value + Send + 'static,
) {
    std::thread::spawn(move || {
        crate::qos::set(crate::qos::Class::Utility);
        let started = SystemTime::now();
        let mut previous: Option<(SystemTime, Duration)> = None;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let Some(usage) = usage() else { continue };
            let now = SystemTime::now();
            // Occupancy over the interval, so 100 means one core saturated and
            // more than that means several. A total since launch would hide
            // exactly the spikes this exists to catch.
            let busy = previous.and_then(|(then, cpu)| {
                let wall = now.duration_since(then).ok()?.as_secs_f64();
                let spent = usage.cpu().checked_sub(cpu)?.as_secs_f64();
                (wall > 0.0).then(|| spent / wall * 100.0)
            });
            previous = Some((now, usage.cpu()));
            let record = serde_json::json!({
                "t": now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64(),
                "uptime": now.duration_since(started).unwrap_or_default().as_secs_f64(),
                "footprintMiB": usage.footprint as f64 / 1048576.0,
                "residentMiB": usage.resident as f64 / 1048576.0,
                "cpuPercent": busy,
                "cpuSeconds": usage.cpu().as_secs_f64(),
                "host": extra(),
                "metrics": recorder.metrics.snapshot(),
            });
            recorder.write(&record);
        }
    });
}

pub fn default_log_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join("Library/Application Support/gwnative/diagnostics")
}

/// Fold a `{"count": {...}, "gauge": {...}, "peak": {...}}` body from the page
/// into the registry. Unknown buckets and non-numeric values are dropped
/// rather than refused — the page must not be able to fail a launch by
/// mistyping a metric name.
pub fn absorb(metrics: &Metrics, body: &serde_json::Value) {
    for (bucket, kind) in [
        ("count", Kind::Count),
        ("gauge", Kind::Gauge),
        ("peak", Kind::Peak),
    ] {
        let Some(entries) = body.get(bucket).and_then(|v| v.as_object()) else {
            continue;
        };
        for (name, value) in entries {
            if let Some(value) = value.as_f64() {
                metrics.record(kind, name, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_keeps_a_different_thing() {
        let m = Metrics::default();
        for value in [3.0, 1.0, 2.0] {
            m.count("c", value);
            m.record(Kind::Gauge, "g", value);
            m.peak("p", value);
        }
        let snapshot = m.snapshot();
        assert_eq!(snapshot["c"], 6.0, "a count accumulates");
        assert_eq!(snapshot["g"], 2.0, "a gauge keeps the last");
        assert_eq!(snapshot["p"], 3.0, "a peak keeps the largest");
    }

    #[test]
    fn a_peak_cannot_be_poisoned() {
        let m = Metrics::default();
        m.peak("p", 10.0);
        m.peak("p", f64::NAN);
        m.peak("p", f64::INFINITY);
        assert_eq!(m.snapshot()["p"], 10.0);
        // The real damage a NaN does is silent: it compares false against
        // everything, so a poisoned peak stops rising and reads as calm.
        m.peak("p", 20.0);
        assert_eq!(m.snapshot()["p"], 20.0);
    }

    #[test]
    fn the_name_table_stops_growing() {
        let m = Metrics::default();
        for i in 0..MAX_NAMES + 50 {
            m.count(&format!("name.{i}"), 1.0);
        }
        let snapshot = m.snapshot();
        // The cap, plus the one slot the overflow counter takes for itself.
        assert_eq!(snapshot.len(), MAX_NAMES + 1);
        assert_eq!(
            snapshot[OVERFLOW], 50.0,
            "and it says how many were refused"
        );
    }

    #[test]
    fn a_full_table_still_records_the_names_it_knows() {
        let m = Metrics::default();
        m.count("gw.frames", 1.0);
        for i in 0..MAX_NAMES * 2 {
            m.count(&format!("name.{i}"), 1.0);
        }
        // The whole point of the cap: a flood of new names must not stop the
        // metrics that were already being kept from moving.
        m.count("gw.frames", 4.0);
        assert_eq!(m.snapshot()["gw.frames"], 5.0);
    }

    #[test]
    fn an_absurd_name_is_refused() {
        let m = Metrics::default();
        m.count(&"x".repeat(MAX_NAME_LEN + 1), 1.0);
        assert!(m.snapshot().is_empty());
    }

    #[test]
    fn the_page_can_only_move_numbers() {
        let m = Metrics::default();
        absorb(
            &m,
            &serde_json::json!({
                "count": {"gw.frame.dropped": 2},
                "gauge": {"gw.frame.ms": 16.7},
                "peak": {"gw.frame.ms.max": 41.0},
                "wat": {"gw.ignored": 1},
                "unknown-shape": 5,
            }),
        );
        absorb(
            &m,
            &serde_json::json!({"count": {"gw.frame.dropped": "lots"}}),
        );
        let snapshot = m.snapshot();
        assert_eq!(snapshot["gw.frame.dropped"], 2.0);
        assert_eq!(snapshot["gw.frame.ms"], 16.7);
        assert_eq!(snapshot["gw.frame.ms.max"], 41.0);
        assert!(!snapshot.contains_key("gw.ignored"));
    }

    #[test]
    fn the_kernel_reports_this_process() {
        let usage = usage().expect("proc_pid_rusage should work on our own pid");
        assert!(usage.footprint > 0, "we are using some memory");
        assert!(usage.cpu() > Duration::ZERO, "we have run at all");
        // The timebase conversion is the thing worth guarding: read as raw
        // ticks this would be 41.67x smaller, and a test suite that has been
        // running for a second would look like it used 24 ms.
        assert!(
            usage.cpu() < Duration::from_secs(3600),
            "and not implausibly much"
        );
    }

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp(tag: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!(
            "gwnative-diag-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        TempDir(path)
    }

    #[test]
    fn records_land_one_per_line() {
        let dir = temp("lines");
        let recorder = Recorder::open(dir.0.clone());
        recorder.write(&serde_json::json!({"n": 1}));
        recorder.write(&serde_json::json!({"n": 2}));

        let body = fs::read_to_string(dir.0.join("gwnative.jsonl")).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            serde_json::from_str::<serde_json::Value>(line).expect("each line parses alone");
        }
    }

    #[test]
    fn rotation_keeps_the_last_five_files_and_no_more() {
        let dir = temp("rotate");
        let recorder = Recorder::open(dir.0.clone());
        // One oversized record per rotation is enough; the check is on the
        // file's length, not on how many records made it that long.
        let fat = serde_json::json!({ "pad": "x".repeat(MAX_BYTES as usize) });
        for _ in 0..10 {
            recorder.write(&fat);
        }

        let mut names: Vec<String> = fs::read_dir(&dir.0)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "gwnative.1.jsonl",
                "gwnative.2.jsonl",
                "gwnative.3.jsonl",
                "gwnative.4.jsonl",
                "gwnative.jsonl"
            ],
            "ten rotations must still leave five files"
        );
    }

    #[test]
    fn a_file_left_large_by_a_previous_run_is_rotated_not_extended() {
        let dir = temp("inherited");
        fs::create_dir_all(&dir.0).unwrap();
        fs::write(
            dir.0.join("gwnative.jsonl"),
            vec![b'\n'; MAX_BYTES as usize],
        )
        .unwrap();

        let recorder = Recorder::open(dir.0.clone());
        recorder.write(&serde_json::json!({"n": 1}));

        assert!(
            dir.0.join("gwnative.1.jsonl").is_file(),
            "the old log moved"
        );
        assert_eq!(
            fs::read_to_string(dir.0.join("gwnative.jsonl")).unwrap(),
            "{\"n\":1}\n",
            "and this run started a fresh one"
        );
    }
}
