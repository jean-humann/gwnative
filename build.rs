//! Compiles the companion module, the one artefact Cargo cannot build for us.
//!
//! `src/companion-kernel/lib.rs` is a freestanding `no_std` crate targeting
//! `wasm32-unknown-unknown`. It cannot be a Cargo target of this package —
//! a package builds for one target triple at a time, and the host binary is
//! `aarch64-apple-darwin` — and it does not want to be a workspace member
//! either: it has no dependencies, no `Cargo.toml` would carry information the
//! flags below do not, and a second manifest would give `cargo test` a crate it
//! cannot link. One `rustc` call is the whole build.
//!
//! `--import-memory` is what makes it a companion rather than a program: the
//! linker leaves `env.memory` unresolved so the page can instantiate it over
//! the *client's* memory, which is the only reason any of it can see game
//! state. `panic=abort` and `-C opt-level=s` keep it small and free of the
//! unwinder; `--strip-all` drops the name section, which is a third of the
//! output and of no use to anything that reads this module.
//!
//! The result is `include_bytes!`d by [`crate::server`] rather than written
//! into `web/`, so the kernel a build serves is always the one that build
//! compiled, and no packaging step has to remember to copy it.
//!
//! The second half of the file is one link line, for the updater framework —
//! see [`sparkle`].

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the source lives, relative to the package root.
const KERNEL: &str = "src/companion-kernel/lib.rs";

/// The vendored updater framework, relative to the package root. See
/// `packaging/sparkle/README.md` for what is in it and where it came from.
const SPARKLE: &str = "packaging/sparkle";

/// The triple the companion is built for. Named here as well as in
/// `rust-toolchain.toml` because the toolchain file is what installs it and
/// this is what asks for it; the error below is what connects the two when a
/// checkout has one and not the other.
const TARGET: &str = "wasm32-unknown-unknown";

fn main() {
    println!("cargo::rerun-if-changed={KERNEL}");
    println!("cargo::rerun-if-changed=build.rs");

    sparkle();

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("companion-kernel.wasm");
    // `RUSTC` rather than a bare `rustc`, so the compiler that builds the
    // companion is the one Cargo is already using — a `cargo +nightly` run
    // would otherwise silently mix two toolchains.
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());

    let status = Command::new(&rustc)
        .arg(KERNEL)
        .args(["--edition=2021", "--target", TARGET])
        .args(["--crate-type", "cdylib"])
        .args(["-C", "opt-level=s"])
        .args(["-C", "panic=abort"])
        .args(["-C", "link-arg=--import-memory"])
        .args(["-C", "link-arg=--strip-all"])
        .arg("-o")
        .arg(&out)
        .status();

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => panic!(
            "compiling {KERNEL} for {TARGET} failed ({status}).\n\
             If the target is missing: rustup target add {TARGET}",
        ),
        Err(e) => panic!("could not run {}: {e}", Path::new(&rustc).display()),
    }
}

/// Link the updater framework, weakly, and say where to find it at run time.
///
/// Weakly is the whole design. `scripts/bundle` installs Sparkle into
/// `Contents/Frameworks`, and nothing else does — a `cargo run` build, a
/// benchmark and the test harness are bare executables with no `../Frameworks`
/// to look in. A weak link is the one kind dyld is allowed to not find: the
/// process starts, the classes are simply absent, and [`crate::updater`] asks
/// the runtime whether they are there rather than assuming. What it does when
/// they are not is what this project did before Sparkle existed — ask GitHub
/// and put the answer in an alert.
///
/// So the arguments are two facts and one instruction. `-F` is where the
/// linker reads the framework's headers and stub from now, absolute because a
/// relative path would be read against whatever directory `rustc` was spawned
/// in. `-weak_framework` is the link itself. `-rpath` is where *dyld* looks
/// later, and `@executable_path/../Frameworks` is that path spelled relative to
/// the binary, so the bundle can be moved, renamed, or opened from a disk image
/// without the lookup changing.
///
/// `-bins` rather than plain `rustc-link-arg`: the test harness is a separate
/// executable that would take the rpath and never use it, and the classes being
/// absent under `cargo test` is the case the fallback most needs to stay honest
/// about.
fn sparkle() {
    let root = std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let dir = PathBuf::from(root).join(SPARKLE);
    let binary = dir.join("Sparkle.framework/Versions/B/Sparkle");
    println!("cargo::rerun-if-changed={}", binary.display());
    if !binary.exists() {
        // Not a panic. The framework is committed, so this means a checkout
        // somebody trimmed on purpose — and the build that comes out of it
        // still runs, still updates, and only does it the older way.
        println!(
            "cargo::warning={} is missing; building without Sparkle",
            dir.display()
        );
        return;
    }
    println!("cargo::rustc-link-arg-bins=-F{}", dir.display());
    println!("cargo::rustc-link-arg-bins=-weak_framework");
    println!("cargo::rustc-link-arg-bins=Sparkle");
    println!("cargo::rustc-link-arg-bins=-Wl,-rpath,@executable_path/../Frameworks");
}
