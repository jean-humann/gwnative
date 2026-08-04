import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location(
    "benchmarklib", ROOT / "scripts/benchmarklib.py"
)
assert SPEC and SPEC.loader
benchmarklib = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = benchmarklib
SPEC.loader.exec_module(benchmarklib)


class HermeticBenchmarkTests(unittest.TestCase):
    def test_blank_profile_cannot_name_the_default_keychain_item(self):
        # The fixture represents an existing real default-profile credential.
        fixture = {"service": "gwnative", "account": "login", "password": "fixture-secret"}
        name = benchmarklib.profile_id("a" * 32)
        requested = benchmarklib.keychain_account(name)

        self.assertNotEqual(requested, fixture["account"])
        output = {"profile": name, "keychainAccount": requested}
        benchmarklib.assert_no_forbidden(output, [fixture["password"]])

    def test_profile_names_are_unique_safe_and_bounded(self):
        first = benchmarklib.profile_id("1" * 32)
        second = benchmarklib.profile_id("2" * 32)
        self.assertNotEqual(first, second)
        self.assertRegex(first, r"^benchmark-[0-9a-f]{24}$")
        with self.assertRaises(benchmarklib.Refusal):
            benchmarklib.profile_id("not-safe")

    def test_rounds_are_alternated_and_never_fewer_than_five(self):
        apps = ["gwnative", "reference"]
        self.assertEqual(benchmarklib.alternating_order(0, apps), apps)
        self.assertEqual(benchmarklib.alternating_order(1, apps), list(reversed(apps)))
        self.assertEqual(benchmarklib.require_rounds(5), 5)
        with self.assertRaises(benchmarklib.Refusal):
            benchmarklib.require_rounds(4)

    def test_tree_hash_covers_bytes_and_mtime(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "state.json"
            path.write_text("one")
            first = benchmarklib.tree_state(root)
            path.write_text("two")
            second = benchmarklib.tree_state(root)
            self.assertNotEqual(first.digest, second.digest)

            stat = path.stat()
            os.utime(path, ns=(stat.st_atime_ns, stat.st_mtime_ns + 1_000_000))
            third = benchmarklib.tree_state(root)
            self.assertNotEqual(second.digest, third.digest)

    def test_changed_warm_source_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "state"
            path.write_text("before")
            before = benchmarklib.tree_state(root)
            path.write_text("after")
            with self.assertRaisesRegex(benchmarklib.Refusal, "changed during"):
                benchmarklib.assert_unchanged(before, root)

    def test_warm_fixture_requires_an_explicit_matching_marker(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source"
            destination = Path(directory) / "clone"
            source.mkdir()
            with self.assertRaisesRegex(benchmarklib.Refusal, "benchmark-source.json"):
                benchmarklib.clone_declared_fixture(source, destination, "gwnative")

            (source / benchmarklib.SOURCE_MARKER).write_text(
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "application": "reference",
                        "sceneReadiness": "login screen only",
                    }
                )
            )
            with self.assertRaisesRegex(benchmarklib.Refusal, "does not match"):
                benchmarklib.clone_declared_fixture(source, destination, "gwnative")

    def test_warm_fixture_clone_is_private_and_source_stays_identical(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            destination = root / "clone"
            source.mkdir()
            (source / benchmarklib.SOURCE_MARKER).write_text(
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "application": "gwnative",
                        "sceneReadiness": "launcher ready; no login automation",
                    }
                )
            )
            (source / "settings.json").write_text("settings")
            clone = benchmarklib.clone_declared_fixture(source, destination, "gwnative")
            self.assertEqual((destination / "settings.json").read_text(), "settings")
            (destination / "settings.json").write_text("mutated clone")
            after = benchmarklib.assert_unchanged(clone.source_start, source)
            self.assertEqual(after.digest, clone.source_start.digest)
            self.assertEqual((source / "settings.json").read_text(), "settings")

    def test_symlinked_or_special_warm_input_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            outside = root / "outside"
            outside.write_text("outside")
            source = root / "source"
            source.mkdir()
            (source / "link").symlink_to(outside)
            with self.assertRaisesRegex(benchmarklib.Refusal, "symlink"):
                benchmarklib.tree_state(source)

    def test_mismatched_manifests_are_named_not_compared(self):
        base = {
            "artifactHashes": {"app": "a"},
            "runtime": "jspi",
            "rendering": "isolated",
            "renderScale": 2,
            "display": {"refreshHz": 60},
            "window": {"mode": "windowed"},
            "cacheState": "warm",
            "imageState": "partial",
            "machine": {"macOSBuild": "A"},
            "sceneReadiness": "login",
        }
        changed = dict(base, sceneReadiness="character select")
        self.assertEqual(benchmarklib.compatibility_reasons([base, base]), [])
        self.assertEqual(
            benchmarklib.compatibility_reasons([base, changed]), ["sceneReadiness"]
        )

    def test_ambiguous_xpc_ownership_invalidates_a_sample(self):
        exact = {10: "WebContent", 11: "GPU", 12: "Networking"}
        self.assertEqual(benchmarklib.validate_webkit_attribution(exact, []), [])
        reasons = benchmarklib.validate_webkit_attribution(
            {**exact, 13: "WebContent"}, [13]
        )
        self.assertTrue(any("outlived" in reason for reason in reasons))
        self.assertTrue(any("observed 2" in reason for reason in reasons))

    def test_reversed_order_fixture_detects_bias(self):
        biased = [
            {"orderPosition": 0, "firstFrameMs": value}
            for value in [100, 101, 99, 102, 98]
        ] + [
            {"orderPosition": 1, "firstFrameMs": value}
            for value in [140, 141, 139, 142, 138]
        ]
        self.assertTrue(benchmarklib.order_bias(biased, "firstFrameMs"))

    def test_statistics_are_derived_from_and_retain_raw_values(self):
        samples = [{"firstFrameMs": value} for value in [5, 1, 4, 2, 3]]
        summary = benchmarklib.summarize(samples, "firstFrameMs")
        self.assertEqual(summary["median"], 3)
        self.assertEqual(summary["values"], [5.0, 1.0, 4.0, 2.0, 3.0])
        with self.assertRaises(benchmarklib.Refusal):
            benchmarklib.summarize(samples[:4], "firstFrameMs")

    def test_secret_canary_cannot_enter_capture_output(self):
        canary = "default-keychain-password-canary"
        with self.assertRaisesRegex(benchmarklib.Refusal, "protected fixture"):
            benchmarklib.assert_no_forbidden({"output": [[0, canary]]}, [canary])


if __name__ == "__main__":
    unittest.main()
