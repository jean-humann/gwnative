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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
}

impl HashAlgo {
    /// Digest length in bytes, which is also what selects the algorithm: the
    /// manifest names no algorithm, only a hex string, and 32, 40 and 64
    /// characters can mean nothing else.
    const fn length(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }
}

/// A content hash, kept as its bytes rather than as the hex it arrived in.
///
/// The snapshot alone contributes 16167 chunk references, and a `String` for
/// each is 24 bytes of struct plus a 64-byte allocation plus the allocator's
/// header — about a hundred bytes, every one of them behind a pointer, and one
/// trip through malloc per chunk while parsing. The widest digest in use is 32
/// bytes, so the whole thing fits inline with room left to say which algorithm
/// it is, the parse fills a flat run of memory, and comparing two hashes is a
/// fixed-width byte compare rather than a string one.
///
/// Hex is what the cache filenames are made of, so it is still reachable — see
/// [`hex`](Self::hex), which renders onto the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash {
    algo: HashAlgo,
    /// Only `algo.length()` bytes are meaningful; the tail stays zero so that
    /// the derived `Eq` and `Hash` can read the whole array.
    digest: [u8; 32],
}

impl ContentHash {
    pub fn parse(value: &str) -> Result<Self> {
        let algo = match value.len() {
            32 => HashAlgo::Md5,
            40 => HashAlgo::Sha1,
            64 => HashAlgo::Sha256,
            _ => return Err(Error::HashFormat(value.to_owned())),
        };
        let mut digest = [0u8; 32];
        hex::decode_to_slice(value, &mut digest[..algo.length()])
            .map_err(|_| Error::HashFormat(value.to_owned()))?;
        Ok(Self { algo, digest })
    }

    pub fn algo(&self) -> HashAlgo {
        self.algo
    }

    /// The digest itself, trimmed to the algorithm's width.
    pub fn bytes(&self) -> &[u8] {
        &self.digest[..self.algo.length()]
    }

    /// The lowercase hex form, on the stack.
    pub fn hex(&self) -> Hex {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut text = [0u8; 64];
        for (byte, pair) in self.bytes().iter().zip(text.chunks_exact_mut(2)) {
            pair[0] = DIGITS[usize::from(byte >> 4)];
            pair[1] = DIGITS[usize::from(byte & 0xf)];
        }
        Hex {
            text,
            length: self.algo.length() * 2,
        }
    }
}

/// A [`ContentHash`] rendered as hex, borrowable as `&str` and never allocated.
pub struct Hex {
    text: [u8; 64],
    length: usize,
}

impl Hex {
    pub fn as_str(&self) -> &str {
        // Only the sixteen ASCII hex digits are ever written here, so this
        // cannot fail; it is a length check over at most 64 bytes rather than
        // an `unsafe` for something that costs nothing.
        std::str::from_utf8(&self.text[..self.length]).expect("hex digits are ascii")
    }
}

impl std::ops::Deref for Hex {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for Hex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.hex())
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

        let compression = compression(obj)?;
        let chunk_size = chunk_size(obj)?;

        let dirs = list(obj, "directories", "directory")?;
        let files = list(obj, "files", "file")?;
        if dirs.len() > MAX_DIRECTORIES || files.len() > MAX_FILES {
            return Err(Error::ManifestFormat("manifest too large".into()));
        }

        let dir_paths = resolve_directories(dirs)?;

        let mut parsed: HashMap<String, FileEntry> = HashMap::new();
        let mut tally = Tally::default();
        for file in files {
            let (path, entry) = parse_file(file, &dir_paths, chunk_size, &mut tally)?;
            if parsed.insert(path.clone(), entry).is_some() {
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

fn compression(obj: &serde_json::Map<String, serde_json::Value>) -> Result<Compression> {
    match obj.get("compressionMode").and_then(|v| v.as_str()) {
        Some("none") => Ok(Compression::None),
        Some("gzip") => Ok(Compression::Gzip),
        other => Err(Error::ManifestFormat(format!(
            "unsupported compression: {other:?}"
        ))),
    }
}

fn chunk_size(obj: &serde_json::Map<String, serde_json::Value>) -> Result<u64> {
    obj.get("chunkSize")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0 && *n <= MAX_CHUNK_SIZE)
        .ok_or_else(|| Error::ManifestFormat("bad chunkSize".into()))
}

/// One of the manifest's two arrays.
///
/// Absent and null both read as empty, which is not laxness: the live service
/// omits `directories` entirely for a flat manifest, and refusing that would
/// refuse every such update.
fn list<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
    kind: &str,
) -> Result<&'a [serde_json::Value]> {
    match obj.get(key) {
        None | Some(serde_json::Value::Null) => Ok(&[]),
        Some(v) => v
            .as_array()
            .map(Vec::as_slice)
            .ok_or_else(|| Error::ManifestFormat(format!("invalid {kind} list"))),
    }
}

/// What the file loop carries from one file to the next: the length every hash
/// has been seen to decode to, and how many chunk references the whole manifest
/// has spent so far.
#[derive(Default)]
struct Tally {
    lengths: HashMap<ContentHash, u64>,
    references: u64,
}

/// One entry of the file list, resolved to its full path and its chunk list.
fn parse_file(
    file: &serde_json::Value,
    dir_paths: &[String],
    chunk_size: u64,
    tally: &mut Tally,
) -> Result<(String, FileEntry)> {
    let entry = file
        .as_object()
        .ok_or_else(|| Error::ManifestFormat("invalid file entry".into()))?;
    let name = validate_name(entry.get("name"), "file")?;
    let parent = validate_parent(entry.get("parentIndex"), dir_paths.len(), "file")?;
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
    tally.references += expected;
    if tally.references > MAX_CHUNK_REFERENCES {
        return Err(Error::ManifestFormat("too many chunk references".into()));
    }

    let mut chunk_hashes = Vec::with_capacity(hashes.len());
    for (i, value) in hashes.iter().enumerate() {
        let hash = ContentHash::parse(
            value
                .as_str()
                .ok_or_else(|| Error::HashFormat(value.to_string()))?,
        )?;
        // Every reference to a hash must agree on how many bytes it decodes to,
        // or a short final chunk could be served where a full one is expected.
        let length = chunk_size.min(size - i as u64 * chunk_size);
        match tally.lengths.get(&hash) {
            Some(prev) if *prev != length => {
                return Err(Error::ManifestFormat(format!(
                    "chunk {hash} has conflicting lengths"
                )));
            }
            _ => {
                tally.lengths.insert(hash, length);
            }
        }
        chunk_hashes.push(hash);
    }

    Ok((path, FileEntry { size, chunk_hashes }))
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

    /// The hex is what the cache filenames are made of, so a hash that decoded
    /// and rendered back to anything but the manifest's own text would look for
    /// its chunks in the wrong place — silently, and only on a real manifest.
    #[test]
    fn hex_round_trips_at_every_width() {
        for text in [
            "0123456789abcdef".repeat(2),
            "fe".repeat(20),
            "9c".repeat(32),
        ] {
            let hash = ContentHash::parse(&text).unwrap();
            assert_eq!(hash.hex().as_str(), text);
            assert_eq!(hash.bytes().len() * 2, text.len());
        }
    }

    /// Storing bytes rather than text is what makes this true for free; the
    /// `to_ascii_lowercase` it replaces had to be remembered.
    #[test]
    fn case_does_not_make_a_different_hash() {
        let lower = ContentHash::parse(&"ab".repeat(16)).unwrap();
        let upper = ContentHash::parse(&"AB".repeat(16)).unwrap();
        assert_eq!(lower, upper);
        assert_eq!(upper.hex().as_str(), "ab".repeat(16));
    }

    /// The unused tail of a short digest has to stay zero, or two equal MD5s
    /// would hash and compare as different once the derived `Eq` read all 32
    /// bytes — and the chunk cache keys on exactly that.
    #[test]
    fn a_short_digest_leaves_no_junk_behind_it() {
        let md5 = ContentHash::parse(&"7f".repeat(16)).unwrap();
        assert_eq!(md5.bytes(), [0x7f; 16]);
        assert_eq!(md5, ContentHash::parse(&"7F".repeat(16)).unwrap());
    }
}
