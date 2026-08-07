#!/usr/bin/env python3
"""Render the ordered MemeBank PostgreSQL desired state deterministically."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

ORDER_NAME = re.compile(r"^[0-9]{3}_[a-z0-9_]+\.sql$")


class RenderError(ValueError):
    pass


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def inside(root: Path, candidate: Path) -> bool:
    try:
        candidate.relative_to(root)
        return True
    except ValueError:
        return False


def load_order(root: Path) -> list[Path]:
    schema_root = (root / "schema").resolve()
    order_path = schema_root / "order.txt"
    try:
        entries = [
            line.strip()
            for line in order_path.read_text(encoding="utf-8").splitlines()
            if line.strip() and not line.lstrip().startswith("#")
        ]
    except OSError as error:
        raise RenderError(f"cannot read {order_path}: {error}") from error
    if not entries:
        raise RenderError("schema/order.txt must not be empty")
    if len(entries) != len(set(entries)):
        raise RenderError("schema/order.txt contains duplicate entries")
    if entries != sorted(entries):
        raise RenderError("schema/order.txt must be lexicographically ordered")

    paths: list[Path] = []
    for entry in entries:
        if not ORDER_NAME.fullmatch(entry):
            raise RenderError(f"unsupported schema filename {entry!r}")
        path = (schema_root / entry).resolve()
        if not inside(schema_root, path) or not path.is_file():
            raise RenderError(f"ordered schema file is missing or escapes schema/: {entry}")
        paths.append(path)

    unordered = sorted(
        path.name
        for path in schema_root.glob("*.sql")
        if path.name not in set(entries)
    )
    if unordered:
        raise RenderError(f"schema files missing from order.txt: {unordered}")
    return paths


def normalize_sql(path: Path) -> str:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise RenderError(f"cannot read {path}: {error}") from error
    if "\r" in text:
        raise RenderError(f"CRLF is not permitted in {path.name}")
    if not text.endswith("\n"):
        raise RenderError(f"{path.name} must end with a newline")
    return text


def render(root: Path) -> tuple[str, dict[str, Any]]:
    root = root.resolve()
    paths = load_order(root)
    file_reports: list[dict[str, Any]] = []
    chunks = [
        "-- GENERATED FROM schema/order.txt; DO NOT EDIT THIS BUNDLE DIRECTLY.\n",
        "-- Source of truth: the ordered files under schema/.\n\n",
    ]
    all_sql = ""
    for path in paths:
        text = normalize_sql(path)
        encoded = text.encode("utf-8")
        relative = path.relative_to(root).as_posix()
        digest = sha256_bytes(encoded)
        file_reports.append(
            {
                "path": relative,
                "sha256": digest,
                "bytes": len(encoded),
            }
        )
        chunks.append(f"-- BEGIN {relative} sha256={digest}\n")
        chunks.append(text)
        chunks.append(f"-- END {relative}\n\n")
        all_sql += text

    bundle = "".join(chunks)
    table_names = sorted(
        set(
            re.findall(
                r"(?im)^CREATE\s+TABLE\s+((?:memebank|memebank_private)\.[a-z0-9_]+)\s*\(",
                all_sql,
            )
        )
    )
    index_names = sorted(
        set(
            re.findall(
                r"(?im)^CREATE\s+(?:UNIQUE\s+)?INDEX\s+([a-z0-9_]+)\s+",
                all_sql,
            )
        )
    )
    policy_names = sorted(
        set(
            re.findall(
                r"(?im)^CREATE\s+POLICY\s+([a-z0-9_]+)\s+",
                all_sql,
            )
        )
    )
    query_reports = []
    for path in sorted((root / "queries").glob("*.sql")):
        encoded = normalize_sql(path).encode("utf-8")
        query_reports.append(
            {
                "path": path.relative_to(root).as_posix(),
                "sha256": sha256_bytes(encoded),
                "bytes": len(encoded),
            }
        )
    report: dict[str, Any] = {
        "schema_version": 1,
        "contract": "memebank-postgresql-desired-state",
        "postgres_major": 16,
        "required_extensions": ["pgcrypto", "pg_trgm", "vector"],
        "schema_files": file_reports,
        "schema_bundle_sha256": sha256_bytes(bundle.encode("utf-8")),
        "schema_bundle_bytes": len(bundle.encode("utf-8")),
        "tables": table_names,
        "indexes": index_names,
        "policies": policy_names,
        "queries": query_reports,
        "vector_tables": {
            "384": "memebank.asset_embeddings_384",
            "768": "memebank.asset_embeddings_768",
            "1024": "memebank.asset_embeddings_1024",
        },
        "automatic_apply": False,
    }
    return bundle, report


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    bundle, report = render(args.root)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(bundle, encoding="utf-8")
    args.report.write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RenderError as error:
        print(f"schema render failed: {error}", file=sys.stderr)
        raise SystemExit(2) from error
