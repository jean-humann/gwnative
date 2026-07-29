//! Whether the project has published a build newer than this one.
//!
//! Asked, never volunteered. An application that phones a server on every start
//! to ask about itself is doing something the player did not request, and the
//! answer is never urgent enough to justify it — so the Check for Updates item
//! is the request, and nothing else here fires by itself.
//!
//! There is one exception and it is one the player made: `autoCheckUpdates`,
//! off in a fresh profile, turns launches into askers. Even then the rules stay
//! the ones above. [`due`] holds it to once a day rather than once a launch, and
//! [`interrupts`] lets only [`Notice::Available`] reach the screen — being told
//! at boot that a check failed, or that nothing has changed, is the volunteering
//! this module exists not to do. The other two outcomes go to the log, where
//! somebody looking for them can find them.
//!
//! The answer has three shapes, never two. "We could not tell" is not "you are
//! up to date", and the difference matters most exactly when the check is
//! failing — a player told they are current by a request that never left the
//! machine will not look again. So the failures are a closed vocabulary with a
//! sentence each, and no free text from the network reaches the screen: the
//! version shown is re-rendered from the numbers this file parsed, not passed
//! through from GitHub's answer.
//!
//! SemVer only, in the shapes the project's own tags take: `X.Y.Z`, optionally
//! `-alpha.N` / `-beta.N` / `-rc.N`, optionally written with a leading `v`.
//! Nothing here knows what a year or a month is, so a CalVer scheme expressed in
//! SemVer syntax parses here without this file being told about it.

use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::transport;

/// Where the project lives, taken from `Cargo.toml` rather than written here.
///
/// Empty until the package declares a `repository`, and both things that would
/// use it — the Help menu's website item and the update check — are left out
/// while it is. A build that offers to show the project website and then shows
/// nothing, or worse shows someone else's repository because the URL was
/// guessed, is worse than a Help menu with one item in it.
pub const PROJECT_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// This build's own version, as `Cargo.toml` declares it.
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Long enough that leaning on the button is one request, short enough that a
/// player who has just been told to update and wants to re-check after
/// installing is not made to wait an hour.
const FRESH: Duration = Duration::from_secs(10 * 60);

/// The request is a courtesy; a slow one has already failed at being one.
const TIMEOUT: Duration = Duration::from_secs(5);

/// How long an automatic check waits before the next launch may ask again.
///
/// A day, in seconds. [`FRESH`] cannot do this job: it is an in-process cache
/// that a quit forgets, so a player who opens and closes the game six times in
/// an evening would make six requests. This one is read from the settings file,
/// which is the only thing in this application that remembers anything across a
/// launch.
const AUTOMATIC_INTERVAL: u64 = 24 * 60 * 60;

/// Whether this launch should ask GitHub about itself without being told to.
///
/// `last` is [`crate::settings::Settings::last_update_check_at`] and `now` is
/// [`crate::settings::now`]; both are seconds since the epoch, and both are
/// passed in rather than read here so that the rule is a function of three
/// numbers and can be exercised without a clock or a profile.
///
/// A clock that has gone backwards — a corrected machine, a restored profile,
/// the 0 that `now` returns when the clock will not answer — asks. The failure
/// mode of asking early is one extra request; the failure mode of treating a
/// future timestamp as recent is a check that never runs again on that profile.
pub fn due(auto: bool, last: Option<u64>, now: u64) -> bool {
    auto && last.is_none_or(|last| now < last || now - last >= AUTOMATIC_INTERVAL)
}

/// Whether an answer nobody asked for has earned a modal.
///
/// Only the one that says something the player can act on. `Current` is the
/// application interrupting to say nothing happened, and `Unknown` is it
/// interrupting to say it failed at something they did not ask for — both are
/// log lines. A *requested* check shows all three, because there a silence
/// would be the menu item doing nothing.
pub fn interrupts(notice: &Notice) -> bool {
    matches!(notice, Notice::Available { .. })
}

/// GitHub pages releases, and orders the page by publication activity rather
/// than by version. One page of a hundred is what it takes for the newest
/// *version* to be on the page that comes back — and a project with more than a
/// hundred releases whose newest is not among the last hundred published has a
/// stranger problem than this file can solve.
const PER_PAGE: usize = 100;

const HEADERS: &[(&str, &str)] = &[
    ("accept", "application/vnd.github+json"),
    ("x-github-api-version", "2022-11-28"),
    // Not optional: GitHub answers a request without one with 403, which would
    // arrive here as a rate limit and be wrong in a way that looks plausible.
    (
        "user-agent",
        concat!("gwnative/", env!("CARGO_PKG_VERSION")),
    ),
];

/// Which pre-release run a version belongs to, in the order they are published.
///
/// The order is the declaration order, which is what makes the derived
/// comparison below correct: every prerelease of `1.2.0` precedes `1.2.0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Channel {
    Alpha,
    Beta,
    Rc,
    Stable,
}

/// A version this project could have published.
///
/// Field order is comparison order — major, minor, patch, channel, sequence —
/// so the derived `Ord` is SemVer precedence for the subset of SemVer this
/// project uses. Nothing else may be inserted between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    channel: Channel,
    /// The prerelease's own counter. Always 0 on a stable release, which has
    /// none — and 0 is below every real prerelease sequence, which is why a
    /// stable release cannot be ordered by it.
    sequence: u64,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            major,
            minor,
            patch,
            channel,
            sequence,
        } = self;
        write!(f, "{major}.{minor}.{patch}")?;
        let run = match channel {
            Channel::Alpha => "alpha",
            Channel::Beta => "beta",
            Channel::Rc => "rc",
            Channel::Stable => return Ok(()),
        };
        write!(f, "-{run}.{sequence}")
    }
}

/// One numeric identifier: digits, no sign, no leading zero, and small enough
/// to be a number.
///
/// The leading-zero rule is SemVer's and it earns its place here: without it
/// `2026.07.01` parses as 2026.7.1, and a release named for July would compare
/// as older than one named for the tenth month of some other year. Failing is
/// the honest outcome — the tag is not a version this project publishes.
fn identifier(field: &str) -> Option<u64> {
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if field.len() > 1 && field.starts_with('0') {
        return None;
    }
    // A run of digits too long for `u64` fails here rather than saturating into
    // a value two different tags would share.
    field.parse().ok()
}

/// `None` for any string this project would not publish as a release.
pub fn parse(tag: &str) -> Option<Version> {
    // Tags carry the `v`; the version inside the package does not.
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    // Split on the first `-` only: everything after it is the prerelease, and
    // `1.2.3-alpha-1` has to fail rather than quietly read as `alpha`.
    let (core, prerelease) = match tag.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (tag, None),
    };
    let mut fields = core.split('.');
    let major = identifier(fields.next()?)?;
    let minor = identifier(fields.next()?)?;
    let patch = identifier(fields.next()?)?;
    if fields.next().is_some() {
        return None;
    }
    let (channel, sequence) = match prerelease {
        None => (Channel::Stable, 0),
        Some(prerelease) => {
            let (run, sequence) = prerelease.split_once('.')?;
            let channel = match run {
                "alpha" => Channel::Alpha,
                "beta" => Channel::Beta,
                "rc" => Channel::Rc,
                // Including build metadata: `1.2.3+deadbeef` is a version, but
                // it is not one this project's release workflow publishes, so
                // nothing here should claim to know how it orders.
                _ => return None,
            };
            (channel, identifier(sequence)?)
        }
    };
    Some(Version {
        major,
        minor,
        patch,
        channel,
        sequence,
    })
}

/// Why a check could not answer. One sentence each, and no other reasons: a
/// failure that does not fit one of these is a bug in the caller, not a new
/// kind of weather.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// This build did not come from the project's release process — either it
    /// declares no repository, or its own version is not a shape the workflow
    /// publishes. Either way there is nothing to compare against.
    Unsupported,
    /// The request did not come back. Offline, DNS, TLS and timeout all land
    /// here: CFNetwork describes them in the user's own language, and branching
    /// on a localised string would be branching on which country the player is
    /// in. The detail is in the log, where it is diagnosis rather than sentence.
    Unreachable,
    /// GitHub is refusing further anonymous requests from this address for now.
    RateLimited,
    /// GitHub answered, with something other than the list.
    Server,
    /// GitHub answered with the list, and it was not one.
    Unreadable,
}

impl Reason {
    /// What to tell the player. Each ends by saying what they can do about it,
    /// including when the answer is nothing.
    pub fn sentence(self) -> &'static str {
        match self {
            Self::Unsupported => {
                "This build did not come from the project's release process, so there is \
                 no published version to compare it against."
            }
            Self::Unreachable => {
                "GitHub could not be reached. Check the network connection and try again."
            }
            Self::RateLimited => {
                "GitHub is refusing further requests from this network for the moment. \
                 Try again in a few minutes."
            }
            Self::Server => "GitHub answered with an error. Try again in a few minutes.",
            Self::Unreadable => {
                "GitHub's answer could not be read. The diagnostics log has the detail."
            }
        }
    }
}

/// Three states, never two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// Something newer is published, and this install is allowed to have it.
    Available { current: String, latest: String },
    /// Nothing newer is published that this install is allowed to have.
    Current { current: String },
    /// The question was not answered.
    Unknown(Reason),
}

/// What a player is told, and the one thing they can do about it.
///
/// Split out of the alert that shows it because an AppKit modal is not something
/// a test can read, and these three sentences are the entire feature as far as
/// anyone using it is concerned. What is left next door is the twenty lines that
/// put a string in a window.
pub struct Wording {
    pub title: &'static str,
    pub detail: String,
    /// Where the newer build can be got, when there is one — and `None`
    /// otherwise, which is also what makes the alert a one-button alert. This
    /// build cannot install its own replacement, so what is offered is the page
    /// the download is on; a button named for that rather than "Update" is the
    /// difference between telling the truth and opening a browser after
    /// promising not to.
    pub releases: Option<String>,
}

/// Turn an answer into the sentences for it.
pub fn wording(notice: &Notice) -> Wording {
    match notice {
        Notice::Available { current, latest } => Wording {
            title: "A newer version is available",
            detail: format!("This is Guild Wars {current}. Version {latest} has been published."),
            releases: releases_url(),
        },
        Notice::Current { current } => Wording {
            title: "Guild Wars is up to date",
            detail: format!("Version {current} is the newest one published."),
            releases: None,
        },
        // Never "up to date". A player told they are current by a request that
        // never left the machine has been given the one answer that stops them
        // looking again.
        Notice::Unknown(reason) => Wording {
            title: "Guild Wars could not check for updates",
            detail: reason.sentence().to_owned(),
            releases: None,
        },
    }
}

/// The last answer, and when it was given.
///
/// Held across the request rather than beside it: a second asker blocks here,
/// finds the answer waiting when it wakes, and never makes a second request.
/// That is the whole concurrency design, and it is enough because the only
/// caller is a person clicking a menu item. Every answer is cached for the same
/// ten minutes including the failures — without that rule an offline machine
/// lets repeated clicks bypass the ceiling precisely when the request is least
/// likely to help.
static LAST: Mutex<Option<(Instant, Notice)>> = Mutex::new(None);

/// Ask, or repeat the answer from the last ten minutes.
///
/// Blocks for up to [`TIMEOUT`]; every caller is on a worker thread.
pub fn check() -> Notice {
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((asked, notice)) = last.as_ref()
        && asked.elapsed() < FRESH
    {
        return notice.clone();
    }
    let notice = match (repository(), parse(CURRENT)) {
        (Some(repository), Some(current)) => ask(repository, current),
        // No repository declared, or a version that is not a shape the release
        // workflow publishes. Either way there is nothing to compare, and no
        // request worth making to find that out.
        _ => Notice::Unknown(Reason::Unsupported),
    };
    *last = Some((Instant::now(), notice.clone()));
    notice
}

/// `owner/name`, when `Cargo.toml` names a GitHub repository.
///
/// The API path is built from this rather than from the URL, so a `repository`
/// pointing anywhere else — a self-hosted forge, a tarball — declines instead of
/// composing a request GitHub would answer about some other project.
pub fn repository() -> Option<&'static str> {
    slug(PROJECT_URL)
}

fn slug(url: &str) -> Option<&str> {
    let slug = url.strip_prefix("https://github.com/")?;
    let slug = slug.strip_suffix('/').unwrap_or(slug);
    let slug = slug.strip_suffix(".git").unwrap_or(slug);
    let (owner, name) = slug.split_once('/')?;
    // Exactly two segments. `github.com/owner/name/tree/main` is a page in a
    // repository, not a repository, and the API would answer about neither.
    (!owner.is_empty() && !name.is_empty() && !name.contains('/')).then_some(slug)
}

/// Where a player goes to get the newer build.
pub fn releases_url() -> Option<String> {
    Some(format!("https://github.com/{}/releases", repository()?))
}

/// One request, and the answer it justifies.
///
/// Takes what it is asking about rather than reading the constants, so the live
/// endpoint can be exercised by a test against a repository that has actually
/// published something — which a build that declares no `repository` never
/// could.
fn ask(repository: &str, current: Version) -> Notice {
    let url = format!("https://api.github.com/repos/{repository}/releases?per_page={PER_PAGE}");
    let response = match transport::fetch("GET", &url, HEADERS, None, TIMEOUT) {
        Ok(response) => response,
        Err(e) => {
            note!("[release] could not reach GitHub: {e}");
            return Notice::Unknown(Reason::Unreachable);
        }
    };
    match response.status {
        200 => {}
        // An exhausted anonymous budget is a 403 with the remaining count at
        // zero; 429 is the same refusal said the other way. Both are "come back
        // later", which is not the same as "GitHub is broken".
        403 | 429 => {
            note!("[release] GitHub is rate limiting this address");
            return Notice::Unknown(Reason::RateLimited);
        }
        status => {
            note!("[release] GitHub answered {status}");
            return Notice::Unknown(Reason::Server);
        }
    }
    match newest_offered(&response.body, current) {
        Err(e) => {
            note!("[release] could not read GitHub's release list: {e}");
            Notice::Unknown(Reason::Unreadable)
        }
        Ok(None) => Notice::Current {
            current: current.to_string(),
        },
        Ok(Some(latest)) => Notice::Available {
            current: current.to_string(),
            latest: latest.to_string(),
        },
    }
}

/// The greatest release on the page that `current` may be offered, if any.
///
/// A tag that does not parse is skipped rather than failing the whole answer:
/// one hand-made tag in a repository's history should not stop the check
/// working for everyone. `Err` is for the response not being a release list at
/// all, which is a different thing and is worth saying out loud.
fn newest_offered(body: &[u8], current: Version) -> Result<Option<Version>, String> {
    let releases: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("not JSON: {e}"))?;
    let releases = releases
        .as_array()
        .ok_or_else(|| "not a list of releases".to_owned())?;
    let mut newest: Option<Version> = None;
    for release in releases {
        // A draft is not published; it is visible to whoever wrote it and to
        // nobody this check runs for.
        if release["draft"] == serde_json::Value::Bool(true) {
            continue;
        }
        let Some(candidate) = release["tag_name"].as_str().and_then(parse) else {
            continue;
        };
        let flagged = release["prerelease"] == serde_json::Value::Bool(true);
        if !offered(current, candidate, flagged) {
            continue;
        }
        if newest.is_none_or(|newest| candidate > newest) {
            newest = Some(candidate);
        }
    }
    Ok(newest)
}

/// Whether an install on `current` may be told about `candidate`.
///
/// Newer, and on a run this install is already on. A stable install is never
/// offered a prerelease — including one whose tag looks stable but which the
/// publisher marked prerelease, because the mark is the publisher saying so and
/// the tag is only how they spelled it. A prerelease install may advance along
/// the prereleases and onto the stable release they lead to, which is the whole
/// reason someone is running one.
fn offered(current: Version, candidate: Version, flagged: bool) -> bool {
    if candidate <= current {
        return false;
    }
    current.channel != Channel::Stable || !(flagged || candidate.channel != Channel::Stable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(tag: &str) -> Version {
        parse(tag).expect("a version this project could publish")
    }

    #[test]
    fn a_tag_is_a_version_only_in_the_shapes_this_project_publishes() {
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
        assert_eq!(v("v1.2.3").to_string(), "1.2.3", "tags carry the v");
        assert_eq!(v("2026.7.1-rc.2").to_string(), "2026.7.1-rc.2");
        assert_eq!(
            v("0.0.1").to_string(),
            "0.0.1",
            "zero is not a leading zero"
        );

        // The one that matters: a CalVer-looking tag with a padded month is not
        // a SemVer version, and reading it as one would order July below
        // October.
        assert_eq!(parse("2026.07.01"), None);

        for rejected in [
            "1.2",         // not three fields
            "1.2.3.4",     // four
            "1.2.3-alpha", // a run with no sequence
            "1.2.3-alpha.01",
            "1.2.3-dev.1",   // not a run this project publishes
            "1.2.3-alpha-1", // nor this spelling of one
            "1.2.3+deadbeef",
            "v",
            "",
            "latest",
            "99999999999999999999.0.0", // wider than u64
        ] {
            assert_eq!(parse(rejected), None, "{rejected} is not a release");
        }
    }

    #[test]
    fn every_prerelease_comes_before_the_release_it_leads_to() {
        let ascending = [
            "1.2.2",
            "1.2.3-alpha.1",
            "1.2.3-alpha.2",
            "1.2.3-beta.1",
            "1.2.3-rc.1",
            "1.2.3",
            "1.3.0-alpha.1",
            "1.3.0",
            "2.0.0",
        ];
        for pair in ascending.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn a_stable_install_is_never_offered_a_prerelease() {
        let stable = v("1.2.3");
        assert!(offered(stable, v("1.2.4"), false));
        assert!(!offered(stable, v("1.2.4-rc.1"), false));
        // Marked prerelease by the publisher, whatever the tag looks like.
        assert!(!offered(stable, v("1.2.4"), true));
        assert!(!offered(stable, v("1.2.3"), false), "not newer");
        assert!(!offered(stable, v("1.2.2"), false), "older");

        // Someone running a prerelease is asking to be moved along it, and off
        // it onto the release it leads to.
        let testing = v("1.2.4-alpha.1");
        assert!(offered(testing, v("1.2.4-beta.1"), true));
        assert!(offered(testing, v("1.2.4"), false));
        assert!(!offered(testing, v("1.2.4-alpha.1"), true), "the same one");
    }

    /// The page comes back in publication order, so the newest version is not
    /// the first entry — and on a repository that has ever published a fix to
    /// an older line, it is not the last one either.
    #[test]
    fn the_newest_version_is_chosen_not_the_newest_publication() {
        let page = br#"[
            {"tag_name":"v1.1.9","draft":false,"prerelease":false},
            {"tag_name":"v2.0.0","draft":false,"prerelease":false},
            {"tag_name":"v1.2.0","draft":false,"prerelease":false}
        ]"#;
        assert_eq!(
            newest_offered(page, v("1.0.0"))
                .unwrap()
                .map(|v| v.to_string()),
            Some("2.0.0".to_owned())
        );
    }

    #[test]
    fn drafts_prereleases_and_nonsense_are_passed_over() {
        let page = br#"[
            {"tag_name":"v9.9.9","draft":true,"prerelease":false},
            {"tag_name":"v3.0.0-rc.1","draft":false,"prerelease":true},
            {"tag_name":"nightly","draft":false,"prerelease":false},
            {"tag_name":"v2.0.0","draft":false,"prerelease":false},
            {"name":"a release with no tag"}
        ]"#;
        assert_eq!(
            newest_offered(page, v("1.0.0"))
                .unwrap()
                .map(|v| v.to_string()),
            Some("2.0.0".to_owned()),
            "the draft and the prerelease are not on offer, and `nightly` is not a version"
        );
        // Nothing on the page is newer, which is an answer, not a failure.
        assert_eq!(newest_offered(page, v("2.0.0")).unwrap(), None);
    }

    #[test]
    fn an_answer_that_is_not_a_release_list_is_not_an_up_to_date() {
        // What a 200 from the wrong endpoint, or an error object, looks like.
        assert!(newest_offered(br#"{"message":"Not Found"}"#, v("1.0.0")).is_err());
        assert!(newest_offered(b"", v("1.0.0")).is_err());
        assert!(newest_offered(b"<html>", v("1.0.0")).is_err());
        // An empty list is a repository with no releases: readable, and nothing
        // newer in it.
        assert_eq!(newest_offered(b"[]", v("1.0.0")).unwrap(), None);
    }

    /// Every failure has a sentence, and no sentence is another state's.
    #[test]
    fn each_reason_says_what_it_means() {
        let reasons = [
            Reason::Unsupported,
            Reason::Unreachable,
            Reason::RateLimited,
            Reason::Server,
            Reason::Unreadable,
        ];
        for reason in reasons {
            let sentence = reason.sentence();
            assert!(!sentence.is_empty());
            assert!(
                !sentence.to_lowercase().contains("up to date"),
                "{reason:?} must not read as an answer"
            );
        }
        for pair in reasons.windows(2) {
            assert_ne!(pair[0].sentence(), pair[1].sentence());
        }
    }

    /// The three sentences a player can ever read, and which of them comes with
    /// a second button. This is the whole feature from the outside, so it is
    /// checked here rather than left to an alert nothing can read.
    #[test]
    fn the_three_answers_read_differently_and_only_one_leads_anywhere() {
        let available = Notice::Available {
            current: "1.2.3".into(),
            latest: "1.3.0".into(),
        };
        let said = wording(&available);
        assert!(said.detail.contains("1.2.3") && said.detail.contains("1.3.0"));
        assert_eq!(
            said.releases,
            releases_url(),
            "the one answer with somewhere to go must offer it"
        );

        let current = wording(&Notice::Current {
            current: "1.2.3".into(),
        });
        assert!(current.detail.contains("1.2.3"));
        assert_eq!(current.releases, None, "there is nothing to fetch");

        // The failures are the point of the third state: each says its own
        // reason, and none of them is allowed to read as an answer.
        for reason in [
            Reason::Unsupported,
            Reason::Unreachable,
            Reason::RateLimited,
            Reason::Server,
            Reason::Unreadable,
        ] {
            let said = wording(&Notice::Unknown(reason));
            assert_eq!(said.title, "Guild Wars could not check for updates");
            assert_eq!(said.detail, reason.sentence());
            assert_eq!(said.releases, None, "there is no version to go and get");
            assert_ne!(said.title, current.title);
        }
    }

    /// The API path is built from `owner/name`, so anything that is not a
    /// GitHub repository root declines rather than composing a request about
    /// something else.
    #[test]
    fn only_a_github_repository_root_is_a_repository() {
        assert_eq!(slug("https://github.com/owner/name"), Some("owner/name"));
        assert_eq!(slug("https://github.com/owner/name/"), Some("owner/name"));
        assert_eq!(
            slug("https://github.com/owner/name.git"),
            Some("owner/name")
        );

        for declined in [
            "",
            "https://github.com/owner",
            "https://github.com/owner/",
            "https://github.com/owner/name/tree/main",
            "https://github.com/",
            // Not GitHub, so the API path would name a different project's
            // releases or none at all.
            "https://gitlab.com/owner/name",
            "https://example.invalid/owner/name",
            // Plain http would be a downgrade this check has no reason to make.
            "http://github.com/owner/name",
        ] {
            assert_eq!(slug(declined), None, "{declined} is not a repository");
        }

        // And what this build actually declares: the check is offered exactly
        // when the URL is one that can be turned into an API path.
        assert_eq!(repository().is_some(), releases_url().is_some());
    }

    /// The opt-in is the whole of the permission: off means no launch asks,
    /// whatever the clock says.
    #[test]
    fn a_launch_asks_once_a_day_and_only_when_invited() {
        const DAY: u64 = AUTOMATIC_INTERVAL;
        let now = 1_800_000_000;

        assert!(!due(false, None, now), "never opted in");
        assert!(!due(false, Some(now - 10 * DAY), now), "still never");

        assert!(due(true, None, now), "opted in and never checked");
        assert!(due(true, Some(now - DAY), now), "exactly a day");
        assert!(due(true, Some(now - DAY - 1), now));
        assert!(!due(true, Some(now - DAY + 1), now), "not quite a day");
        assert!(!due(true, Some(now), now), "this launch already asked");
    }

    /// A clock that disagrees with the file must fall on the side of one extra
    /// request, not of a profile that never checks again.
    #[test]
    fn a_clock_that_went_backwards_asks_rather_than_waits() {
        // A timestamp from the future: a corrected machine, a profile copied
        // from another one, a laptop whose battery died.
        assert!(due(true, Some(2_000_000_000), 1_800_000_000));
        // What `settings::now` returns when the clock will not answer at all.
        assert!(due(true, Some(1_800_000_000), 0));
    }

    /// The line between "asked" and "volunteered". Only the answer a player can
    /// do something about is allowed to arrive uninvited.
    #[test]
    fn an_unrequested_check_interrupts_only_for_a_newer_version() {
        assert!(interrupts(&Notice::Available {
            current: "1.2.3".into(),
            latest: "1.3.0".into(),
        }));
        assert!(!interrupts(&Notice::Current {
            current: "1.2.3".into(),
        }));
        for reason in [
            Reason::Unsupported,
            Reason::Unreachable,
            Reason::RateLimited,
            Reason::Server,
            Reason::Unreadable,
        ] {
            assert!(
                !interrupts(&Notice::Unknown(reason)),
                "{reason:?} is a log line, not a modal"
            );
        }
    }

    /// The real endpoint. Run on request: `cargo test --release -- --ignored`.
    ///
    /// Everything above works from a recorded page, which cannot notice GitHub
    /// renaming a field, or refusing the headers this build sends. Asking the
    /// live API about this project's declared repository is what turns those
    /// into a failure here rather than into "could not check for updates" on a
    /// player's screen.
    #[test]
    #[ignore = "reaches api.github.com"]
    fn the_live_endpoint_answers() {
        let repository = repository().expect("Cargo.toml declares a GitHub repository");
        // Below everything this project has published, and on a prerelease
        // channel, so every release on the page is one this install may be moved
        // to.
        let notice = ask(repository, v("0.0.1-alpha.0"));
        let Notice::Available { latest, .. } = notice else {
            panic!("the live check found nothing newer: {notice:?}");
        };
        // What the alert would show, and it came back through the parse rather
        // than out of the response.
        assert_eq!(parse(&latest).map(|v| v.to_string()), Some(latest.clone()));

        // The same page, read by a stable install. It may answer `Current` or
        // `Available` as releases evolve; what it must never be is `Unknown` —
        // the page was fetched and read either way.
        let notice = ask(repository, v("0.0.1"));
        assert!(!matches!(notice, Notice::Unknown(_)), "{notice:?}");
    }

    /// A repository that exists and publishes nothing is not a failed check.
    #[test]
    #[ignore = "reaches api.github.com"]
    fn a_repository_with_no_releases_is_an_answer() {
        let missing = format!(
            "{}-does-not-exist",
            repository().expect("Cargo.toml declares a GitHub repository")
        );
        assert_eq!(
            ask(&missing, v("0.0.1")),
            Notice::Unknown(Reason::Server),
            "a repository that is not there is not an up-to-date"
        );
    }
}
