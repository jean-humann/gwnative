"""Dependency-free JSPI and WebAssembly artifact inspection.

The tools in ``scripts/client-*`` intentionally report metadata, hashes and
interface names only. They never copy a client artifact into the repository or
emit code/data section contents.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1
SECTION_NAMES = {
    0: "custom",
    1: "type",
    2: "import",
    3: "function",
    4: "table",
    5: "memory",
    6: "global",
    7: "export",
    8: "start",
    9: "element",
    10: "code",
    11: "data",
    12: "data-count",
    13: "tag",
}
IMPORT_KINDS = {0: "function", 1: "table", 2: "memory", 3: "global", 4: "tag"}
EXPORT_KINDS = IMPORT_KINDS
MODULE_PATH = re.compile(
    rb"""\bModule(?:(?:\.[A-Za-z_$][\w$]*)|(?:\[\s*(['"])[^'"]+\1\s*\]))+"""
)
MODULE_PATH_SEGMENT = re.compile(
    rb"""(?:\.([A-Za-z_$][\w$]*))|(?:\[\s*(['"])([^'"]+)\2\s*\])"""
)


class AnalysisError(ValueError):
    """An artifact is malformed or not the requested format."""


@dataclass
class Reader:
    data: bytes
    offset: int = 0

    def remaining(self) -> int:
        return len(self.data) - self.offset

    def byte(self) -> int:
        if self.offset >= len(self.data):
            raise AnalysisError("unexpected end of WebAssembly input")
        value = self.data[self.offset]
        self.offset += 1
        return value

    def take(self, size: int) -> bytes:
        if size < 0 or self.offset + size > len(self.data):
            raise AnalysisError("WebAssembly field extends past its section")
        value = self.data[self.offset : self.offset + size]
        self.offset += size
        return value

    def uleb(self, maximum_bytes: int = 10) -> int:
        value = 0
        shift = 0
        for _ in range(maximum_bytes):
            byte = self.byte()
            value |= (byte & 0x7F) << shift
            if byte & 0x80 == 0:
                return value
            shift += 7
        raise AnalysisError("WebAssembly integer is too large")

    def name(self) -> str:
        raw = self.take(self.uleb(5))
        try:
            return raw.decode("utf-8")
        except UnicodeDecodeError as error:
            raise AnalysisError("WebAssembly name is not UTF-8") from error


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def _limits(reader: Reader) -> None:
    flags = reader.uleb(5)
    width = 10 if flags & 0x04 else 5
    reader.uleb(width)
    if flags & 0x01:
        reader.uleb(width)


def _table_type(reader: Reader) -> None:
    reader.byte()
    _limits(reader)


def _global_type(reader: Reader) -> None:
    reader.byte()
    reader.byte()


def _section_count(payload: bytes) -> int:
    return Reader(payload).uleb(5)


def _imports(payload: bytes) -> tuple[list[dict[str, str]], Counter[str]]:
    reader = Reader(payload)
    imports: list[dict[str, str]] = []
    counts: Counter[str] = Counter()
    for _ in range(reader.uleb(5)):
        module = reader.name()
        name = reader.name()
        kind_id = reader.byte()
        kind = IMPORT_KINDS.get(kind_id, f"unknown-{kind_id}")
        counts[kind] += 1
        imports.append({"module": module, "name": name, "kind": kind})
        if kind_id == 0:
            reader.uleb(5)
        elif kind_id == 1:
            _table_type(reader)
        elif kind_id == 2:
            _limits(reader)
        elif kind_id == 3:
            _global_type(reader)
        elif kind_id == 4:
            reader.byte()
            reader.uleb(5)
        else:
            raise AnalysisError(f"unknown WebAssembly import kind {kind_id}")
    if reader.remaining():
        raise AnalysisError("trailing bytes in WebAssembly import section")
    return imports, counts


def _exports(payload: bytes) -> list[dict[str, Any]]:
    reader = Reader(payload)
    exports: list[dict[str, Any]] = []
    for _ in range(reader.uleb(5)):
        name = reader.name()
        kind_id = reader.byte()
        exports.append(
            {
                "name": name,
                "kind": EXPORT_KINDS.get(kind_id, f"unknown-{kind_id}"),
                "index": reader.uleb(5),
            }
        )
    if reader.remaining():
        raise AnalysisError("trailing bytes in WebAssembly export section")
    return exports


def _code(payload: bytes) -> list[str]:
    reader = Reader(payload)
    hashes: list[str] = []
    for _ in range(reader.uleb(5)):
        body = reader.take(reader.uleb(5))
        hashes.append(sha256_bytes(body))
    if reader.remaining():
        raise AnalysisError("trailing bytes in WebAssembly code section")
    return hashes


def inspect_wasm(path: Path) -> dict[str, Any]:
    data = path.read_bytes()
    if len(data) < 8 or data[:4] != b"\0asm":
        raise AnalysisError(f"{path} is not a WebAssembly module")
    version = int.from_bytes(data[4:8], "little")
    if version != 1:
        raise AnalysisError(f"{path} uses unsupported WebAssembly version {version}")

    reader = Reader(data, 8)
    sections: list[dict[str, Any]] = []
    imports: list[dict[str, str]] = []
    import_counts: Counter[str] = Counter()
    exports: list[dict[str, Any]] = []
    function_body_hashes: list[str] = []
    while reader.remaining():
        section_offset = reader.offset
        section_id = reader.byte()
        payload = reader.take(reader.uleb(5))
        entry: dict[str, Any] = {
            "id": section_id,
            "name": SECTION_NAMES.get(section_id, f"unknown-{section_id}"),
            "offset": section_offset,
            "size": len(payload),
        }
        if section_id == 0:
            entry["customName"] = Reader(payload).name()
        elif section_id == 2:
            imports, import_counts = _imports(payload)
            entry["count"] = len(imports)
        elif section_id == 7:
            exports = _exports(payload)
            entry["count"] = len(exports)
        elif section_id == 10:
            function_body_hashes = _code(payload)
            entry["count"] = len(function_body_hashes)
        elif section_id in {1, 3, 4, 5, 6, 9, 11, 12, 13}:
            entry["count"] = _section_count(payload)
        sections.append(entry)

    return {
        "path": str(path),
        "sha256": sha256_bytes(data),
        "size": len(data),
        "version": version,
        "sections": sections,
        "imports": imports,
        "importCounts": dict(sorted(import_counts.items())),
        "exports": exports,
        "functionBodyHashes": function_body_hashes,
    }


def inspect_jspi(path: Path) -> dict[str, Any]:
    data = path.read_bytes()
    paths = set()
    for path_match in MODULE_PATH.finditer(data):
        segments = [
            (segment.group(1) or segment.group(3)).decode("utf-8", "replace")
            for segment in MODULE_PATH_SEGMENT.finditer(path_match.group())
        ]
        if segments:
            paths.add(".".join(segments))
    properties = {path.partition(".")[0] for path in paths}
    return {
        "path": str(path),
        "sha256": sha256_bytes(data),
        "size": len(data),
        "moduleProperties": sorted(properties),
        "modulePaths": sorted(paths),
        "jspi": {
            "suspending": b"WebAssembly.Suspending" in data,
            "promising": b"WebAssembly.promising" in data,
        },
    }


def inspect_pair(wasm: Path, jspi: Path | None = None) -> dict[str, Any]:
    report: dict[str, Any] = {
        "schemaVersion": SCHEMA_VERSION,
        "wasm": inspect_wasm(wasm),
    }
    if jspi is not None:
        report["jspi"] = inspect_jspi(jspi)
    return report


def _section_signature(section: dict[str, Any]) -> tuple[int, str]:
    return section["id"], section.get("customName", "")


def _named_imports(values: list[dict[str, Any]]) -> set[str]:
    return {
        f"{value['kind']}:{value['module']}.{value['name']}" for value in values
    }


def _named_exports(values: list[dict[str, Any]]) -> set[str]:
    return {f"{value['kind']}:{value['name']}" for value in values}


def _retargeted_exports(
    before: list[dict[str, Any]], after: list[dict[str, Any]]
) -> list[dict[str, Any]]:
    old = {
        (value["kind"], value["name"]): value["index"]
        for value in before
    }
    new = {
        (value["kind"], value["name"]): value["index"]
        for value in after
    }
    return [
        {
            "export": f"{kind}:{name}",
            "beforeIndex": old[(kind, name)],
            "afterIndex": new[(kind, name)],
        }
        for kind, name in sorted(old.keys() & new.keys())
        if old[(kind, name)] != new[(kind, name)]
    ]


def diff_reports(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    old = before["wasm"]
    new = after["wasm"]
    old_bodies = old["functionBodyHashes"]
    new_bodies = new["functionBodyHashes"]
    old_counts = Counter(old_bodies)
    new_counts = Counter(new_bodies)
    shared_bodies = sum((old_counts & new_counts).values())
    same_index = sum(left == right for left, right in zip(old_bodies, new_bodies))
    old_unique = {
        value: index
        for index, value in enumerate(old_bodies)
        if old_counts[value] == 1
    }
    shifts = Counter(
        index - old_unique[value]
        for index, value in enumerate(new_bodies)
        if new_counts[value] == 1 and value in old_unique
    )
    dominant_shift = None
    if shifts:
        delta, count = shifts.most_common(1)[0]
        dominant_shift = {"delta": delta, "matchingFunctions": count}

    old_sections = {_section_signature(value): value for value in old["sections"]}
    new_sections = {_section_signature(value): value for value in new["sections"]}
    section_changes = []
    for key in sorted(old_sections.keys() | new_sections.keys()):
        left = old_sections.get(key)
        right = new_sections.get(key)
        section_changes.append(
            {
                "id": key[0],
                "name": (right or left)["name"],
                **({"customName": key[1]} if key[1] else {}),
                "beforeSize": left["size"] if left else None,
                "afterSize": right["size"] if right else None,
                "beforeCount": left.get("count") if left else None,
                "afterCount": right.get("count") if right else None,
            }
        )

    result: dict[str, Any] = {
        "schemaVersion": SCHEMA_VERSION,
        "wasm": {
            "beforeSha256": old["sha256"],
            "afterSha256": new["sha256"],
            "beforeSize": old["size"],
            "afterSize": new["size"],
            "functionBodies": {
                "before": len(old_bodies),
                "after": len(new_bodies),
                "sharedExact": shared_bodies,
                "sameIndexExact": same_index,
                "dominantUniqueBodyShift": dominant_shift,
            },
            "sections": section_changes,
            "importsAdded": sorted(
                _named_imports(new["imports"]) - _named_imports(old["imports"])
            ),
            "importsRemoved": sorted(
                _named_imports(old["imports"]) - _named_imports(new["imports"])
            ),
            "exportsAdded": sorted(
                _named_exports(new["exports"]) - _named_exports(old["exports"])
            ),
            "exportsRemoved": sorted(
                _named_exports(old["exports"]) - _named_exports(new["exports"])
            ),
            "exportsRetargeted": _retargeted_exports(
                old["exports"], new["exports"]
            ),
        },
    }
    if "jspi" in before and "jspi" in after:
        old_properties = set(before["jspi"]["moduleProperties"])
        new_properties = set(after["jspi"]["moduleProperties"])
        old_paths = set(before["jspi"]["modulePaths"])
        new_paths = set(after["jspi"]["modulePaths"])
        result["jspi"] = {
            "beforeSha256": before["jspi"]["sha256"],
            "afterSha256": after["jspi"]["sha256"],
            "modulePropertiesAdded": sorted(new_properties - old_properties),
            "modulePropertiesRemoved": sorted(old_properties - new_properties),
            "modulePathsAdded": sorted(new_paths - old_paths),
            "modulePathsRemoved": sorted(old_paths - new_paths),
            "beforeCapabilities": before["jspi"]["jspi"],
            "afterCapabilities": after["jspi"]["jspi"],
        }
    return result


def _registry(source: Path) -> dict[str, Any]:
    text = source.read_text()
    template_text = text.split("pub(super) const BUILDS:", 1)[1].split(
        "pub(super) fn find_build", 1
    )[0]
    enhancement_text = text.split(
        "pub(super) const ENHANCEMENT_BUILDS:", 1
    )[1].split("pub(super) fn find_enhancement_build", 1)[0]
    template_pairs = re.findall(
        r'sha256:\s*"([0-9a-f]{64})".*?output_sha256:\s*"([0-9a-f]{64})"',
        template_text,
        re.S,
    )
    enhancement_records = re.findall(
        r'sha256:\s*"([0-9a-f]{64})".*?'
        r'output_sha256:\s*"([0-9a-f]{64})".*?'
        r"program_id:\s*(\d+).*?build_id:\s*(\d+).*?"
        r"hook_function:\s*(\d+)",
        enhancement_text,
        re.S,
    )
    return {
        "templates": [
            {"inputSha256": source_hash, "outputSha256": output_hash}
            for source_hash, output_hash in template_pairs
        ],
        "enhancements": [
            {
                "inputSha256": source_hash,
                "outputSha256": output_hash,
                "programId": int(program_id),
                "buildId": int(build_id),
                "hookFunction": int(hook),
            }
            for source_hash, output_hash, program_id, build_id, hook in enhancement_records
        ],
    }


def _output_check(
    path: Path | None, actual: str | None, expected: str | None
) -> dict[str, Any] | None:
    if path is None:
        return None
    return {
        "path": str(path),
        "sha256": actual,
        "expectedSha256": expected,
        "matches": actual == expected,
    }


def certify(
    wasm: Path,
    registry_source: Path,
    template_output: Path | None = None,
    enhanced_output: Path | None = None,
) -> tuple[dict[str, Any], bool]:
    registry = _registry(registry_source)
    source_hash = sha256_file(wasm)
    template = next(
        (
            record
            for record in registry["templates"]
            if record["inputSha256"] == source_hash
        ),
        None,
    )
    enhancement = (
        next(
            (
                record
                for record in registry["enhancements"]
                if template and record["inputSha256"] == template["outputSha256"]
            ),
            None,
        )
        if template
        else next(
            (
                record
                for record in registry["enhancements"]
                if record["inputSha256"] == source_hash
            ),
            None,
        )
    )
    supplied_template_hash = (
        sha256_file(template_output) if template_output is not None else None
    )
    supplied_enhanced_hash = (
        sha256_file(enhanced_output) if enhanced_output is not None else None
    )
    certified = template is not None or enhancement is not None
    checks_pass = certified
    if template_output is not None:
        checks_pass = (
            checks_pass
            and template is not None
            and supplied_template_hash == template["outputSha256"]
        )
    if enhanced_output is not None:
        checks_pass = (
            checks_pass
            and enhancement is not None
            and supplied_enhanced_hash == enhancement["outputSha256"]
        )
    return (
        {
            "schemaVersion": SCHEMA_VERSION,
            "source": {"path": str(wasm), "sha256": source_hash},
            "certified": certified,
            "template": template,
            "enhancement": enhancement,
            "checks": {
                "templateOutput": _output_check(
                    template_output,
                    supplied_template_hash,
                    template["outputSha256"] if template else None,
                ),
                "enhancedOutput": _output_check(
                    enhanced_output,
                    supplied_enhanced_hash,
                    enhancement["outputSha256"] if enhancement else None,
                ),
            },
        },
        checks_pass,
    )


def _write(report: dict[str, Any], as_json: bool, summary: str) -> None:
    if as_json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(summary)


def inspect_main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Inspect a JSPI/WASM client pair without emitting client code"
    )
    parser.add_argument("wasm", type=Path)
    parser.add_argument("--jspi", type=Path)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        report = inspect_pair(args.wasm, args.jspi)
    except (OSError, AnalysisError) as error:
        parser.error(str(error))
    wasm = report["wasm"]
    summary = (
        f"WASM {wasm['sha256']} ({wasm['size']} bytes): "
        f"{len(wasm['functionBodyHashes'])} defined functions, "
        f"{len(wasm['imports'])} imports, {len(wasm['exports'])} exports"
    )
    if "jspi" in report:
        jspi = report["jspi"]
        summary += (
            f"\nJSPI {jspi['sha256']} ({jspi['size']} bytes): "
            f"{len(jspi['moduleProperties'])} Module properties, "
            f"{len(jspi['modulePaths'])} referenced paths"
        )
    _write(report, args.json, summary)
    return 0


def diff_main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Structurally compare two JSPI/WASM client pairs"
    )
    parser.add_argument("before_wasm", type=Path)
    parser.add_argument("after_wasm", type=Path)
    parser.add_argument("--before-jspi", type=Path)
    parser.add_argument("--after-jspi", type=Path)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if (args.before_jspi is None) != (args.after_jspi is None):
        parser.error("--before-jspi and --after-jspi must be supplied together")
    try:
        report = diff_reports(
            inspect_pair(args.before_wasm, args.before_jspi),
            inspect_pair(args.after_wasm, args.after_jspi),
        )
    except (OSError, AnalysisError) as error:
        parser.error(str(error))
    bodies = report["wasm"]["functionBodies"]
    shift = bodies["dominantUniqueBodyShift"]
    shift_text = (
        f", dominant index shift {shift['delta']:+d} across "
        f"{shift['matchingFunctions']} unique bodies"
        if shift
        else ""
    )
    summary = (
        f"Functions: {bodies['before']} -> {bodies['after']}; "
        f"{bodies['sharedExact']} exact bodies shared, "
        f"{bodies['sameIndexExact']} unchanged at the same index{shift_text}"
    )
    _write(report, args.json, summary)
    return 0


def certify_main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Check client and derived hashes against the fail-closed registry"
    )
    parser.add_argument("wasm", type=Path)
    parser.add_argument(
        "--registry",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "src/wasm/builds.rs",
    )
    parser.add_argument("--template-output", type=Path)
    parser.add_argument("--enhanced-output", type=Path)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        report, passed = certify(
            args.wasm,
            args.registry,
            args.template_output,
            args.enhanced_output,
        )
    except (OSError, AnalysisError, IndexError) as error:
        parser.error(str(error))
    enhancement = report["enhancement"]
    if report["certified"]:
        summary = f"certified source {report['source']['sha256']}"
        if enhancement:
            summary += (
                f"; program {enhancement['programId']} build "
                f"{enhancement['buildId']}, hook {enhancement['hookFunction']}"
            )
    else:
        summary = f"uncertified source {report['source']['sha256']}"
    for name, check in report["checks"].items():
        if check is not None:
            summary += f"\n{name}: {'PASS' if check['matches'] else 'FAIL'}"
    _write(report, args.json, summary)
    return 0 if passed else 2
