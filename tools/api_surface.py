"""Inventory public Guild Wars interoperability surfaces without copying code.

The report contains source revisions, interface names, and artifact metadata.
It deliberately excludes native signatures, assertion strings, WebAssembly
function bodies, data segments, and implementation source.
"""

from __future__ import annotations

import argparse
import ast
import json
import re
import subprocess
from collections import defaultdict
from pathlib import Path
from typing import Any

from tools.client_analyzer import AnalysisError, inspect_jspi, inspect_wasm

SCHEMA_VERSION = 1
GWCA_FUNCTION = re.compile(
    r"\bGWCA_API\b(?P<declaration>[^;{}]*?\b[A-Za-z_]\w*\s*\()",
    re.S,
)
FUNCTION_NAME = re.compile(r"([A-Za-z_]\w*)\s*\($")
EMBEDDED_MODULE = re.compile(
    r"\bPYBIND11_EMBEDDED_MODULE\s*\(\s*([A-Za-z_]\w*)\s*,"
)
BINDING_NAME = re.compile(
    r"\.(?:def|def_static|def_property|def_property_readonly|"
    r"def_readwrite|def_readonly)\s*\(\s*\"([^\"]+)\""
)
RUST_FIELD = re.compile(r"^\s*pub\s+([a-zA-Z_]\w*)\s*:", re.MULTILINE)
VERSION = re.compile(r'#define\s+GWCA_VERSION\s+"([^"]+)"')
CERTIFIED_DOMAIN_FIELDS = {
    "agent": "agents",
    "camera": "camera",
    "completion": "completion",
    "effects": "effects",
    "friend_list": "social",
    "guild": "social",
    "item": "inventory",
    "map": "map_id",
    "merchant": "merchant",
    "party": "party",
    "player": "player_id",
    "progression": "progression",
    "quest": "quests",
    "skillbar": "skillbar",
    "target": "target_valid",
    "trade": "trade",
    "ui": "ui",
}


class SurfaceError(ValueError):
    """A source tree cannot be inventoried safely."""


def _revision(root: Path) -> str | None:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    return result.stdout.strip() or None


def _dirty(root: Path) -> bool | None:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "status", "--porcelain"],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    return bool(result.stdout.strip())


def _source(root: Path) -> dict[str, Any]:
    if not root.is_dir():
        raise SurfaceError(f"{root} is not a directory")
    return {
        "path": str(root.resolve()),
        "revision": _revision(root),
        "dirty": _dirty(root),
    }


def _without_comments(source: str) -> str:
    source = re.sub(r"/\*.*?\*/", "", source, flags=re.S)
    return re.sub(r"//.*?$", "", source, flags=re.M)


def inspect_gwca(toolbox: Path) -> dict[str, Any]:
    report = _source(toolbox)
    include = toolbox / "Dependencies/GWCA/include/GWCA"
    managers = include / "Managers"
    version_path = include / "GWCAVersion.h"
    if not managers.is_dir() or not version_path.is_file():
        raise SurfaceError(f"{toolbox} has no packaged GWCA headers")

    surfaces: dict[str, list[str]] = {}
    for header in sorted(managers.glob("*.h")):
        source = _without_comments(header.read_text(errors="replace"))
        names = set()
        for match in GWCA_FUNCTION.finditer(source):
            declaration = match.group("declaration").rstrip()
            name_match = FUNCTION_NAME.search(declaration)
            if name_match:
                names.add(name_match.group(1))
        surfaces[header.stem] = sorted(names)

    version_match = VERSION.search(version_path.read_text(errors="replace"))
    report.update(
        {
            "license": "MIT",
            "gwcaVersion": version_match.group(1) if version_match else None,
            "managerFunctions": surfaces,
            "totals": {
                "managers": len(surfaces),
                "functions": sum(len(names) for names in surfaces.values()),
            },
        }
    )
    return report


def _stub_class(node: ast.ClassDef) -> dict[str, list[str]]:
    methods: set[str] = set()
    attributes: set[str] = set()
    for child in node.body:
        if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
            methods.add(child.name)
        elif isinstance(child, ast.AnnAssign) and isinstance(child.target, ast.Name):
            attributes.add(child.target.id)
        elif isinstance(child, ast.Assign):
            attributes.update(
                target.id for target in child.targets if isinstance(target, ast.Name)
            )
    return {"methods": sorted(methods), "attributes": sorted(attributes)}


def inspect_py4gw(py4gw: Path) -> dict[str, Any]:
    report = _source(py4gw)
    stubs_root = py4gw / "stubs"
    if not stubs_root.is_dir():
        raise SurfaceError(f"{py4gw} has no stubs directory")

    stubs: dict[str, dict[str, Any]] = {}
    total_functions = 0
    total_classes = 0
    total_methods = 0
    for path in sorted(stubs_root.glob("*.pyi")):
        try:
            tree = ast.parse(path.read_text(), filename=str(path))
        except (OSError, SyntaxError) as error:
            raise SurfaceError(f"cannot parse {path}: {error}") from error
        functions = sorted(
            {
                node.name
                for node in tree.body
                if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            }
        )
        classes = {
            node.name: _stub_class(node)
            for node in tree.body
            if isinstance(node, ast.ClassDef)
        }
        stubs[path.stem] = {"functions": functions, "classes": classes}
        total_functions += len(functions)
        total_classes += len(classes)
        total_methods += sum(len(value["methods"]) for value in classes.values())

    report.update(
        {
            "license": "see upstream repository",
            "stubs": stubs,
            "totals": {
                "modules": len(stubs),
                "functions": total_functions,
                "classes": total_classes,
                "methods": total_methods,
            },
        }
    )
    return report


def inspect_py4gw_native(native: Path) -> dict[str, Any]:
    report = _source(native)
    offsets_root = native / "offsets"
    if not offsets_root.is_dir():
        raise SurfaceError(f"{native} has no offsets directory")

    offsets: dict[str, dict[str, list[str]]] = {}
    for path in sorted(offsets_root.glob("*.json")):
        try:
            value = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError) as error:
            raise SurfaceError(f"cannot parse {path}: {error}") from error
        if not isinstance(value, dict):
            raise SurfaceError(f"{path} does not contain a JSON object")
        namespace = value.get("namespace")
        patterns = value.get("patterns", {})
        resolvers = value.get("resolvers", {})
        if (
            not isinstance(namespace, str)
            or not isinstance(patterns, dict)
            or not isinstance(resolvers, dict)
        ):
            raise SurfaceError(f"{path} has an unexpected offset schema")
        offsets[namespace] = {
            "patterns": sorted(str(name) for name in patterns),
            "resolvers": sorted(str(name) for name in resolvers),
        }

    bindings: dict[str, list[str]] = defaultdict(list)
    for path in sorted(native.rglob("*bindings.cpp")):
        source = _without_comments(path.read_text(errors="replace"))
        modules = list(EMBEDDED_MODULE.finditer(source))
        if len(modules) == 1:
            bindings[modules[0].group(1)].extend(BINDING_NAME.findall(source))
            continue
        for index, module in enumerate(modules):
            end = modules[index + 1].start() if index + 1 < len(modules) else None
            bindings[module.group(1)].extend(
                BINDING_NAME.findall(source[module.start() : end])
            )
    bindings = {
        module: sorted(set(names)) for module, names in sorted(bindings.items())
    }

    report.update(
        {
            "license": "Apache-2.0",
            "offsets": offsets,
            "bindings": bindings,
            "totals": {
                "offsetNamespaces": len(offsets),
                "patterns": sum(
                    len(value["patterns"]) for value in offsets.values()
                ),
                "resolvers": sum(
                    len(value["resolvers"]) for value in offsets.values()
                ),
                "bindingModules": len(bindings),
                "bindingNames": sum(len(names) for names in bindings.values()),
            },
        }
    )
    return report


def _rust_struct(source: str, declaration: str) -> list[str]:
    start = source.find(declaration)
    if start < 0:
        raise SurfaceError(f"cannot find {declaration}")
    opening = source.find("{", start + len(declaration))
    if opening < 0:
        raise SurfaceError(f"{declaration} has no body")
    depth = 0
    for index in range(opening, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return RUST_FIELD.findall(source[opening + 1 : index])
    raise SurfaceError(f"{declaration} has an unterminated body")


def _wasm_surface(path: Path) -> dict[str, Any]:
    report = inspect_wasm(path)
    return {
        "path": report["path"],
        "sha256": report["sha256"],
        "size": report["size"],
        "definedFunctions": len(report["functionBodyHashes"]),
        "importCounts": report["importCounts"],
        "imports": report["imports"],
        "exports": report["exports"],
        "customSections": [
            section["customName"]
            for section in report["sections"]
            if section["id"] == 0
        ],
    }


def _interface_name(value: dict[str, Any], imported: bool) -> str:
    if imported:
        return f"{value['kind']}:{value['module']}.{value['name']}"
    return f"{value['kind']}:{value['name']}"


def inspect_gwnative(
    root: Path,
    jspi_wasm: Path | None = None,
    jspi_js: Path | None = None,
    asyncify_wasm: Path | None = None,
) -> dict[str, Any]:
    report = _source(root)
    game_api = (root / "src/game_api.rs").read_text()
    builds = (root / "src/wasm/builds.rs").read_text()
    state_fields = _rust_struct(game_api, "pub struct State")
    report.update(
        {
            "license": "GPL-2.0-or-later",
            "stateFields": state_fields,
            "certifiedDomains": sorted(
                domain
                for domain, field in CERTIFIED_DOMAIN_FIELDS.items()
                if field in state_fields
            ),
            "enhancementLayoutFields": _rust_struct(
                builds, "pub(super) struct EnhancementLayout"
            ),
            "actionsCertified": False,
        }
    )

    if jspi_wasm is not None:
        report["jspiWasm"] = _wasm_surface(jspi_wasm)
    if jspi_js is not None:
        report["jspiHost"] = inspect_jspi(jspi_js)
    if asyncify_wasm is not None:
        report["asyncifyWasm"] = _wasm_surface(asyncify_wasm)
    if jspi_wasm is not None and asyncify_wasm is not None:
        jspi = report["jspiWasm"]
        asyncify = report["asyncifyWasm"]
        for imported, key in ((True, "imports"), (False, "exports")):
            old = {
                _interface_name(value, imported) for value in jspi[key]
            }
            new = {
                _interface_name(value, imported) for value in asyncify[key]
            }
            noun = key.capitalize()
            report.setdefault("clientContractComparison", {}).update(
                {
                    f"shared{noun}": len(old & new),
                    f"jspiOnly{noun}": sorted(old - new),
                    f"asyncifyOnly{noun}": sorted(new - old),
                }
            )
    return report


def _domain(value: str) -> str:
    if value == "Py4GW":
        return "runtime"
    value = value.removeprefix("Py").removesuffix("Mgr")
    value = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1_\2", value)
    value = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", value).lower()
    aliases = {
        "agents": "agent",
        "effects": "effect",
        "events": "event",
        "friendlist": "friend_list",
        "im_gui": "imgui",
        "items": "item",
        "inventory": "item",
        "maps": "map",
        "module": "runtime",
        "players": "player",
        "quests": "quest",
        "stoc": "sto_c",
        "u_i": "ui",
        "u_i_manager": "ui",
        "ui_manager": "ui",
    }
    return aliases.get(value, value)


def _domain_index(
    gwca: dict[str, Any],
    py4gw: dict[str, Any],
    native: dict[str, Any],
    gwnative: dict[str, Any],
) -> dict[str, dict[str, Any]]:
    index: defaultdict[str, dict[str, Any]] = defaultdict(
        lambda: {
            "gwcaManagers": [],
            "py4gwStubs": [],
            "nativeBindings": [],
            "nativeOffsets": [],
            "gwnativeCertified": False,
        }
    )
    for name in gwca["managerFunctions"]:
        index[_domain(name)]["gwcaManagers"].append(name)
    for name in py4gw["stubs"]:
        index[_domain(name)]["py4gwStubs"].append(name)
    for name in native["bindings"]:
        index[_domain(name)]["nativeBindings"].append(name)
    for name in native["offsets"]:
        index[_domain(name)]["nativeOffsets"].append(name)
    for name in gwnative["certifiedDomains"]:
        index[_domain(name)]["gwnativeCertified"] = True
    return {
        domain: {
            key: sorted(value) if isinstance(value, list) else value
            for key, value in fields.items()
        }
        for domain, fields in sorted(index.items())
    }


def build_report(
    root: Path,
    toolbox: Path,
    py4gw: Path,
    native: Path,
    jspi_wasm: Path | None = None,
    jspi_js: Path | None = None,
    asyncify_wasm: Path | None = None,
) -> dict[str, Any]:
    sources = {
        "gwtoolbox": inspect_gwca(toolbox),
        "py4gw": inspect_py4gw(py4gw),
        "py4gwNative": inspect_py4gw_native(native),
        "gwnative": inspect_gwnative(
            root, jspi_wasm, jspi_js, asyncify_wasm
        ),
    }
    return {
        "schemaVersion": SCHEMA_VERSION,
        "policy": {
            "namesAndMetadataOnly": True,
            "nativeSignaturesIncluded": False,
            "wasmBodiesIncluded": False,
            "offsetValuesIncluded": False,
        },
        "sources": sources,
        "domains": _domain_index(
            sources["gwtoolbox"],
            sources["py4gw"],
            sources["py4gwNative"],
            sources["gwnative"],
        ),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Map public GWToolbox, Py4GW, JSPI, WASM, and gwnative interfaces"
    )
    parser.add_argument(
        "--gwnative",
        type=Path,
        default=Path(__file__).resolve().parents[1],
    )
    parser.add_argument("--gwtoolbox", type=Path, required=True)
    parser.add_argument("--py4gw", type=Path, required=True)
    parser.add_argument("--py4gw-native", type=Path, required=True)
    parser.add_argument("--jspi-wasm", type=Path)
    parser.add_argument("--jspi-js", type=Path)
    parser.add_argument("--asyncify-wasm", type=Path)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        report = build_report(
            args.gwnative,
            args.gwtoolbox,
            args.py4gw,
            args.py4gw_native,
            args.jspi_wasm,
            args.jspi_js,
            args.asyncify_wasm,
        )
    except (OSError, AnalysisError, SurfaceError) as error:
        parser.error(str(error))
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        sources = report["sources"]
        print(
            "GWCA "
            f"{sources['gwtoolbox']['gwcaVersion']}: "
            f"{sources['gwtoolbox']['totals']['functions']} manager functions"
        )
        print(
            "Py4GW: "
            f"{sources['py4gw']['totals']['modules']} stub modules, "
            f"{sources['py4gw']['totals']['methods']} class methods"
        )
        print(
            "Py4GW Native: "
            f"{sources['py4gwNative']['totals']['bindingModules']} modules, "
            f"{sources['py4gwNative']['totals']['resolvers']} named resolvers"
        )
        print(
            "gwnative: "
            f"{len(sources['gwnative']['stateFields'])} state fields, "
            f"{len(sources['gwnative']['enhancementLayoutFields'])} "
            "certified layout words"
        )
        print(f"Cross-project domains: {len(report['domains'])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
