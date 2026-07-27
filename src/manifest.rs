//! ArenaNet patch manifest.
//!
//! `directories` and `files` are flat lists linked by `parentIndex`. A falsy
//! parent — absent, null, or literally 0 — means the root, so directory 0 can
//! never be anyone's parent. That is what the client does, and diverging from it
//! silently reshuffles every path in the tree.

use std::collections::{HashMap, HashSet};

use crate::error::{Error, Result};

const MAX_CHUNK_SIZE: u64 = 16 * 1024 * 1024;
const MAX_DIRECTORIES: usize = 4_096;
const MAX_FILES: usize = 4_096;
const MAX_CHUNK_REFERENCES: u64 = 1_000_000;
const MAX_NAME_LENGTH: usize = 255;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub size: u64,
    pub chunk_hashes: Vec<ContentHash>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
}

/// A lowercase hex content hash whose length selects the algorithm.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(String);

impl ContentHash {
    pub fn parse(value: &str) -> Result<Self> {
        let known_length = matches!(value.len(), 32 | 40 | 64);
        if !known_length || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::HashFormat(value.to_owned()));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn algo(&self) -> HashAlgo {
        match self.0.len() {
            32 => HashAlgo::Md5,
            40 => HashAlgo::Sha1,
            _ => HashAlgo::Sha256,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub struct Manifest {
    pub compression: Compression,
    pub chunk_size: u64,
    pub files: HashMap<String, FileEntry>,
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let raw: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| Error::ManifestFormat(e.to_string()))?;
        let obj = raw
            .as_object()
            .ok_or_else(|| Error::ManifestFormat("manifest must be an object".into()))?;

        let compression = match obj.get("compressionMode").and_then(|v| v.as_str()) {
            Some("none") => Compression::None,
            Some("gzip") => Compression::Gzip,
            other => {
                return Err(Error::ManifestFormat(format!(
                    "unsupported compression: {other:?}"
                )));
            }
        };

        let chunk_size = obj
            .get("chunkSize")
            .and_then(|v| v.as_u64())
            .filter(|n| *n > 0 && *n <= MAX_CHUNK_SIZE)
            .ok_or_else(|| Error::ManifestFormat("bad chunkSize".into()))?;

        let empty = Vec::new();
        let dirs = match obj.get("directories") {
            None | Some(serde_json::Value::Null) => &empty,
            Some(v) => v
                .as_array()
                .ok_or_else(|| Error::ManifestFormat("invalid directory list".into()))?,
        };
        let files = match obj.get("files") {
            None | Some(serde_json::Value::Null) => &empty,
            Some(v) => v
                .as_array()
                .ok_or_else(|| Error::ManifestFormat("invalid file list".into()))?,
        };
        if dirs.len() > MAX_DIRECTORIES || files.len() > MAX_FILES {
            return Err(Error::ManifestFormat("manifest too large".into()));
        }

        let dir_paths = resolve_directories(dirs)?;

        let mut parsed: HashMap<String, FileEntry> = HashMap::new();
        let mut hash_lengths: HashMap<ContentHash, u64> = HashMap::new();
        let mut references: u64 = 0;

        for file in files {
            let entry = file
                .as_object()
                .ok_or_else(|| Error::ManifestFormat("invalid file entry".into()))?;
            let name = validate_name(entry.get("name"), "file")?;
            let parent = validate_parent(entry.get("parentIndex"), dirs.len(), "file")?;
            let path = match parent {
                0 => name,
                p => format!("{}/{}", dir_paths[p], name),
            };

            let size = entry
                .get("size")
                .and_then(|v| v.as_u64())
                .filter(|n| *n > 0)
                .ok_or_else(|| Error::ManifestFormat(format!("invalid size for {path}")))?;

            let hashes = entry
                .get("chunkHashes")
                .and_then(|v| v.as_array())
                .ok_or_else(|| Error::ManifestFormat(format!("invalid chunk list for {path}")))?;

            let expected = size.div_ceil(chunk_size);
            if hashes.len() as u64 != expected {
                return Err(Error::ManifestFormat(format!(
                    "chunk count mismatch for {path}"
                )));
            }
            references += expected;
            if references > MAX_CHUNK_REFERENCES {
                return Err(Error::ManifestFormat("too many chunk references".into()));
            }

            let mut chunk_hashes = Vec::with_capacity(hashes.len());
            for (i, value) in hashes.iter().enumerate() {
                let hash = ContentHash::parse(
                    value
                        .as_str()
                        .ok_or_else(|| Error::HashFormat(value.to_string()))?,
                )?;
                // Every reference to a hash must agree on how many bytes it
                // decodes to, or a short final chunk could be served where a
                // full one is expected.
                let length = chunk_size.min(size - i as u64 * chunk_size);
                match hash_lengths.get(&hash) {
                    Some(prev) if *prev != length => {
                        return Err(Error::ManifestFormat(format!(
                            "chunk {hash} has conflicting lengths"
                        )));
                    }
                    _ => {
                        hash_lengths.insert(hash.clone(), length);
                    }
                }
                chunk_hashes.push(hash);
            }

            if parsed
                .insert(path.clone(), FileEntry { size, chunk_hashes })
                .is_some()
            {
                return Err(Error::ManifestFormat(format!(
                    "duplicate manifest path {path}"
                )));
            }
        }

        Ok(Self {
            compression,
            chunk_size,
            files: parsed,
        })
    }

    /// Decoded byte length of the `index`-th chunk of `path`.
    pub fn chunk_length(&self, path: &str, index: usize) -> Option<u64> {
        let entry = self.files.get(path)?;
        let offset = index as u64 * self.chunk_size;
        (offset < entry.size).then(|| self.chunk_size.min(entry.size - offset))
    }

    /// Resolve a basename to its single manifest path, refusing ambiguity.
    pub fn require_unique(&self, basename: &str) -> Result<&str> {
        let mut found = None;
        for path in self.files.keys() {
            if path.rsplit('/').next() == Some(basename) {
                if found.is_some() {
                    return Err(Error::ManifestFormat(format!(
                        "manifest must contain exactly one {basename}"
                    )));
                }
                found = Some(path.as_str());
            }
        }
        found.ok_or_else(|| Error::ManifestFormat(format!("manifest is missing {basename}")))
    }
}

fn resolve_directories(dirs: &[serde_json::Value]) -> Result<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unvisited,
        Visiting,
        Done,
    }

    let mut paths: Vec<String> = vec![String::new(); dirs.len()];
    let mut marks = vec![Mark::Unvisited; dirs.len()];

    // Iterative so a deep or hostile parent chain cannot blow the stack.
    for start in 0..dirs.len() {
        if marks[start] == Mark::Done {
            continue;
        }
        let mut stack = vec![start];
        while let Some(&index) = stack.last() {
            if marks[index] == Mark::Done {
                stack.pop();
                continue;
            }
            let entry = dirs[index]
                .as_object()
                .ok_or_else(|| Error::ManifestFormat("invalid directory entry".into()))?;
            let parent = validate_parent(entry.get("parentIndex"), dirs.len(), "directory")?;

            if parent != 0 && marks[parent] != Mark::Done {
                if marks[parent] == Mark::Visiting {
                    return Err(Error::ManifestFormat("directory parent cycle".into()));
                }
                marks[index] = Mark::Visiting;
                stack.push(parent);
                continue;
            }

            let name = validate_name(entry.get("name"), "directory")?;
            paths[index] = if parent == 0 {
                name
            } else {
                format!("{}/{}", paths[parent], name)
            };
            marks[index] = Mark::Done;
            stack.pop();
        }
    }

    let unique: HashSet<&String> = paths.iter().collect();
    if unique.len() != paths.len() {
        return Err(Error::ManifestFormat("duplicate directory path".into()));
    }
    Ok(paths)
}

/// Names must not be able to escape the tree. Rust has no prototype to pollute,
/// so unlike the JavaScript host this only guards path traversal and separators.
fn validate_name(value: Option<&serde_json::Value>, kind: &str) -> Result<String> {
    let name = value
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::ManifestFormat(format!("invalid {kind} name")))?;
    let bad = name.is_empty()
        || name.len() > MAX_NAME_LENGTH
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0');
    if bad {
        return Err(Error::ManifestFormat(format!("invalid {kind} name")));
    }
    Ok(name.to_owned())
}

/// The live service signals "no parent" by sending null rather than omitting the
/// field. Rejecting that stops every client update from applying.
fn validate_parent(value: Option<&serde_json::Value>, len: usize, kind: &str) -> Result<usize> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(0),
        Some(v) => {
            let n = v
                .as_u64()
                .filter(|n| (*n as usize) < len)
                .ok_or_else(|| Error::ManifestFormat(format!("invalid {kind} parent index")))?;
            Ok(n as usize)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(json: &str) -> Result<Manifest> {
        Manifest::parse(json.as_bytes())
    }

    #[test]
    fn parses_flat_files() {
        let m = manifest(&format!(
            r#"{{"compressionMode":"none","chunkSize":1024,
                 "files":[{{"name":"Gw.jspi.wasm","size":2048,
                            "chunkHashes":["{}","{}"]}}]}}"#,
            "a".repeat(32),
            "b".repeat(32)
        ))
        .expect("parse");
        assert_eq!(m.chunk_size, 1024);
        assert_eq!(m.compression, Compression::None);
        assert_eq!(m.files["Gw.jspi.wasm"].chunk_hashes.len(), 2);
    }

    #[test]
    fn null_parent_index_means_root() {
        let m = manifest(&format!(
            r#"{{"compressionMode":"gzip","chunkSize":16,
                 "directories":[{{"name":"root","parentIndex":null}}],
                 "files":[{{"name":"a.bin","size":16,"parentIndex":null,
                            "chunkHashes":["{}"]}}]}}"#,
            "c".repeat(64)
        ))
        .expect("parse");
        assert!(m.files.contains_key("a.bin"));
    }

    #[test]
    fn rejects_traversal_names() {
        let err = manifest(&format!(
            r#"{{"compressionMode":"none","chunkSize":16,
                 "files":[{{"name":"..","size":16,"chunkHashes":["{}"]}}]}}"#,
            "d".repeat(40)
        ));
        assert!(err.is_err());
    }

    #[test]
    fn rejects_chunk_count_mismatch() {
        let err = manifest(&format!(
            r#"{{"compressionMode":"none","chunkSize":16,
                 "files":[{{"name":"a","size":64,"chunkHashes":["{}"]}}]}}"#,
            "e".repeat(64)
        ));
        assert!(err.is_err());
    }

    #[test]
    fn rejects_conflicting_chunk_lengths() {
        // Same hash used where a full chunk and a short tail chunk are expected.
        let h = "f".repeat(64);
        let err = manifest(&format!(
            r#"{{"compressionMode":"none","chunkSize":16,
                 "files":[{{"name":"a","size":32,"chunkHashes":["{h}","{h}"]}},
                          {{"name":"b","size":8,"chunkHashes":["{h}"]}}]}}"#
        ));
        assert!(err.is_err());
    }

    #[test]
    fn rejects_directory_cycle() {
        let err = manifest(
            r#"{"compressionMode":"none","chunkSize":16,
                "directories":[{"name":"a","parentIndex":2},
                               {"name":"b","parentIndex":1},
                               {"name":"c","parentIndex":1}],
                "files":[]}"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn hash_algorithm_follows_length() {
        assert_eq!(
            ContentHash::parse(&"a".repeat(32)).unwrap().algo(),
            HashAlgo::Md5
        );
        assert_eq!(
            ContentHash::parse(&"a".repeat(40)).unwrap().algo(),
            HashAlgo::Sha1
        );
        assert_eq!(
            ContentHash::parse(&"a".repeat(64)).unwrap().algo(),
            HashAlgo::Sha256
        );
        assert!(ContentHash::parse(&"a".repeat(31)).is_err());
        assert!(ContentHash::parse(&"z".repeat(32)).is_err());
    }
}
