"""Hermetic planning, provenance, and analysis for ``scripts/benchmark``.

This module has no launch side effects. Warm inputs must be explicitly prepared
fixtures; no function here discovers a user's installed profile.
"""

from __future__ import annotations

import hashlib
import json
import shutil
import statistics
import subprocess
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Mapping, Sequence

MIN_ROUNDS = 5
PROFILE_PREFIX = "benchmark-"
SOURCE_MARKER = "benchmark-source.json"
WEBKIT_KINDS = frozenset({"WebContent", "GPU", "Networking"})


class Refusal(RuntimeError):
    """A condition under which publishing a benchmark would be misleading."""


def profile_id(seed: str | None = None) -> str:
    """A safe, unique profile name and therefore a unique Keychain account."""
    suffix = (seed or uuid.uuid4().hex).lower()
    if not suffix or any(character not in "0123456789abcdef" for character in suffix):
        raise Refusal("benchmark profile seed must be non-empty lowercase hex")
    return f"{PROFILE_PREFIX}{suffix[:24]}"


def keychain_account(name: str) -> str:
    """Mirror the native named-profile account mapping for safety assertions."""
    if not name.startswith(PROFILE_PREFIX):
        raise Refusal("benchmark launches require a fresh named profile")
    return f"login:{name}"


def alternating_order(round_index: int, applications: Sequence[str]) -> list[str]:
    """Reverse every other round so one application is not always warmer."""
    order = list(applications)
    if round_index % 2:
        order.reverse()
    return order


def require_rounds(rounds: int) -> int:
    if rounds < MIN_ROUNDS:
        raise Refusal(f"every matrix cell requires at least {MIN_ROUNDS} clean rounds")
    return rounds


def _file_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


@dataclass(frozen=True)
class TreeState:
    root: str
    digest: str
    entries: int
    bytes: int

    def json(self) -> dict[str, object]:
        return {
            "root": self.root,
            "sha256": self.digest,
            "entries": self.entries,
            "bytes": self.bytes,
        }


def tree_state(root: Path) -> TreeState:
    """Hash bytes and mutation-relevant metadata without following symlinks."""
    root = root.resolve()
    if not root.is_dir():
        raise Refusal(f"warm source is not a directory: {root}")
    digest = hashlib.sha256()
    count = 0
    size = 0
    for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
        relative = path.relative_to(root).as_posix()
        stat = path.lstat()
        if path.is_symlink():
            raise Refusal(f"warm source contains a symlink: {relative}")
        kind = "d" if path.is_dir() else "f" if path.is_file() else "?"
        if kind == "?":
            raise Refusal(f"warm source contains a special file: {relative}")
        record = [kind, relative, stat.st_mode & 0o7777, stat.st_size, stat.st_mtime_ns]
        if kind == "f":
            record.append(_file_digest(path))
            size += stat.st_size
        digest.update(json.dumps(record, separators=(",", ":")).encode())
        digest.update(b"\n")
        count += 1
    return TreeState(str(root), digest.hexdigest(), count, size)


def assert_unchanged(before: TreeState, source: Path) -> TreeState:
    after = tree_state(source)
    if before != after:
        raise Refusal(
            "warm fixture changed during the run "
            f"({before.digest[:16]}… -> {after.digest[:16]}…)"
        )
    return after


def declared_source(root: Path, application: str) -> dict[str, object]:
    """Accept only a fixture whose marker explicitly names its intended use."""
    marker = root / SOURCE_MARKER
    try:
        value = json.loads(marker.read_bytes())
    except (OSError, ValueError) as error:
        raise Refusal(f"warm fixture needs a readable {SOURCE_MARKER}: {error}") from error
    if value.get("schemaVersion") != 1 or value.get("application") != application:
        raise Refusal("warm fixture marker does not match this application")
    if not isinstance(value.get("sceneReadiness"), str) or not value["sceneReadiness"]:
        raise Refusal("warm fixture marker needs an exact scene/readiness limitation")
    return value


def _copy_on_write(source: Path, destination: Path) -> str:
    """Clone an approved fixture on APFS; preserve metadata with a fallback."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    cloned = subprocess.run(
        ["/bin/cp", "-c", "-p", "-R", str(source), str(destination)],
        capture_output=True,
        text=True,
    )
    if cloned.returncode == 0:
        return "clonefile"
    if source.is_dir():
        shutil.copytree(source, destination, copy_function=shutil.copy2)
    else:
        shutil.copy2(source, destination)
    return "copy"


@dataclass(frozen=True)
class WarmClone:
    source_start: TreeState
    destination: Path
    method: str
    marker: Mapping[str, object]


def clone_declared_fixture(source: Path, destination: Path, application: str) -> WarmClone:
    """Clone an operator-supplied fixture, never an inferred installed path."""
    source = source.resolve()
    marker = declared_source(source, application)
    before = tree_state(source)
    if destination.exists():
        raise Refusal(f"warm destination already exists: {destination}")
    method = _copy_on_write(source, destination)
    return WarmClone(before, destination, method, marker)


def file_sha256(path: Path) -> str:
    return _file_digest(path) if path.is_file() else "unavailable"


def certificate_identity(feed: Path) -> dict[str, object]:
    try:
        parsed = json.loads(feed.read_bytes())
        families = parsed.get("families") or []
        return {
            "sequence": parsed.get("sequence"),
            "families": [family.get("familyId") for family in families],
            "feedSha256": file_sha256(feed),
        }
    except (OSError, ValueError, AttributeError):
        return {"sequence": None, "families": [], "feedSha256": "unavailable"}


def command_text(argv: Sequence[str]) -> str:
    try:
        return subprocess.run(
            list(argv), capture_output=True, text=True, timeout=30, check=False
        ).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return "unavailable"


def machine_identity() -> dict[str, str]:
    return {
        "macOS": command_text(["/usr/bin/sw_vers", "-productVersion"]),
        "macOSBuild": command_text(["/usr/bin/sw_vers", "-buildVersion"]),
        "webkitBuild": command_text(
            [
                "/usr/bin/defaults",
                "read",
                "/System/Library/Frameworks/WebKit.framework/Resources/Info",
                "CFBundleVersion",
            ]
        ),
        "powerSource": command_text(["/usr/bin/pmset", "-g", "batt"]),
        "thermalState": command_text(["/usr/bin/pmset", "-g", "therm"]),
    }


def compatibility_reasons(manifests: Sequence[Mapping[str, object]]) -> list[str]:
    """Name every controlled condition that differs across samples."""
    if not manifests:
        return ["no manifests"]
    keys = (
        "artifactHashes",
        "runtime",
        "rendering",
        "renderScale",
        "display",
        "window",
        "cacheState",
        "imageState",
        "machine",
        "sceneReadiness",
    )
    first = manifests[0]
    return [key for key in keys if any(item.get(key) != first.get(key) for item in manifests[1:])]


def validate_webkit_attribution(
    candidates: Mapping[int, str], survivors: Iterable[int]
) -> list[str]:
    """Accept one fresh service of each kind; reject every ambiguous shape."""
    survivors = set(survivors)
    reasons = []
    if survivors:
        reasons.append("candidate WebKit services outlived the host")
    counts = {kind: 0 for kind in WEBKIT_KINDS}
    for kind in candidates.values():
        if kind not in WEBKIT_KINDS:
            reasons.append(f"unknown WebKit service kind {kind}")
        else:
            counts[kind] += 1
    for kind, count in sorted(counts.items()):
        if count != 1:
            reasons.append(f"expected one {kind} service, observed {count}")
    return reasons


def summarize(samples: Sequence[Mapping[str, object]], metric: str) -> dict[str, object]:
    values = [
        float(sample[metric])
        for sample in samples
        if isinstance(sample.get(metric), (int, float))
    ]
    if len(values) < MIN_ROUNDS:
        raise Refusal(f"{metric} has only {len(values)} clean rounds")
    return {
        "count": len(values),
        "min": min(values),
        "median": statistics.median(values),
        "max": max(values),
        "values": values,
    }


def order_bias(
    samples: Sequence[Mapping[str, object]], metric: str, threshold: float = 0.10
) -> bool:
    by_position: dict[int, list[float]] = {}
    for sample in samples:
        value, position = sample.get(metric), sample.get("orderPosition")
        if isinstance(value, (int, float)) and isinstance(position, int):
            by_position.setdefault(position, []).append(float(value))
    if len(by_position) < 2 or any(not values for values in by_position.values()):
        return False
    medians = [statistics.median(values) for values in by_position.values()]
    centre = statistics.median(medians)
    return (centre == 0 and max(medians) != 0) or (
        centre != 0 and (max(medians) - min(medians)) / abs(centre) > threshold
    )


def assert_no_forbidden(document: object, forbidden: Iterable[str]) -> None:
    encoded = json.dumps(document, ensure_ascii=False, sort_keys=True)
    for value in forbidden:
        if value and value in encoded:
            raise Refusal("protected fixture value reached benchmark output")
