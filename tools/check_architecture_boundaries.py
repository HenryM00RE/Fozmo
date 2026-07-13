#!/usr/bin/env python3
"""Lightweight architecture boundary checks.

The intent is to catch obvious drift without turning architecture into a
second type system. Keep rules simple, named, and easy to remove or tighten as
the refactor phases complete.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


ROOT = Path(__file__).resolve().parents[1]
SOURCE_SUFFIXES = {".rs", ".ts", ".tsx"}
APP_STATE_FIELDS = (
    "settings",
    "library",
    "listening",
    "qobuz",
    "airplay",
    "sonos",
    "playback_sequencer",
    "playback_config_applicator",
    "zones",
    "pairing",
    "diagnostics",
    "hegel_status",
    "remote_access",
    "remote_auth_limiter",
    "public_base_url",
    "music_dir",
    "presets_dir",
)


@dataclass(frozen=True)
class Rule:
    name: str
    description: str
    includes: tuple[str, ...]
    pattern: re.Pattern[str]
    excludes: tuple[str, ...] = ()


def iter_source_files() -> Iterable[Path]:
    for base in (ROOT / "src", ROOT / "ui" / "src"):
        if not base.exists():
            continue
        for path in base.rglob("*"):
            if path.is_file() and path.suffix in SOURCE_SUFFIXES:
                yield path


def rel(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def path_matches(path: str, prefixes: tuple[str, ...]) -> bool:
    return any(path == prefix or path.startswith(prefix) for prefix in prefixes)


def matching_lines(path: Path, pattern: re.Pattern[str]) -> list[tuple[int, str]]:
    matches: list[tuple[int, str]] = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if pattern.search(line):
            matches.append((line_number, line.strip()))
    return matches


def check_rule(rule: Rule, files: Iterable[Path]) -> list[str]:
    failures: list[str] = []
    for path in files:
        relative = rel(path)
        if not path_matches(relative, rule.includes):
            continue
        if path_matches(relative, rule.excludes):
            continue
        for line_number, line in matching_lines(path, rule.pattern):
            failures.append(f"{relative}:{line_number}: {line}")
    return failures


RULES = (
    Rule(
        name="backend-framework-imports",
        description=(
            "Axum and Tower HTTP imports belong in HTTP/app adapters, not in "
            "low-level domains."
        ),
        includes=(
            "src/audio/",
            "src/services/",
            "src/library/",
            "src/zones/",
            "src/settings/",
            "src/protocol/",
            "src/diagnostics/",
            "src/agent.rs",
            "src/listening.rs",
        ),
        pattern=re.compile(r"\b(?:axum|tower_http)::"),
    ),
    Rule(
        name="app-state-domain-leakage",
        description=(
            "AppState should not leak into audio, service, persistence, "
            "settings, protocol, or diagnostics modules."
        ),
        includes=(
            "src/audio/",
            "src/services/",
            "src/library/",
            "src/settings/",
            "src/protocol/",
            "src/diagnostics/",
        ),
        pattern=re.compile(r"\bAppState\b|crate::app::state"),
    ),
    Rule(
        name="audio-engine-internals",
        description="Audio engine internals should stay inside src/audio/.",
        includes=("src/",),
        excludes=("src/audio/",),
        pattern=re.compile(r"crate::audio::engine::"),
    ),
    Rule(
        name="integration-neutral-playback-config",
        description=(
            "Generic playback config must stay free of sink- or "
            "amplifier-specific behavior."
        ),
        includes=("src/playback/config.rs",),
        pattern=re.compile(
            r"\b(?:Hegel|hegel|AirPlay|airplay|Sonos|sonos)\b|"
            r"SinkProtocol::(?:AirPlay|AirPlay2|SonosUpnp)"
        ),
    ),
    Rule(
        name="app-shell-no-direct-fetching",
        description=(
            "App.tsx should compose feature hooks and routes, not perform "
            "direct API fetching."
        ),
        includes=("ui/src/app/App.tsx",),
        pattern=re.compile(r"\b(?:fetch|apiRequest|apiJson)\s*\("),
    ),
    Rule(
        name="app-shell-no-direct-feature-model-imports",
        description=(
            "App.tsx should compose app hooks and route state, not import "
            "feature model internals directly."
        ),
        includes=("ui/src/app/App.tsx",),
        pattern=re.compile(r"\bfrom ['\"]\.\./features/[^'\"]*(?:/model/|Model)"),
    ),
    Rule(
        name="migrated-app-state-callers-use-accessors",
        description=(
            "Routes and app adapters migrated in architecture cleanup Phase 1 "
            "should use AppState accessors instead of direct field access."
        ),
        includes=(
            "src/api/routes/",
            "src/app/auth.rs",
            "src/app/runtime.rs",
            "src/playback/",
        ),
        pattern=re.compile(rf"\b(?:state|self\.state)\.({'|'.join(APP_STATE_FIELDS)})\b(?!\s*\()"),
    ),
    Rule(
        name="playback-no-http-framework-imports",
        description="Playback modules should accept domain data, not Axum HTTP types.",
        includes=("src/playback/",),
        pattern=re.compile(r"\baxum::"),
    ),
)


def main() -> int:
    files = tuple(iter_source_files())
    all_failures: list[tuple[Rule, list[str]]] = []
    for rule in RULES:
        failures = check_rule(rule, files)
        if failures:
            all_failures.append((rule, failures))

    if not all_failures:
        print("Architecture boundary checks passed.")
        return 0

    print("Architecture boundary checks failed:\n")
    for rule, failures in all_failures:
        print(f"==> {rule.name}")
        print(rule.description)
        for failure in failures:
            print(f"  {failure}")
        print()
    return 1


if __name__ == "__main__":
    sys.exit(main())
