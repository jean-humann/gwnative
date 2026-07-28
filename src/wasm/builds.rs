//! The one ArenaNet build this transform is certified against, described
//! precisely enough that anything else fails closed.
//!
//! Everything here is data: which stub each bridge replaces, what that stub's
//! body looks like byte-for-byte, and where every call site to it sits. The
//! bodies are recorded rather than merely the indices because an index is a
//! coincidence and a body is a fingerprint — a later build that happens to put
//! a different function at 185 is rejected instead of rewritten.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum BridgeKind {
    EnsureDirectory,
    FindFiles,
    FileBaseName,
    DeleteFile,
    FileExists,
}

impl BridgeKind {
    /// The dirfd this bridge travels behind. Far outside any fd a real call can
    /// produce, and negative, so the carrier's own callers cannot collide with
    /// it. `web/template-save.js` receives these from the host rather than
    /// restating them, so the side that writes them into the module and the side
    /// that answers them cannot drift apart.
    pub(super) const fn marker(self) -> i64 {
        match self {
            Self::EnsureDirectory => -70_001,
            Self::FindFiles => -70_002,
            Self::FileBaseName => -70_003,
            Self::DeleteFile => -70_004,
            Self::FileExists => -70_005,
        }
    }

    /// The name the page knows this by.
    pub(super) const fn key(self) -> &'static str {
        match self {
            Self::EnsureDirectory => "ensureDirectory",
            Self::FindFiles => "findFiles",
            Self::FileBaseName => "fileBaseName",
            Self::DeleteFile => "deleteFile",
            Self::FileExists => "fileExists",
        }
    }
}

pub(super) const ALL_BRIDGE_KINDS: [BridgeKind; 5] = [
    BridgeKind::EnsureDirectory,
    BridgeKind::FindFiles,
    BridgeKind::FileBaseName,
    BridgeKind::DeleteFile,
    BridgeKind::FileExists,
];

pub(super) struct CallSite {
    /// Index into the code section, i.e. function index minus import count.
    pub local_function: usize,
    /// Byte offset of the `call` opcode inside that function body.
    pub body_offset: usize,
}

pub(super) struct StubBridge {
    pub kind: BridgeKind,
    /// The function the certified call sites currently target. Its type is
    /// reused for the forwarder, and `FileExists` also calls it.
    pub stub_function: usize,
    /// Whole body of the stub, so an unexpected build fails closed. `None` when
    /// the target is a real implementation rather than a stub — there the call
    /// site's own target index is the certification.
    pub stub_body: Option<&'static [u8]>,
    pub call_sites: &'static [CallSite],
}

pub(super) struct KnownBuild {
    pub sha256: &'static str,
    pub output_sha256: &'static str,
    pub import_count: u32,
    /// Import index of `__syscall_newfstatat`, the bridge's carrier.
    pub carrier_import: u32,
    pub bridges: &'static [StubBridge],
}

pub(super) const BUILDS: &[KnownBuild] = &[KnownBuild {
    sha256: "b0319704f3072d6948a66026a35af5eb0af12b48d70986783c293e7c77e98483",
    output_sha256: "68c6e09cec0f6992058a44a5617ca9eac7fab4697be1421943bbf664e6d444f6",
    import_count: 219,
    carrier_import: 207,
    bridges: &[
        // PathCreateDirectory(path, recursive) -> error. `i32.const 2` is
        // ERROR_FILE_NOT_FOUND, returned unconditionally.
        StubBridge {
            kind: BridgeKind::EnsureDirectory,
            stub_function: 185,
            stub_body: Some(&[0x00, 0x41, 0x02, 0x0b]),
            call_sites: &[
                CallSite {
                    local_function: 9538,
                    body_offset: 171,
                }, // template save
                CallSite {
                    local_function: 11525,
                    body_offset: 142,
                }, // chat log
                CallSite {
                    local_function: 12214,
                    body_offset: 127,
                }, // screenshot
            ],
        },
        // FindFiles(out, pattern, flags) -> void. An empty body leaves the
        // caller's list zeroed, so every directory reads as empty.
        StubBridge {
            kind: BridgeKind::FindFiles,
            stub_function: 186,
            stub_body: Some(&[0x00, 0x0b]),
            call_sites: &[
                CallSite {
                    local_function: 9527,
                    body_offset: 157,
                }, // skills list
                CallSite {
                    local_function: 9528,
                    body_offset: 157,
                }, // equipment list
                CallSite {
                    local_function: 11525,
                    body_offset: 210,
                }, // chat log
                CallSite {
                    local_function: 12214,
                    body_offset: 419,
                }, // screenshot
            ],
        },
        // FileBaseName(dst, _, baseDir, _, path, dstChars) -> written.
        // `i32.const 0` leaves the caller reading uninitialised stack. Only the
        // two template lists are repointed; the model paths that also call it
        // keep the fallback branch they take today.
        StubBridge {
            kind: BridgeKind::FileBaseName,
            stub_function: 197,
            stub_body: Some(&[0x00, 0x41, 0x00, 0x0b]),
            call_sites: &[
                CallSite {
                    local_function: 9527,
                    body_offset: 276,
                },
                CallSite {
                    local_function: 9528,
                    body_offset: 278,
                },
            ],
        },
        // DeleteFile(path) -> deleted. Not a silent stub like the others: the
        // body is `assert("not implemented")` followed by `unreachable`, so
        // deleting a build aborted the client outright.
        StubBridge {
            kind: BridgeKind::DeleteFile,
            stub_function: 333,
            stub_body: Some(&[
                0x00, //
                0x41, 0x9e, 0x87, 0xc5, 0x80, 0x00, //
                0x41, 0x80, 0xbb, 0xc3, 0x80, 0x00, //
                0x41, 0xc8, 0x06, //
                0x10, 0xc2, 0x82, 0x80, 0x80, 0x00, //
                0x00, //
                0x0b,
            ]),
            call_sites: &[CallSite {
                local_function: 459,
                body_offset: 201,
            }],
        },
        // File::Open(path, mode, err). Mode 1 is meant to open an existing
        // file, and 9757 uses it to ask whether a rename's destination is
        // already taken — but in this build mode 1 opens O_RDWR|O_CREAT, the
        // same as the write mode. The probe creates the file it is testing for,
        // reports it present, and refuses every rename.
        //
        // Only the probe is repointed. The write call five instructions later
        // keeps the real function, and so does the load path.
        StubBridge {
            kind: BridgeKind::FileExists,
            stub_function: 552,
            stub_body: None,
            call_sites: &[CallSite {
                local_function: 9538,
                body_offset: 201,
            }],
        },
    ],
}];

pub(super) fn find_build(sha256: &str) -> Option<&'static KnownBuild> {
    BUILDS.iter().find(|build| build.sha256 == sha256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_marker_is_distinct_and_out_of_reach_of_a_real_fd() {
        let mut seen = std::collections::HashSet::new();
        for kind in ALL_BRIDGE_KINDS {
            assert!(
                seen.insert(kind.marker()),
                "duplicate marker for {}",
                kind.key()
            );
            // A real dirfd is a small non-negative integer or AT_FDCWD (-100).
            assert!(kind.marker() < -1000);
        }
    }

    #[test]
    fn the_table_is_self_consistent() {
        for build in BUILDS {
            assert_eq!(build.sha256.len(), 64);
            assert_eq!(build.output_sha256.len(), 64);
            assert!(build.carrier_import < build.import_count);
            let kinds: Vec<_> = build.bridges.iter().map(|b| b.kind).collect();
            for kind in ALL_BRIDGE_KINDS {
                assert!(kinds.contains(&kind), "{} has no bridge", kind.key());
            }
            for bridge in build.bridges {
                assert!(
                    !bridge.call_sites.is_empty(),
                    "{} repoints nothing",
                    bridge.kind.key()
                );
            }
        }
    }
}
