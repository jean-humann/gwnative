# Mods

gwnative supports the `.gwmod` session format established by
[gw_in_browser](https://github.com/gwdevhub/gw_in_browser). Loading is strictly
opt-in: discovery never executes a module, and no mod is loaded unless the
player supplies `-modfile`.

```sh
gwnative mods
gwnative -modfile tools.gwmod
gwnative -modfile session.json
gwnative -modfile one-module.wasm
```

Mods execute in the game WebAssembly realm and can share its linear memory.
They are native-equivalent trusted code, not sandboxed add-ons. Only load
modules whose source and publisher you trust.

## Manifest format

A format-1 manifest is valid both as `manifest.json` inside a `.gwmod` ZIP and
as an explicit session JSON file:

```json
{
  "format": 1,
  "name": "Example tools",
  "entry": "main.wasm",
  "modules": [
    "support.wasm",
    "main.wasm"
  ]
}
```

`modules` is dependency-first load order. `entry` must be its final item. A
member may be another `.gwmod`, in which case its graph is flattened at that
position. Repeated bundles and repeated module content are loaded once.

Archive paths must be simple relative paths using forward slashes. Absolute
paths, `..`, backslashes, NUL bytes, directory entries, and symlinks are
rejected. Modules must be WebAssembly 1 binaries and must end in `.wasm`.

## Validation limits

The Rust host validates the complete selection before opening WebKit:

| Limit | Value |
| --- | --- |
| Compressed bundle | 64 MiB |
| One expanded member | 64 MiB |
| All resolved module bytes | 128 MiB |
| Manifest | 64 KiB |
| Archive entries | 128 |
| Resolved modules | 64 |
| Nesting | 8 levels |
| Mod name | 128 bytes |

ZIP64, multi-disk archives, encrypted members, data outside the archive,
unsupported compression, conflicting local/central headers, size mismatches,
and CRC mismatches are refused. Stored and Deflate members are supported.

Each resolved module receives a SHA-256 digest. The token-gated loopback catalog
publishes the expected name, index, length, digest, and relative URL. The page
checks all metadata, downloads the module, verifies its length and SHA-256
again, and only then compiles it.

## Runtime contract

Modules are instantiated in manifest order. The runtime can resolve:

- the running game's exported functions, memory, and table;
- exports from earlier modules;
- mutable `GOT.mem` globals;
- bounded host log and diagnostic-alert strings;
- JSPI-aware `emscripten_sleep` where WebKit supports it;
- table publish/adopt helpers; and
- conservative `wasi_snapshot_preview1` stubs returning `ENOSYS`.

After normal WebAssembly initialization, the entry point is:

```c
void mod_init(uint32_t game_wasm_pointer, uint32_t game_wasm_length);
```

The host provides a copy of the current game module when the client allocator
can safely hold it; otherwise both arguments are zero. A missing `mod_init`,
unresolved import, failed hash, or initialization error aborts the selected mod
session and is reported in diagnostics.

The page also exposes `window.GwInject.load()` and
`window.GwInject.selected()` for the one already host-approved catalog. This
does not permit runtime selection of arbitrary paths or URLs.

## Discovery

`gwnative mods` inspects at most 256 top-level `.gwmod` files in the selected
mod directory and prints their manifest validity. It does not recurse, compile,
instantiate, or call module code.

The default directory is:

```text
~/Library/Application Support/gwnative/mods
```

Override it with `--mods PATH`. A selected `-modfile` may live elsewhere; it is
canonicalized before validation.
