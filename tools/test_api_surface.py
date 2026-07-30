from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from tools.api_surface import (
    SurfaceError,
    inspect_gwca,
    inspect_gwnative,
    inspect_py4gw,
    inspect_py4gw_native,
    _domain,
)


class ApiSurfaceTests(unittest.TestCase):
    def test_gwnative_domains_follow_the_public_state_schema(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "src"
            wasm = source / "wasm"
            wasm.mkdir(parents=True)
            (source / "game_api.rs").write_text(
                "pub struct State {\n"
                "    pub map_id: Option<u32>,\n"
                "    pub player_id: Option<u32>,\n"
                "    pub target_valid: Option<bool>,\n"
                "    pub party: Option<Party>,\n"
                "    pub skillbar: Option<Skillbar>,\n"
                "    pub effects: Option<PlayerEffects>,\n"
                "    pub agents: Option<MapAgents>,\n"
                "    pub quests: Option<Quests>,\n"
                "}\n"
            )
            (wasm / "builds.rs").write_text(
                "pub(super) struct EnhancementLayout {\n"
                "    pub context_root: u32,\n"
                "}\n"
            )
            report = inspect_gwnative(root)

        self.assertEqual(
            report["certifiedDomains"],
            [
                "agent",
                "effects",
                "map",
                "party",
                "player",
                "quest",
                "skillbar",
                "target",
            ],
        )

    def test_gwca_inventory_keeps_names_but_not_declarations(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            include = root / "Dependencies/GWCA/include/GWCA"
            managers = include / "Managers"
            managers.mkdir(parents=True)
            (include / "GWCAVersion.h").write_text(
                '#define GWCA_VERSION "1.2.3.4"\n'
            )
            (managers / "AgentMgr.h").write_text(
                "GWCA_API const Agent* GetAgent(uint32_t id);\n"
                "GWCA_API bool ChangeTarget(uint32_t id);\n"
            )
            report = inspect_gwca(root)
        self.assertEqual(report["gwcaVersion"], "1.2.3.4")
        self.assertEqual(
            report["managerFunctions"]["AgentMgr"],
            ["ChangeTarget", "GetAgent"],
        )
        self.assertNotIn("uint32_t", json.dumps(report))

    def test_py4gw_inventory_reads_stub_contract(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stubs = root / "stubs"
            stubs.mkdir()
            (stubs / "PyAgent.pyi").write_text(
                "class Agent:\n"
                "    agent_id: int\n"
                "    def position(self) -> tuple[float, float]: ...\n"
                "def target() -> int: ...\n"
            )
            report = inspect_py4gw(root)
        self.assertEqual(report["stubs"]["PyAgent"]["functions"], ["target"])
        self.assertEqual(
            report["stubs"]["PyAgent"]["classes"]["Agent"],
            {"methods": ["position"], "attributes": ["agent_id"]},
        )

    def test_native_inventory_excludes_signature_material(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            offsets = root / "offsets"
            source = root / "src"
            offsets.mkdir()
            source.mkdir()
            (offsets / "agent.json").write_text(
                json.dumps(
                    {
                        "namespace": "agent",
                        "patterns": {
                            "agent_array": {
                                "pattern": "secret bytes",
                                "assertion_message": "secret assertion",
                            }
                        },
                        "resolvers": {
                            "agent_array_addr": {
                                "steps": [{"value": "0x1234"}]
                            }
                        },
                    }
                )
            )
            (source / "agent_bindings.cpp").write_text(
                'PYBIND11_EMBEDDED_MODULE(PyAgent, m) {\n'
                'm.def("get_agent", &get_agent);\n'
                'binding.def_property_readonly("position", &Agent::position);\n'
                "}\n"
                'PYBIND11_EMBEDDED_MODULE(PyMouse, m) {\n'
                'm.def("click", &click);\n'
                "}\n"
            )
            report = inspect_py4gw_native(root)
        self.assertEqual(
            report["offsets"]["agent"],
            {
                "patterns": ["agent_array"],
                "resolvers": ["agent_array_addr"],
            },
        )
        self.assertEqual(
            report["bindings"]["PyAgent"], ["get_agent", "position"]
        )
        self.assertEqual(report["bindings"]["PyMouse"], ["click"])
        rendered = json.dumps(report)
        self.assertNotIn("secret", rendered)
        self.assertNotIn("0x1234", rendered)

    def test_rejects_unexpected_offset_schema(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            offsets = root / "offsets"
            offsets.mkdir()
            (offsets / "bad.json").write_text('{"namespace": 7}')
            with self.assertRaises(SurfaceError):
                inspect_py4gw_native(root)

    def test_normalises_acronym_domains(self) -> None:
        self.assertEqual(_domain("Py4GW"), "runtime")
        self.assertEqual(_domain("PyDXOverlay"), "dx_overlay")
        self.assertEqual(_domain("PyImGui"), "imgui")
        self.assertEqual(_domain("UIMgr"), "ui")
        self.assertEqual(_domain("StoCMgr"), "sto_c")


if __name__ == "__main__":
    unittest.main()
