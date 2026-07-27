//! Run the page's own tests as part of `cargo test`.
//!
//! Half of this app is JavaScript, and until now none of it was tested — a
//! regression in `web/` was something a player found. Node's built-in runner
//! needs no package.json and no dependency tree, so the cost of covering that
//! half is this file.
//!
//! Skipped, loudly, when node is not installed: the host builds and runs
//! without it, and failing a Rust test suite over a missing optional tool would
//! only teach people to ignore the result.

use std::path::Path;
use std::process::Command;

#[test]
fn page_modules_behave() {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");

    let output = match Command::new("node")
        .arg("--test")
        // Node resolves this glob itself. A shell would expand it first and
        // pass absolute paths, which works too, but only from a shell.
        .arg("*.test.js")
        .current_dir(&web)
        .output()
    {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("[web] skipped: node is not installed");
            return;
        }
        Err(e) => panic!("could not run node: {e}"),
    };

    if !output.status.success() {
        // The runner's own report says more than any assertion here could.
        panic!(
            "the page's tests failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
