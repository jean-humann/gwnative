from __future__ import annotations

import importlib.machinery
import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
LOADER = importlib.machinery.SourceFileLoader(
    "gwnative_e2e_runner",
    str(ROOT / "scripts/e2e"),
)
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None
RUNNER = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(RUNNER)


def event(sequence: int, kind: str) -> dict[str, object]:
    return {"sequence": sequence, "kind": kind, "detail": {}}


class StubStream(RUNNER.EventStream):
    def __init__(self, batches: list[list[dict[str, object]]]) -> None:
        super().__init__("", "")
        self.batches = list(batches)

    def next_batch(self, deadline: float) -> list[dict[str, object]]:
        del deadline
        if self.pending:
            result, self.pending = self.pending, []
            return result
        return self.batches.pop(0) if self.batches else []


class EventStreamTests(unittest.TestCase):
    def test_optional_wait_preserves_sibling_events(self) -> None:
        stream = StubStream(
            [[
                event(1, "client-traffic"),
                event(2, "login-response"),
                event(3, "socket-open"),
            ]]
        )
        self.assertEqual(
            stream.wait_optional("login-response", 0.1)["sequence"],
            2,
        )
        self.assertEqual(
            [item["kind"] for item in stream.next_batch(0)],
            ["client-traffic", "socket-open"],
        )

    def test_milestone_wait_preserves_unrelated_events(self) -> None:
        stream = StubStream(
            [[
                event(1, "socket-open"),
                event(2, "bridge-ready"),
                event(3, "first-frame"),
                event(4, "client-traffic"),
            ]]
        )
        found = stream.wait_for({"bridge-ready", "first-frame"}, 0.1)
        self.assertEqual(set(found), {"bridge-ready", "first-frame"})
        self.assertEqual(
            [item["kind"] for item in stream.next_batch(0)],
            ["socket-open", "client-traffic"],
        )


if __name__ == "__main__":
    unittest.main()
