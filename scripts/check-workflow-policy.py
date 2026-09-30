#!/usr/bin/env python3
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github" / "workflows"
SHA40 = re.compile(r"^[0-9a-fA-F]{40}$")
DIGEST = re.compile(r"^docker://.+@sha256:[0-9a-fA-F]{64}$")
USES = re.compile(r"^(\s*)(?:-\s+)?uses:\s*([^\s#]+)")
JOB = re.compile(r"^  ([A-Za-z0-9_.-]+):\s*(?:#.*)?$")


def fail(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


errors: list[str] = []
paths = sorted([*WORKFLOWS.glob("*.yml"), *WORKFLOWS.glob("*.yaml")])
if not paths:
    fail("no GitHub Actions workflows found")

for path in paths:
    rel = path.relative_to(ROOT)
    lines = path.read_text(encoding="utf-8").splitlines()
    jobs_index = next((i for i, line in enumerate(lines) if line == "jobs:"), None)
    if jobs_index is None:
        errors.append(f"{rel}: workflow has no jobs block")
        continue

    for i, line in enumerate(lines[:jobs_index], 1):
        if re.match(r"^\s+[A-Za-z-]+:\s*write\s*(?:#.*)?$", line):
            errors.append(f"{rel}:{i}: top-level write permission is forbidden; scope it to one job")

    for i, line in enumerate(lines):
        match = USES.match(line)
        if not match:
            continue
        ref = match.group(2)
        if ref.startswith("./"):
            continue
        if ref.startswith("docker://"):
            if not DIGEST.match(ref):
                errors.append(f"{rel}:{i + 1}: Docker action is not digest-pinned: {ref}")
            continue
        if "@" not in ref:
            errors.append(f"{rel}:{i + 1}: external action has no immutable ref: {ref}")
            continue
        _, action_ref = ref.rsplit("@", 1)
        if not SHA40.match(action_ref):
            errors.append(f"{rel}:{i + 1}: external action is not pinned to a full SHA: {ref}")

        if ref.startswith("actions/checkout@"):
            base_indent = len(match.group(1))
            block: list[str] = []
            for following in lines[i + 1 :]:
                stripped = following.strip()
                if not stripped:
                    continue
                indent = len(following) - len(following.lstrip())
                if indent <= base_indent and (stripped.startswith("- ") or not following.startswith(" ")):
                    break
                block.append(following)
            if not any("persist-credentials: false" in item for item in block):
                errors.append(f"{rel}:{i + 1}: checkout must set persist-credentials: false")

    for i, line in enumerate(lines, 1):
        if re.search(r"\b(?:ubuntu|macos|windows)-latest\b", line):
            errors.append(f"{rel}:{i}: moving runner label is forbidden; pin an explicit runner image")

    job_lines = lines[jobs_index + 1 :]
    job_starts = [
        (jobs_index + 1 + i, JOB.match(line).group(1))
        for i, line in enumerate(job_lines)
        if JOB.match(line)
    ]
    for index, (start, name) in enumerate(job_starts):
        end = job_starts[index + 1][0] if index + 1 < len(job_starts) else len(lines)
        block = lines[start + 1 : end]
        reusable = any(re.match(r"^    uses:\s*", item) for item in block)
        if not reusable and not any(re.match(r"^    timeout-minutes:\s*\d+", item) for item in block):
            errors.append(f"{rel}:{start + 1}: job {name!r} has no timeout-minutes")

if errors:
    print("workflow policy violations:", file=sys.stderr)
    for error in errors:
        print(f"  - {error}", file=sys.stderr)
    raise SystemExit(1)

print(f"PASS: {len(paths)} workflows satisfy immutable/read-only/bounded CI policy")
