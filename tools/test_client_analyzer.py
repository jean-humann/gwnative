from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from tools.client_analyzer import (
    certify,
    diff_reports,
    inspect_jspi,
    inspect_wasm,
    locate_wasm_string,
    sha256_file,
)


def uleb(value: int) -> bytes:
    result = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        result.append(byte | (0x80 if value else 0))
        if not value:
            return bytes(result)


def sleb(value: int) -> bytes:
    result = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        done = (value == 0 and byte & 0x40 == 0) or (
            value == -1 and byte & 0x40 != 0
        )
        result.append(byte if done else byte | 0x80)
        if done:
            return bytes(result)


def sleb5(value: int) -> bytes:
    result = bytearray()
    for _ in range(4):
        result.append((value & 0x7F) | 0x80)
        value >>= 7
    result.append(value & 0x7F)
    return bytes(result)


def vector(values: list[bytes]) -> bytes:
    return uleb(len(values)) + b"".join(values)


def name(value: str) -> bytes:
    raw = value.encode()
    return uleb(len(raw)) + raw


def section(section_id: int, payload: bytes) -> bytes:
    return bytes([section_id]) + uleb(len(payload)) + payload


def module(bodies: list[bytes], export_index: int = 1) -> bytes:
    type_section = section(1, vector([b"\x60\x00\x00"]))
    import_entry = name("env") + name("clock") + b"\x00\x00"
    import_section = section(2, vector([import_entry]))
    function_section = section(3, vector([b"\x00" for _ in bodies]))
    export_section = section(
        7, vector([name("main") + b"\x00" + uleb(export_index)])
    )
    code_section = section(10, vector([uleb(len(body)) + body for body in bodies]))
    return (
        b"\0asm\x01\0\0\0"
        + type_section
        + import_section
        + function_section
        + export_section
        + code_section
    )


class ClientAnalyzerTests(unittest.TestCase):
    def test_inspects_wasm_without_exposing_bodies(self) -> None:
        body = b"\x00\x41\x01\x0b"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "client.wasm"
            path.write_bytes(module([b"\x00\x0b", body]))
            report = inspect_wasm(path)
        self.assertEqual(report["importCounts"], {"function": 1})
        self.assertEqual(report["exports"][0]["name"], "main")
        self.assertEqual(len(report["functionBodyHashes"]), 2)
        self.assertNotIn(body.hex(), json.dumps(report))

    def test_reports_exact_body_reuse_and_index_shift(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            before = root / "before.wasm"
            after = root / "after.wasm"
            one = b"\x00\x41\x01\x0b"
            two = b"\x00\x41\x02\x0b"
            before.write_bytes(module([one, two]))
            after.write_bytes(module([b"\x00\x0b", one, two]))
            before_report = inspect_wasm(before)
            after_report = inspect_wasm(after)
            report = diff_reports(
                {"wasm": before_report},
                {"wasm": after_report},
            )
        bodies = report["wasm"]["functionBodies"]
        self.assertEqual(bodies["sharedExact"], 2)
        self.assertEqual(
            bodies["dominantUniqueBodyShift"],
            {"delta": 1, "matchingFunctions": 2},
        )
        self.assertEqual(
            bodies["changedSameIndex"],
            [
                {
                    "definedIndex": 0,
                    "beforeFunctionIndex": 1,
                    "afterFunctionIndex": 1,
                    "beforeSha256": before_report["functionBodyHashes"][0],
                    "afterSha256": after_report["functionBodyHashes"][0],
                },
                {
                    "definedIndex": 1,
                    "beforeFunctionIndex": 2,
                    "afterFunctionIndex": 2,
                    "beforeSha256": before_report["functionBodyHashes"][1],
                    "afterSha256": after_report["functionBodyHashes"][1],
                },
            ],
        )

    def test_reports_absolute_indices_for_same_index_body_changes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            before = root / "before.wasm"
            after = root / "after.wasm"
            before.write_bytes(module([b"\x00\x41\x01\x0b"]))
            after.write_bytes(module([b"\x00\x41\x02\x0b"]))
            report = diff_reports(
                {"wasm": inspect_wasm(before)},
                {"wasm": inspect_wasm(after)},
            )
        [changed] = report["wasm"]["functionBodies"]["changedSameIndex"]
        self.assertEqual(changed["definedIndex"], 0)
        self.assertEqual(changed["beforeFunctionIndex"], 1)
        self.assertEqual(changed["afterFunctionIndex"], 1)
        self.assertNotEqual(changed["beforeSha256"], changed["afterSha256"])

    def test_extracts_only_module_contract_names_from_jspi(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "client.js"
            path.write_text(
                'Module.canvas; Module["socket"]["connect"]; '
                "Module.image.readAsync; WebAssembly.Suspending; "
                "const privateClientCode = 1;"
            )
            report = inspect_jspi(path)
        self.assertEqual(
            report["moduleProperties"], ["canvas", "image", "socket"]
        )
        self.assertEqual(
            report["modulePaths"],
            ["canvas", "image.readAsync", "socket.connect"],
        )
        self.assertTrue(report["jspi"]["suspending"])
        self.assertFalse(report["jspi"]["promising"])

    def test_export_contract_is_separate_from_function_index(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            before = root / "before.wasm"
            after = root / "after.wasm"
            before.write_bytes(module([b"\x00\x0b"], export_index=1))
            after.write_bytes(
                module([b"\x00\x41\x00\x0b", b"\x00\x0b"], export_index=2)
            )
            report = diff_reports(
                {"wasm": inspect_wasm(before)},
                {"wasm": inspect_wasm(after)},
            )
        self.assertEqual(report["wasm"]["exportsAdded"], [])
        self.assertEqual(report["wasm"]["exportsRemoved"], [])
        self.assertEqual(
            report["wasm"]["exportsRetargeted"],
            [{"export": "function:main", "beforeIndex": 1, "afterIndex": 2}],
        )

    def test_certifies_registry_chain_and_checks_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.wasm"
            template = root / "template.wasm"
            enhanced = root / "enhanced.wasm"
            source.write_bytes(b"source")
            template.write_bytes(b"template")
            enhanced.write_bytes(b"enhanced")
            registry = root / "builds.rs"
            registry.write_text(
                f'''
pub(super) const BUILDS: &[KnownBuild] = &[KnownBuild {{
 sha256: "{sha256_file(source)}",
 output_sha256: "{sha256_file(template)}",
}}];
pub(super) fn find_build() {{}}
pub(super) const ENHANCEMENT_BUILDS: &[EnhancementBuild] = &[EnhancementBuild {{
 sha256: "{sha256_file(template)}",
 output_sha256: "{sha256_file(enhanced)}",
 program_id: 1,
 build_id: 9,
 hook_function: 7,
}}];
pub(super) fn find_enhancement_build() {{}}
'''
            )
            report, passed = certify(source, registry, template, enhanced)
        self.assertTrue(passed)
        self.assertTrue(report["certified"])
        self.assertEqual(report["enhancement"]["buildId"], 9)
        self.assertTrue(report["checks"]["enhancedOutput"]["matches"])

    def test_locates_data_strings_without_returning_client_contents(self) -> None:
        address = 0x1002
        candidate = 0x5000
        body = (
            b"\x00"
            + b"\x41"
            + sleb5(address)
            + b"\x1a\x41"
            + sleb(candidate)
            + b"\x1a\x0b"
        )
        data = b"xxknown assertionyy"
        data_segment = (
            uleb(1)
            + uleb(0)
            + b"\x41"
            + sleb(0x1000)
            + b"\x0b"
            + uleb(len(data))
            + data
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "client.wasm"
            path.write_bytes(module([body]) + section(11, data_segment))
            report = locate_wasm_string(path, b"known assertion")
        self.assertEqual(report["memoryAddresses"], [address])
        self.assertEqual(
            report["references"],
            [{
                "address": address,
                "referencedAddress": address,
                "addressDelta": 0,
                "functionIndex": 1,
                "rawReferenceCount": 1,
                "nearbyI32Constants": [address, candidate],
            }],
        )
        self.assertNotIn("known assertion", json.dumps(report))


if __name__ == "__main__":
    unittest.main()
