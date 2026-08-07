#!/usr/bin/env python3
"""Render a deterministic, credential-free HTML audit report for DEN-877."""

from __future__ import annotations

import argparse
import hashlib
import html
import json
import os
import pathlib
import sys
import tempfile

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from publish_hypesiege_streempilot_fleet import (  # noqa: E402
    MAX_REPORT_BYTES,
    PublicationError,
    load_manifest,
    manifest_digest,
)

CSP = "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'"
SECRET_MARKERS = (
    "GITHUB_REPOSITORY_ADMIN_TOKEN",
    "Authorization:",
    "Bearer ",
    "github_pat_",
    "ghp_",
    "ghs_",
)


def render_report(manifest: dict[str, object], *, digest: str) -> str:
    repositories = manifest["repositories"]
    if not isinstance(repositories, list):
        raise PublicationError("audit report repositories must be a list")
    rows: list[str] = []
    for index, record in enumerate(repositories, start=1):
        if not isinstance(record, dict):
            raise PublicationError("audit report repository row must be an object")
        kind = html.escape(str(record["kind"]), quote=True)
        full_name = html.escape(str(record["full_name"]), quote=True)
        description = html.escape(str(record["description"]), quote=True)
        commit = html.escape(str(record["commit"]), quote=True)
        remote = html.escape(str(record["remote"]), quote=True)
        org = html.escape(str(record["org"]), quote=True)
        rows.append(
            "<tr "
            f'data-repository="{full_name}" data-kind="{kind}" data-org="{org}">'
            f'<td class="number">{index}</td>'
            f'<th scope="row"><a href="{remote}" rel="noopener noreferrer">{full_name}</a></th>'
            f"<td><code>{kind}</code></td>"
            f'<td class="number">{record["files"]}</td>'
            f'<td class="number">{record["gitlinks"]}</td>'
            f'<td><code class="commit">{commit}</code></td>'
            f"<td>{description}</td>"
            "</tr>"
        )

    organizations = manifest["organizations"]
    if not isinstance(organizations, dict):
        raise PublicationError("audit report organizations must be an object")
    report = f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="robots" content="noindex,nofollow">
  <meta http-equiv="Content-Security-Policy" content="{html.escape(CSP, quote=True)}">
  <title>DEN-877 repository fleet audit</title>
  <style>
    :root {{ color-scheme: light dark; font-family: system-ui, sans-serif; }}
    body {{ margin: 0; padding: 1rem; line-height: 1.45; }}
    main {{ max-width: 96rem; margin: 0 auto; }}
    h1 {{ margin-bottom: .25rem; }}
    .boundary {{ border: 2px solid currentColor; border-radius: .5rem; padding: .75rem; }}
    .summary {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr)); gap: .75rem; margin: 1rem 0; }}
    .summary div {{ border: 1px solid currentColor; border-radius: .5rem; padding: .75rem; }}
    .summary strong {{ display: block; font-size: 1.5rem; }}
    .table-wrap {{ overflow-x: auto; }}
    table {{ border-collapse: collapse; width: 100%; min-width: 76rem; }}
    caption {{ font-weight: 700; text-align: left; padding: .75rem 0; }}
    th, td {{ border: 1px solid currentColor; padding: .5rem; vertical-align: top; }}
    thead th {{ position: sticky; top: 0; background: Canvas; }}
    code {{ overflow-wrap: anywhere; }}
    .commit {{ font-size: .8rem; }}
    .number {{ text-align: right; font-variant-numeric: tabular-nums; }}
    footer {{ margin-top: 1rem; font-size: .85rem; }}
    a {{ color: LinkText; }}
  </style>
</head>
<body>
<main
  data-repository-count="{manifest['repository_count']}"
  data-tracked-files="{manifest['total_tracked_files']}"
  data-gitlinks="{manifest['total_gitlinks']}"
  data-manifest-sha256="{html.escape(digest, quote=True)}">
  <h1>HypeSiege and StreemPilot repository fleet audit</h1>
  <p class="boundary" role="status">
    <strong>Boundary:</strong> deterministic histories are sealed locally; this report does not claim remote publication.
    Live mutation remains one explicitly confirmed repository at a time.
  </p>
  <section class="summary" aria-label="Fleet totals">
    <div><span>Repositories</span><strong>{manifest['repository_count']}</strong></div>
    <div><span>Tracked files</span><strong>{manifest['total_tracked_files']}</strong></div>
    <div><span>Gitlinks</span><strong>{manifest['total_gitlinks']}</strong></div>
    <div><span>HypeSiege</span><strong>{organizations['hypesiege']}</strong></div>
    <div><span>StreemPilot</span><strong>{organizations['streempilot']}</strong></div>
  </section>
  <div class="table-wrap" tabindex="0" aria-label="Scrollable repository table">
    <table>
      <caption>Canonical child-first publication ledger</caption>
      <thead>
        <tr><th scope="col">#</th><th scope="col">Repository</th><th scope="col">Kind</th><th scope="col">Files</th><th scope="col">Gitlinks</th><th scope="col">Commit</th><th scope="col">Description</th></tr>
      </thead>
      <tbody>{''.join(rows)}</tbody>
    </table>
  </div>
  <footer>
    Manifest SHA-256: <code>{html.escape(digest)}</code>. Generated from the checked-in schema-v2 ledger without credentials or network access.
  </footer>
</main>
</body>
</html>
"""
    if len(report.encode("utf-8")) > MAX_REPORT_BYTES:
        raise PublicationError("HTML audit report exceeded 1 MiB")
    for marker in SECRET_MARKERS:
        if marker in report:
            raise PublicationError(f"HTML audit report contains forbidden marker: {marker}")
    return report


def write_text_atomic(path: pathlib.Path, content: str) -> None:
    if path.exists() and path.is_symlink():
        raise PublicationError("refusing to replace a symlink report path")
    if len(content.encode("utf-8")) > MAX_REPORT_BYTES:
        raise PublicationError("audit output exceeded 1 MiB")
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary = pathlib.Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--manifest",
        type=pathlib.Path,
        default=pathlib.Path("repository-fleets/hypesiege-streempilot.json"),
    )
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--metadata-out", type=pathlib.Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    manifest = load_manifest(args.manifest)
    digest = manifest_digest(args.manifest)
    report = render_report(manifest, digest=digest)
    write_text_atomic(args.output, report)
    if args.metadata_out:
        metadata = {
            "manifest_sha256": digest,
            "report_sha256": hashlib.sha256(report.encode("utf-8")).hexdigest(),
            "repository_count": manifest["repository_count"],
            "total_tracked_files": manifest["total_tracked_files"],
            "total_gitlinks": manifest["total_gitlinks"],
            "network_mutation": False,
            "remote_publication_claimed": False,
        }
        write_text_atomic(
            args.metadata_out,
            json.dumps(metadata, indent=2, sort_keys=True) + "\n",
        )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except PublicationError as error:
        raise SystemExit(f"audit report refused: {error}") from None
