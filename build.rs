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

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the source lives, relative to the package root.
const KERNEL: &str = "src/companion-kernel/lib.rs";

/// The triple the companion is built for. Named here as well as in
/// `rust-toolchain.toml` because the toolchain file is what installs it and
/// this is what asks for it; the error below is what connects the two when a
/// checkout has one and not the other.
const TARGET: &str = "wasm32-unknown-unknown";

fn main() {
    println!("cargo::rerun-if-changed={KERNEL}");
    println!("cargo::rerun-if-changed=build.rs");

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
