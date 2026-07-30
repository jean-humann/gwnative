from __future__ import annotations

import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
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

    def test_traffic_wait_distinguishes_send_from_receive(self) -> None:
        sent = {
            "sequence": 1,
            "kind": "client-traffic",
            "detail": {
                "actionSequence": 7,
                "direction": "send",
                "socketId": 4,
                "bytes": 12,
            },
        }
        received = {
            "sequence": 2,
            "kind": "client-traffic",
            "detail": {
                "actionSequence": 7,
                "direction": "receive",
                "socketId": 4,
                "bytes": 24,
            },
        }
        stream = StubStream([[received, sent]])
        self.assertEqual(
            stream.wait_for_action_traffic(7, 0.1, direction="send"),
            sent,
        )
        self.assertEqual(
            stream.wait_for_action_traffic(7, 0.1, direction="receive"),
            received,
        )


class GameStateTests(unittest.TestCase):
    def test_ready_state_requires_two_matching_revisions(self) -> None:
        states = iter(
            [
                {
                    "revision": 1,
                    "state": {"status": "waiting", "reason": "game"},
                },
                {
                    "revision": 2,
                    "state": {"status": "ready", "mapId": 55, "playerId": 4},
                },
                {
                    "revision": 3,
                    "state": {"status": "ready", "mapId": 55, "playerId": 4},
                },
            ]
        )
        original = RUNNER.game_state_after
        RUNNER.game_state_after = lambda *args, **kwargs: next(states)
        try:
            ready = RUNNER.wait_for_ready_state("", "", 0, 1)
        finally:
            RUNNER.game_state_after = original
        self.assertEqual(ready["revision"], 3)

    def test_gameplay_state_waits_for_two_complete_revisions(self) -> None:
        complete = {
            "status": "ready",
            "mapId": 55,
            "playerId": 4,
            **{name: {} for name in RUNNER.GAMEPLAY_DOMAINS},
            "party": {"players": [{"loginNumber": 1}]},
        }
        states = iter(
            [
                {
                    "revision": 2,
                    "state": {
                        "status": "ready",
                        "mapId": 55,
                        "playerId": 4,
                        "party": {"players": []},
                    },
                },
                {"revision": 3, "state": complete},
                {"revision": 4, "state": complete},
            ]
        )
        initial = next(states)
        original = RUNNER.game_state_after
        RUNNER.game_state_after = lambda *args, **kwargs: next(states)
        try:
            ready = RUNNER.wait_for_complete_gameplay_state(
                "",
                "",
                initial,
                1,
            )
        finally:
            RUNNER.game_state_after = original
        self.assertEqual(ready["revision"], 4)


class ProfileTests(unittest.TestCase):
    def test_default_profile_uses_the_legacy_web_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            self.assertEqual(
                RUNNER.profile_web_root(None, base),
                base / "web",
            )
            self.assertEqual(
                RUNNER.profile_web_root("default", base),
                base / "web",
            )

    def test_named_profile_must_be_safe_and_exist(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            profile = base / "profiles" / "codex-e2e"
            profile.mkdir(parents=True)
            (profile / "profile.json").touch()
            self.assertEqual(
                RUNNER.profile_web_root("codex-e2e", base),
                profile / "web",
            )
            with self.assertRaises(RUNNER.Failure):
                RUNNER.profile_web_root("../default", base)
            with self.assertRaises(RUNNER.Failure):
                RUNNER.profile_web_root("missing", base)


if __name__ == "__main__":
    unittest.main()
