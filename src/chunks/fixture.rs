//! Stores to test against, with no network behind them.
//!
//! Every test in this module asks what is on disk or what the store decided,
//! which are questions the patch client never answers — so the client these are
//! built with has no credentials and is never expected to succeed. A fetch that
//! reaches it fails, which several of the tests rely on.

use std::path::PathBuf;

use super::ChunkStore;
use crate::manifest::Manifest;
use crate::patch::Client;

/// A store over a five-chunk snapshot, for the residency scans.
pub(super) fn store_of_five(cache_dir: PathBuf) -> ChunkStore {
    // Distinct leading bytes, so each chunk lands in its own fan-out bucket.
    let hashes = ["00", "11", "22", "33", "44"]
        .iter()
        .enumerate()
        .map(|(i, bucket)| {
            format!(
                r#""{bucket}{}""#,
                char::from(b'1' + i as u8).to_string().repeat(30)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    open(&hashes, 5 * 1024, cache_dir)
}

/// A store over `count` chunks, for the arithmetic that only bites on a
/// snapshot bigger than the readahead window.
pub(super) fn store_of(cache_dir: PathBuf, count: usize) -> ChunkStore {
    let hashes = (0..count)
        .map(|i| format!(r#""{i:032x}""#))
        .collect::<Vec<_>>()
        .join(",");
    open(&hashes, (count * 1024) as u64, cache_dir)
}

fn open(hashes: &str, size: u64, cache_dir: PathBuf) -> ChunkStore {
    let manifest = Manifest::parse(
        format!(
            r#"{{"compressionMode":"none","chunkSize":1024,
                 "files":[{{"name":"Gw.snapshot","size":{size},
                            "chunkHashes":[{hashes}]}}]}}"#
        )
        .as_bytes(),
    )
    .expect("the synthetic manifest should parse");
    ChunkStore::open(
        Client::new(String::new(), String::new()),
        manifest,
        cache_dir,
    )
    .expect("the store should open over an empty cache")
}
