//! Opt-in `.gwmod` validation and dependency resolution.
//!
//! The bundle format is intentionally compatible with `gw_in_browser`: a ZIP
//! containing `manifest.json`, with modules named in dependency-first order.
//! This implementation is independent and host-side so malformed packages are
//! rejected before WebKit sees a byte. Nothing is discovered or loaded unless
//! the player selects `-modfile`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use flate2::read::DeflateDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const FORMAT: u32 = 1;
const MAX_DEPTH: usize = 8;
const MAX_BUNDLE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 128;
const MAX_MODULES: usize = 64;
const MAX_NAME: usize = 128;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    format: u32,
    name: String,
    entry: String,
    modules: Vec<String>,
}

/// A validated WebAssembly module, in dependency-first load order.
#[derive(Clone, Debug)]
pub struct Module {
    pub name: String,
    pub sha256: String,
    pub bytes: Arc<[u8]>,
}

/// Everything selected by one explicit modfile.
#[derive(Clone, Debug)]
pub struct Catalog {
    pub name: String,
    pub source: PathBuf,
    pub modules: Vec<Module>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub name: String,
    pub file: String,
    pub modules: usize,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Catalog {
    pub fn public_json(&self) -> serde_json::Value {
        serde_json::json!({
            "format": FORMAT,
            "name": self.name,
            "source": self.source.file_name().and_then(|name| name.to_str()),
            "modules": self.modules.iter().enumerate().map(|(index, module)| {
                serde_json::json!({
                    "index": index,
                    "name": module.name,
                    "sha256": module.sha256,
                    "size": module.bytes.len(),
                    "url": format!("__mods/{index}"),
                })
            }).collect::<Vec<_>>(),
        })
    }
}

/// Resolve an explicit session modfile.
pub fn load(path: &Path) -> Result<Catalog, String> {
    let source = absolute(path)?;
    let mut resolver = Resolver::default();
    let (name, modules) = resolver.load_path(&source, 0)?;
    if modules.is_empty() {
        return Err(format!(
            "{} resolves to no WebAssembly modules",
            source.display()
        ));
    }
    Ok(Catalog {
        name,
        source,
        modules,
    })
}

/// Inspect bundle headers for `gwnative mods`; never executes their contents.
pub fn discover(directory: &Path) -> Vec<Summary> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut summaries = Vec::new();
    for entry in entries.flatten().take(256) {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("gwmod") {
            continue;
        }
        let file = entry.file_name().to_string_lossy().into_owned();
        let result = read_bounded(&path, MAX_BUNDLE_BYTES)
            .and_then(|bytes| Archive::parse(&bytes))
            .and_then(|archive| archive.manifest())
            .and_then(|manifest| validate_manifest(&manifest).map(|()| manifest));
        summaries.push(match result {
            Ok(manifest) => Summary {
                name: manifest.name,
                file,
                modules: manifest.modules.len(),
                valid: true,
                error: None,
            },
            Err(error) => Summary {
                name: path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("unknown")
                    .to_owned(),
                file,
                modules: 0,
                valid: false,
                error: Some(error),
            },
        });
    }
    summaries.sort_by(|left, right| left.file.cmp(&right.file));
    summaries
}

#[derive(Default)]
struct Resolver {
    seen_bundles: HashSet<String>,
    seen_modules: HashSet<String>,
    total_bytes: usize,
}

impl Resolver {
    fn load_path(&mut self, path: &Path, depth: usize) -> Result<(String, Vec<Module>), String> {
        let extension = path.extension().and_then(|value| value.to_str());
        match extension {
            Some("json") => self.load_session(path, depth),
            Some("gwmod") => {
                let bytes = read_bounded(path, MAX_BUNDLE_BYTES)?;
                self.load_bundle(
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("bundle.gwmod"),
                    &bytes,
                    depth,
                )
            }
            Some("wasm") => {
                let bytes = read_bounded(path, MAX_ENTRY_BYTES)?;
                let module = self.module(
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("module.wasm"),
                    bytes,
                )?;
                Ok((
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("module")
                        .to_owned(),
                    module.into_iter().collect(),
                ))
            }
            _ => Err(format!(
                "{} must be a .json modfile, .gwmod bundle, or .wasm module",
                path.display()
            )),
        }
    }

    fn load_session(&mut self, path: &Path, depth: usize) -> Result<(String, Vec<Module>), String> {
        depth_guard(depth)?;
        let bytes = read_bounded(path, MAX_MANIFEST_BYTES)?;
        let manifest = parse_manifest(&bytes, &path.display().to_string())?;
        let base = path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", path.display()))?
            .canonicalize()
            .map_err(|error| format!("could not resolve {}: {error}", path.display()))?;
        let mut modules = Vec::new();
        for reference in &manifest.modules {
            safe_name(reference)?;
            let target = base.join(reference);
            let target = target
                .canonicalize()
                .map_err(|error| format!("could not resolve {}: {error}", target.display()))?;
            if !target.starts_with(&base) {
                return Err(format!("{reference:?} escapes the modfile directory"));
            }
            let (_, nested) = self.load_path(&target, depth + 1)?;
            modules.extend(nested);
        }
        Ok((manifest.name, modules))
    }

    fn load_bundle(
        &mut self,
        shown: &str,
        bytes: &[u8],
        depth: usize,
    ) -> Result<(String, Vec<Module>), String> {
        depth_guard(depth)?;
        let hash = hex::encode(Sha256::digest(bytes));
        if !self.seen_bundles.insert(hash) {
            return Ok((shown.to_owned(), Vec::new()));
        }
        let archive = Archive::parse(bytes).map_err(|error| format!("{shown}: {error}"))?;
        let manifest = archive
            .manifest()
            .map_err(|error| format!("{shown}: {error}"))?;
        let mut modules = Vec::new();
        for reference in &manifest.modules {
            let bytes = archive
                .entries
                .get(reference)
                .ok_or_else(|| format!("{shown}: manifest member {reference:?} is missing"))?;
            if reference.ends_with(".gwmod") {
                let (_, nested) = self.load_bundle(reference, bytes, depth + 1)?;
                modules.extend(nested);
            } else if reference.ends_with(".wasm") {
                if let Some(module) = self.module(reference, bytes.clone())? {
                    modules.push(module);
                }
            } else {
                return Err(format!(
                    "{shown}: module {reference:?} must end in .wasm or .gwmod"
                ));
            }
        }
        Ok((manifest.name, modules))
    }

    fn module(&mut self, name: &str, bytes: Vec<u8>) -> Result<Option<Module>, String> {
        if !bytes.starts_with(b"\0asm\x01\0\0\0") {
            return Err(format!("{name}: not a WebAssembly 1 module"));
        }
        self.total_bytes = self
            .total_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| "module byte total overflowed".to_owned())?;
        if self.total_bytes > MAX_TOTAL_BYTES {
            return Err(format!(
                "resolved modules exceed the {} MiB session limit",
                MAX_TOTAL_BYTES / 1024 / 1024
            ));
        }
        let sha256 = hex::encode(Sha256::digest(&bytes));
        if !self.seen_modules.insert(sha256.clone()) {
            return Ok(None);
        }
        Ok(Some(Module {
            name: name.to_owned(),
            sha256,
            bytes: Arc::from(bytes),
        }))
    }
}

#[derive(Debug)]
struct Archive {
    entries: BTreeMap<String, Vec<u8>>,
}

impl Archive {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BUNDLE_BYTES {
            return Err(format!(
                "bundle exceeds the {} MiB compressed limit",
                MAX_BUNDLE_BYTES / 1024 / 1024
            ));
        }
        let eocd = find_eocd(bytes).ok_or_else(|| "ZIP end record is missing".to_owned())?;
        if word(bytes, eocd + 4)? != 0 || word(bytes, eocd + 6)? != 0 {
            return Err("multi-disk ZIP archives are not supported".into());
        }
        let entries = usize::from(word(bytes, eocd + 10)?);
        if entries > MAX_ARCHIVE_ENTRIES {
            return Err(format!(
                "archive has {entries} entries; limit is {MAX_ARCHIVE_ENTRIES}"
            ));
        }
        let central_size = dword(bytes, eocd + 12)? as usize;
        let central_offset = dword(bytes, eocd + 16)? as usize;
        let central_end = central_offset
            .checked_add(central_size)
            .ok_or_else(|| "central directory offset overflowed".to_owned())?;
        if central_end > eocd || central_end > bytes.len() {
            return Err("central directory lies outside the archive".into());
        }

        let mut cursor = central_offset;
        let mut total = 0usize;
        let mut result = BTreeMap::new();
        for _ in 0..entries {
            if dword(bytes, cursor)? != 0x0201_4b50 {
                return Err("central directory entry is malformed".into());
            }
            let flags = word(bytes, cursor + 8)?;
            if flags & 1 != 0 {
                return Err("encrypted ZIP members are not supported".into());
            }
            let method = word(bytes, cursor + 10)?;
            if !matches!(method, 0 | 8) {
                return Err(format!("ZIP compression method {method} is not supported"));
            }
            let expected_crc = dword(bytes, cursor + 16)?;
            let compressed = dword(bytes, cursor + 20)? as usize;
            let uncompressed = dword(bytes, cursor + 24)? as usize;
            if uncompressed > MAX_ENTRY_BYTES {
                return Err(format!(
                    "ZIP member exceeds the {} MiB limit",
                    MAX_ENTRY_BYTES / 1024 / 1024
                ));
            }
            total = total
                .checked_add(uncompressed)
                .ok_or_else(|| "ZIP size total overflowed".to_owned())?;
            if total > MAX_TOTAL_BYTES {
                return Err(format!(
                    "ZIP contents exceed the {} MiB limit",
                    MAX_TOTAL_BYTES / 1024 / 1024
                ));
            }
            let name_length = usize::from(word(bytes, cursor + 28)?);
            let extra_length = usize::from(word(bytes, cursor + 30)?);
            let comment_length = usize::from(word(bytes, cursor + 32)?);
            if word(bytes, cursor + 34)? != 0 {
                return Err("ZIP member is stored on another disk".into());
            }
            let external = dword(bytes, cursor + 38)?;
            if ((external >> 16) & 0xf000) == 0xa000 {
                return Err("symbolic links are not supported in mod bundles".into());
            }
            let local_offset = dword(bytes, cursor + 42)? as usize;
            let name_start = cursor + 46;
            let name_end = checked_add(name_start, name_length, bytes.len())?;
            let name = std::str::from_utf8(&bytes[name_start..name_end])
                .map_err(|_| "ZIP member name is not UTF-8".to_owned())?;
            safe_name(name)?;
            if name.ends_with('/') {
                return Err("directory entries are not needed in mod bundles".into());
            }
            let value = extract_member(
                bytes,
                local_offset,
                name,
                flags,
                method,
                compressed,
                uncompressed,
                expected_crc,
            )?;
            if result.insert(name.to_owned(), value).is_some() {
                return Err(format!("ZIP member {name:?} appears more than once"));
            }
            cursor = checked_add(
                name_end,
                extra_length
                    .checked_add(comment_length)
                    .ok_or_else(|| "ZIP metadata length overflowed".to_owned())?,
                central_end,
            )?;
        }
        if cursor != central_end {
            return Err("central directory size does not match its entries".into());
        }
        Ok(Self { entries: result })
    }

    fn manifest(&self) -> Result<Manifest, String> {
        let bytes = self
            .entries
            .get("manifest.json")
            .ok_or_else(|| "manifest.json is missing".to_owned())?;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "manifest exceeds the {} KiB limit",
                MAX_MANIFEST_BYTES / 1024
            ));
        }
        parse_manifest(bytes, "manifest.json")
    }
}

#[allow(clippy::too_many_arguments)]
fn extract_member(
    archive: &[u8],
    local_offset: usize,
    expected_name: &str,
    central_flags: u16,
    method: u16,
    compressed: usize,
    uncompressed: usize,
    expected_crc: u32,
) -> Result<Vec<u8>, String> {
    if dword(archive, local_offset)? != 0x0403_4b50 {
        return Err(format!("{expected_name:?} has no matching local header"));
    }
    let local_flags = word(archive, local_offset + 6)?;
    let local_method = word(archive, local_offset + 8)?;
    if local_flags != central_flags || local_method != method {
        return Err(format!(
            "{expected_name:?} disagrees between local and central headers"
        ));
    }
    let name_length = usize::from(word(archive, local_offset + 26)?);
    let extra_length = usize::from(word(archive, local_offset + 28)?);
    let name_start = local_offset + 30;
    let name_end = checked_add(name_start, name_length, archive.len())?;
    if archive.get(name_start..name_end) != Some(expected_name.as_bytes()) {
        return Err("ZIP local member name does not match its central entry".into());
    }
    let data_start = checked_add(name_end, extra_length, archive.len())?;
    let data_end = checked_add(data_start, compressed, archive.len())?;
    let encoded = &archive[data_start..data_end];
    let value = match method {
        0 => encoded.to_vec(),
        8 => {
            let mut decoder = DeflateDecoder::new(encoded);
            let mut decoded = Vec::with_capacity(uncompressed.min(1024 * 1024));
            decoder
                .by_ref()
                .take(uncompressed as u64 + 1)
                .read_to_end(&mut decoded)
                .map_err(|error| format!("{expected_name:?} could not be inflated: {error}"))?;
            decoded
        }
        _ => unreachable!("the central directory checked the method"),
    };
    if value.len() != uncompressed {
        return Err(format!(
            "{expected_name:?} expands to {} bytes, expected {uncompressed}",
            value.len()
        ));
    }
    let actual_crc = crc32(&value);
    if actual_crc != expected_crc {
        return Err(format!(
            "{expected_name:?} CRC32 is {actual_crc:08x}, expected {expected_crc:08x}"
        ));
    }
    Ok(value)
}

fn parse_manifest(bytes: &[u8], shown: &str) -> Result<Manifest, String> {
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|error| format!("{shown}: {error}"))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &Manifest) -> Result<(), String> {
    if manifest.format != FORMAT {
        return Err(format!(
            "mod format {} is unsupported; expected {FORMAT}",
            manifest.format
        ));
    }
    if manifest.name.is_empty()
        || manifest.name.len() > MAX_NAME
        || manifest.name.chars().any(char::is_control)
    {
        return Err(format!(
            "mod name must contain 1–{MAX_NAME} printable bytes"
        ));
    }
    if manifest.modules.is_empty() || manifest.modules.len() > MAX_MODULES {
        return Err(format!(
            "modules must contain 1–{MAX_MODULES} entries in load order"
        ));
    }
    for module in &manifest.modules {
        safe_name(module)?;
        if !module.ends_with(".wasm") && !module.ends_with(".gwmod") {
            return Err(format!("module {module:?} must end in .wasm or .gwmod"));
        }
    }
    if manifest.modules.last() != Some(&manifest.entry) {
        return Err("entry must be the final item in modules load order".into());
    }
    Ok(())
}

fn safe_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    let safe = !name.is_empty()
        && !name.contains('\\')
        && !name.as_bytes().contains(&0)
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if safe {
        Ok(())
    } else {
        Err(format!("unsafe relative mod path {name:?}"))
    }
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("could not resolve {}: {error}", path.display()))
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > limit as u64 {
        return Err(format!(
            "{} exceeds the {} MiB limit",
            path.display(),
            limit / 1024 / 1024
        ));
    }
    fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))
}

fn depth_guard(depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        Err(format!("mod bundle nesting exceeds {MAX_DEPTH} levels"))
    } else {
        Ok(())
    }
}

fn find_eocd(bytes: &[u8]) -> Option<usize> {
    let minimum = 22usize;
    if bytes.len() < minimum {
        return None;
    }
    let start = bytes.len().saturating_sub(65_535 + minimum);
    (start..=bytes.len() - minimum)
        .rev()
        .find(|offset| bytes.get(*offset..*offset + 4) == Some(&[0x50, 0x4b, 0x05, 0x06]))
}

fn checked_add(start: usize, length: usize, limit: usize) -> Result<usize, String> {
    let end = start
        .checked_add(length)
        .ok_or_else(|| "ZIP offset overflowed".to_owned())?;
    if end > limit {
        Err("ZIP member lies outside the archive".into())
    } else {
        Ok(end)
    }
}

fn word(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| "truncated ZIP field".to_owned())?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn dword(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "truncated ZIP field".to_owned())?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::TempDir;

    const WASM: &[u8] = b"\0asm\x01\0\0\0";

    #[test]
    fn a_bare_module_is_a_valid_explicit_modfile() {
        let temp = TempDir::new("mods-bare");
        let path = temp.0.join("hello.wasm");
        fs::write(&path, WASM).unwrap();
        let catalog = load(&path).unwrap();
        assert_eq!(catalog.name, "hello");
        assert_eq!(catalog.modules.len(), 1);
        assert_eq!(catalog.modules[0].name, "hello.wasm");
    }

    #[test]
    fn a_session_manifest_resolves_dependencies_in_order() {
        let temp = TempDir::new("mods-session");
        fs::write(temp.0.join("library.wasm"), WASM).unwrap();
        let mut entry = WASM.to_vec();
        entry.push(1);
        fs::write(temp.0.join("entry.wasm"), entry).unwrap();
        fs::write(
            temp.0.join("session.json"),
            br#"{"format":1,"name":"tools","entry":"entry.wasm","modules":["library.wasm","entry.wasm"]}"#,
        )
        .unwrap();
        let catalog = load(&temp.0.join("session.json")).unwrap();
        assert_eq!(
            catalog
                .modules
                .iter()
                .map(|module| module.name.as_str())
                .collect::<Vec<_>>(),
            ["library.wasm", "entry.wasm"]
        );
    }

    #[test]
    fn session_paths_cannot_escape_their_directory() {
        let temp = TempDir::new("mods-traversal");
        fs::write(
            temp.0.join("session.json"),
            br#"{"format":1,"name":"bad","entry":"../bad.wasm","modules":["../bad.wasm"]}"#,
        )
        .unwrap();
        assert!(
            load(&temp.0.join("session.json"))
                .unwrap_err()
                .contains("unsafe")
        );
    }

    #[test]
    fn a_stored_gwmod_round_trips_and_discovers() {
        let temp = TempDir::new("mods-bundle");
        let manifest =
            br#"{"format":1,"name":"hello","entry":"hello.wasm","modules":["hello.wasm"]}"#;
        let bundle = stored_zip(&[("manifest.json", manifest), ("hello.wasm", WASM)]);
        fs::write(temp.0.join("hello.gwmod"), bundle).unwrap();
        let catalog = load(&temp.0.join("hello.gwmod")).unwrap();
        assert_eq!(catalog.name, "hello");
        assert_eq!(catalog.modules.len(), 1);
        assert_eq!(
            discover(&temp.0),
            [Summary {
                name: "hello".into(),
                file: "hello.gwmod".into(),
                modules: 1,
                valid: true,
                error: None,
            }]
        );
    }

    #[test]
    fn duplicate_names_and_bad_checksums_are_refused() {
        let manifest =
            br#"{"format":1,"name":"hello","entry":"hello.wasm","modules":["hello.wasm"]}"#;
        let duplicate = stored_zip(&[
            ("manifest.json", manifest),
            ("hello.wasm", WASM),
            ("hello.wasm", WASM),
        ]);
        assert!(
            Archive::parse(&duplicate)
                .unwrap_err()
                .contains("more than once")
        );

        let mut damaged = stored_zip(&[("manifest.json", manifest), ("hello.wasm", WASM)]);
        let position = damaged
            .windows(WASM.len())
            .position(|window| window == WASM)
            .unwrap();
        damaged[position] ^= 1;
        assert!(Archive::parse(&damaged).unwrap_err().contains("CRC32"));
    }

    #[test]
    fn entry_is_last_and_every_module_is_wasm_or_a_bundle() {
        for body in [
            br#"{"format":1,"name":"bad","entry":"a.wasm","modules":["a.wasm","b.wasm"]}"#
                .as_slice(),
            br#"{"format":1,"name":"bad","entry":"readme.txt","modules":["readme.txt"]}"#
                .as_slice(),
        ] {
            assert!(parse_manifest(body, "test").is_err());
        }
    }

    fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut central = Vec::new();
        for (name, value) in entries {
            let offset = bytes.len() as u32;
            let crc = crc32(value);
            push_u32(&mut bytes, 0x0403_4b50);
            push_u16(&mut bytes, 20);
            push_u16(&mut bytes, 0);
            push_u16(&mut bytes, 0);
            push_u16(&mut bytes, 0);
            push_u16(&mut bytes, 0);
            push_u32(&mut bytes, crc);
            push_u32(&mut bytes, value.len() as u32);
            push_u32(&mut bytes, value.len() as u32);
            push_u16(&mut bytes, name.len() as u16);
            push_u16(&mut bytes, 0);
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(value);

            push_u32(&mut central, 0x0201_4b50);
            push_u16(&mut central, 20);
            push_u16(&mut central, 20);
            push_u16(&mut central, 0);
            push_u16(&mut central, 0);
            push_u16(&mut central, 0);
            push_u16(&mut central, 0);
            push_u32(&mut central, crc);
            push_u32(&mut central, value.len() as u32);
            push_u32(&mut central, value.len() as u32);
            push_u16(&mut central, name.len() as u16);
            push_u16(&mut central, 0);
            push_u16(&mut central, 0);
            push_u16(&mut central, 0);
            push_u16(&mut central, 0);
            push_u32(&mut central, 0);
            push_u32(&mut central, offset);
            central.extend_from_slice(name.as_bytes());
        }
        let central_offset = bytes.len() as u32;
        bytes.extend_from_slice(&central);
        push_u32(&mut bytes, 0x0605_4b50);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, entries.len() as u16);
        push_u16(&mut bytes, entries.len() as u16);
        push_u32(&mut bytes, central.len() as u32);
        push_u32(&mut bytes, central_offset);
        push_u16(&mut bytes, 0);
        bytes
    }

    fn push_u16(bytes: &mut Vec<u8>, value: u16) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}
