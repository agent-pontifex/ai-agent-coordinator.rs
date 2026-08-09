#!/usr/bin/env python3
"""Render a self-contained, read-only HypeSiege/StreemPilot publication plan."""

from __future__ import annotations

import argparse
import base64
import hashlib
import html
import importlib.util
import json
import os
import pathlib
import tempfile
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
PUBLISHER_PATH = ROOT / "scripts" / "publish_hypesiege_streempilot_fleet.py"


def load_publisher():
    spec = importlib.util.spec_from_file_location("fleet_publisher", PUBLISHER_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load fleet publisher")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PUBLISHER = load_publisher()
STYLE = """:root { color-scheme: light dark; font-family: ui-sans-serif, system-ui, sans-serif; }
* { box-sizing: border-box; }
body { margin: 0 auto; max-width: 120rem; padding: 2rem; line-height: 1.45; }
h1 { margin-bottom: .25rem; }
.notice { border: 2px solid currentColor; border-radius: .5rem; padding: 1rem; }
.summary { display: grid; grid-template-columns: repeat(auto-fit, minmax(12rem, 1fr));
  gap: 1rem; margin: 1.5rem 0; }
.summary section { border: 1px solid currentColor; border-radius: .5rem; padding: 1rem; }
.summary strong { display: block; font-size: 1.75rem; }
.table-scroll { max-width: 100%; overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font-size: .9rem; }
caption { font-weight: 700; padding: .75rem; text-align: left; }
th, td { border: 1px solid currentColor; padding: .5rem; text-align: left; vertical-align: top; }
thead th { position: sticky; top: 0; background: Canvas; }
code { overflow-wrap: anywhere; }
@media (max-width: 42rem) { body { padding: 1rem; } .summary { grid-template-columns: 1fr 1fr; } }
"""
STYLE_HASH = base64.b64encode(hashlib.sha256(STYLE.encode("utf-8")).digest()).decode(
    "ascii"
)
META_CSP = (
    "default-src 'none'; "
    f"style-src 'sha256-{STYLE_HASH}'; "
    "img-src 'none'; font-src 'none'; connect-src 'none'; media-src 'none'; "
    "object-src 'none'; frame-src 'none'; worker-src 'none'; "
    "manifest-src 'none'; base-uri 'none'; form-action 'none'"
)
HEADER_CSP = META_CSP + "; frame-ancestors 'none'"


def canonical_manifest_digest(manifest: dict[str, Any]) -> str:
    payload = json.dumps(
        manifest,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def render_plan(manifest: dict[str, Any]) -> str:
    repositories = manifest["repositories"]
    digest = canonical_manifest_digest(manifest)
    rows = []
    for index, record in enumerate(repositories, start=1):
        rows.append(
            '<tr data-repository="{full_name}" data-kind="{kind}">'
            '<td>{index}</td><th scope="row"><code>{full_name}</code></th>'
            '<td>{kind}</td><td>{visibility}</td><td>{files}</td>'
            '<td>{gitlinks}</td><td><code>{commit}</code></td></tr>'.format(
                index=index,
                full_name=html.escape(record["full_name"], quote=True),
                kind=html.escape(record["kind"], quote=True),
                visibility=html.escape(record["visibility"], quote=True),
                files=record["files"],
                gitlinks=record["gitlinks"],
                commit=html.escape(record["commit"], quote=True),
            )
        )

    return f"""<!doctype html>
<html lang="en" data-fleet-digest="{digest}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="{html.escape(META_CSP, quote=True)}">
<meta name="referrer" content="no-referrer">
<title>HypeSiege and StreemPilot publication plan</title>
<style>{STYLE}</style>
</head>
<body>
<header>
<h1>HypeSiege and StreemPilot publication plan</h1>
<p id="publication-status">{html.escape(manifest["publication_status"])}</p>
</header>
<section class="notice" aria-labelledby="safety-heading">
<h2 id="safety-heading">Read-only operator review</h2>
<p>This page does not create repositories, request credentials, execute JavaScript,
submit forms, or contact external services. Live publication remains one repository
at a time and requires exact confirmation.</p>
</section>
<div class="summary" aria-label="Fleet summary">
<section><span>Repositories</span>
<strong id="repository-count">{manifest["repository_count"]}</strong></section>
<section><span>Tracked files</span>
<strong id="tracked-file-count">{manifest["total_tracked_files"]}</strong></section>
<section><span>Gitlinks</span>
<strong id="gitlink-count">{manifest["total_gitlinks"]}</strong></section>
<section><span>HypeSiege</span>
<strong id="hypesiege-count">{manifest["organizations"]["hypesiege"]}</strong></section>
<section><span>StreemPilot</span>
<strong id="streempilot-count">{manifest["organizations"]["streempilot"]}</strong></section>
</div>
<p>Manifest SHA-256: <code id="manifest-digest">{digest}</code></p>
<div class="table-scroll" role="region" aria-label="Repository publication order" tabindex="0">
<table aria-describedby="publication-status">
<caption>Deterministic repository order</caption>
<thead><tr><th scope="col">#</th><th scope="col">Repository</th>
<th scope="col">Kind</th><th scope="col">Visibility</th>
<th scope="col">Files</th><th scope="col">Gitlinks</th>
<th scope="col">Commit</th></tr></thead>
<tbody>{''.join(rows)}</tbody>
</table>
</div>
</body>
</html>
"""


def atomic_write(output: pathlib.Path, content: str) -> None:
    requested = output.expanduser()
    if requested.is_symlink():
        raise RuntimeError("refusing to replace a symbolic-link output")
    requested.parent.mkdir(parents=True, exist_ok=True)
    parent = requested.parent.resolve(strict=True)
    output = parent / requested.name
    if output.exists() and not output.is_file():
        raise RuntimeError("refusing to replace a non-file output")

    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{output.name}.",
        suffix=".tmp",
        dir=parent,
        text=True,
    )
    temporary = pathlib.Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        temporary.chmod(0o644)
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--manifest",
        type=pathlib.Path,
        default=ROOT / "repository-fleets" / "hypesiege-streempilot.json",
    )
    parser.add_argument("--output", type=pathlib.Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    manifest = PUBLISHER.load_manifest(args.manifest)
    atomic_write(args.output, render_plan(manifest))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
