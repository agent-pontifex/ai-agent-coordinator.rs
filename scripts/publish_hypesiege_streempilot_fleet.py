#!/usr/bin/env python3
"""Fail-closed publisher for one deterministic HypeSiege/StreemPilot repository.

Plan mode is credential-free and network-free. Execute mode requires one exact
repository confirmation, an independently reconstructed source tree, and a
short-lived least-privilege GitHub App installation token supplied only through
the environment.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import stat
import subprocess
import tempfile
import urllib.error
import urllib.request
from collections.abc import Iterable
from typing import Any

API_BASE = "https://api.github.com"
API_VERSION = "2022-11-28"
USER_AGENT = "ai-agent-coordinator-hypesiege-streempilot-publisher"
TOKEN_ENV = "GITHUB_REPOSITORY_ADMIN_TOKEN"
ALLOWED_ORGS = frozenset({"hypesiege", "streempilot"})
EXPECTED_ORGANIZATIONS = {"hypesiege": 15, "streempilot": 17}
EXPECTED_GENERATOR_SHA256 = "a57b00961ee57ae09bf3bb2e2d09afbdd1ddbbbde832b027802f82a1fc5dfa84"
EXPECTED_GENERATED_AT = "2026-07-31T00:00:00-04:00"
EXPECTED_PUBLICATION_STATUS = "deterministic histories sealed; remote authorization required"
EXPECTED_REPOSITORIES = 32
EXPECTED_FILES = 888
EXPECTED_GITLINKS = 30
MAX_API_RESPONSE_BYTES = 256 * 1024
MAX_API_REQUEST_BYTES = 64 * 1024
MAX_ERROR_BYTES = 4096
MAX_DESCRIPTION_LENGTH = 350
REPOSITORY_NAME_PATTERN = re.compile(
    r"(?:[a-z0-9]|[a-z0-9][a-z0-9._-]{0,98}[a-z0-9])\Z"
)
KIND_PATTERN = re.compile(r"[a-z][a-z0-9-]{1,31}\Z")
SHA_PATTERN = re.compile(r"[0-9a-f]{40}\Z")
TOKEN_PATTERN = re.compile(r"[A-Za-z0-9_]{20,255}\Z")
RECORD_FIELDS = frozenset(
    {
        "org",
        "name",
        "full_name",
        "kind",
        "commit",
        "files",
        "remote",
        "description",
        "visibility",
        "default_branch",
        "gitlinks",
    }
)
MANIFEST_FIELDS = frozenset(
    {
        "schema_version",
        "generated_at",
        "generator_sha256",
        "default_branch",
        "repository_count",
        "total_tracked_files",
        "total_gitlinks",
        "organizations",
        "publication_status",
        "repositories",
    }
)
REPOSITORY_SETTINGS = {
    "has_issues": True,
    "has_projects": False,
    "has_wiki": False,
    "allow_squash_merge": True,
    "allow_merge_commit": True,
    "allow_rebase_merge": False,
    "delete_branch_on_merge": True,
}


class PublicationError(RuntimeError):
    """The requested publication violated a reviewed invariant."""


class NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Never forward authorization to a redirect target."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: ANN001
        return None


def sanitize_detail(value: object, *, token: str | None = None) -> str:
    detail = str(value)
    if token:
        detail = detail.replace(token, "[REDACTED]")
    for pattern in SECRET_PATTERNS:
        detail = pattern.sub("[REDACTED]", detail)
    detail = "".join(
        character if character in "\n\t" or ord(character) >= 32 else "?"
        for character in detail
    )
    return detail.strip()[:MAX_ERROR_DETAIL_BYTES]


class NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Reject redirects so credentials never cross an unexpected origin."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: ANN001
        return None


def git_binary() -> str:
    located = shutil.which("git")
    if located is None:
        raise PublicationError("git executable is unavailable")
    resolved = pathlib.Path(located).resolve()
    if not resolved.is_file():
        raise PublicationError("git executable is not a regular file")
    return str(resolved)


def base_git_environment() -> dict[str, str]:
    binary = pathlib.Path(git_binary())
    environment = {
        key: os.environ[key]
        for key in ("SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "TMPDIR", "TEMP", "TMP")
        if key in os.environ
    }
    safe_path = [str(binary.parent), *os.defpath.split(os.pathsep)]
    environment.update(
        {
            "PATH": os.pathsep.join(dict.fromkeys(safe_path)),
            "LANG": "C",
            "LC_ALL": "C",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_PROTOCOL_FROM_USER": "0",
            "GIT_ALLOW_PROTOCOL": "https:file",
        }
    )
    return environment


def run(
    args: list[str],
    cwd: pathlib.Path,
    *,
    allowed_returncodes: frozenset[int] = frozenset({0}),
) -> str:
    command = list(args)
    environment = None
    if command and command[0] == "git":
        command[0] = git_binary()
        environment = base_git_environment()
    completed = subprocess.run(
        command,
        cwd=cwd,
        env=environment,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=120,
    )
    if completed.returncode not in allowed_returncodes:
        detail = (completed.stderr or completed.stdout).strip()[:MAX_ERROR_BYTES]
        raise PublicationError(f"{' '.join(args)} failed in {cwd}: {detail}")
    return completed.stdout


def canonical_remote(full_name: str) -> str:
    return f"https://github.com/{full_name}.git"


def validate_record(record: Any) -> dict[str, Any]:
    if not isinstance(record, dict) or set(record) != RECORD_FIELDS:
        raise PublicationError("fleet manifest repository fields changed")

    org = record.get("org")
    name = record.get("name")
    full_name = record.get("full_name")
    if org not in ALLOWED_ORGS:
        raise PublicationError("repository organization is outside the approved fleet")
    if not isinstance(name, str) or not REPOSITORY_NAME_PATTERN.fullmatch(name):
        raise PublicationError("repository name is not a canonical lowercase GitHub name")
    if ".." in name or name.endswith(".git"):
        raise PublicationError("repository name contains an ambiguous path or Git suffix")
    if full_name != f"{org}/{name}":
        raise PublicationError("repository full_name is not canonical")
    if record.get("remote") != canonical_remote(full_name):
        raise PublicationError("repository remote must be the canonical GitHub HTTPS URL")
    if record.get("default_branch") != "main":
        raise PublicationError("repository default branch must be main")
    if record.get("visibility") not in {"public", "private"}:
        raise PublicationError("repository visibility must be explicit")
    if not isinstance(record.get("kind"), str) or not KIND_PATTERN.fullmatch(
        record["kind"]
    ):
        raise PublicationError("repository kind is malformed")
    if not isinstance(record.get("commit"), str) or not SHA_PATTERN.fullmatch(
        record["commit"]
    ):
        raise PublicationError("repository commit must be a full lowercase SHA")
    if (
        not isinstance(record.get("files"), int)
        or isinstance(record.get("files"), bool)
        or not 1 <= record["files"] <= 100_000
    ):
        raise PublicationError("repository tracked-file count is malformed")
    if (
        not isinstance(record.get("gitlinks"), int)
        or isinstance(record.get("gitlinks"), bool)
        or not 0 <= record["gitlinks"] <= EXPECTED_GITLINKS
    ):
        raise PublicationError("repository gitlink count is malformed")
    description = record.get("description")
    if (
        not isinstance(description, str)
        or description != description.strip()
        or not description
        or len(description) > MAX_DESCRIPTION_LENGTH
        or any(ord(character) < 32 or ord(character) == 127 for character in description)
    ):
        raise PublicationError("repository description is malformed")

    expected_gitlinks = 0
    if record["kind"] == "monorepo":
        expected_gitlinks = 14 if org == "hypesiege" else 16
    if record["gitlinks"] != expected_gitlinks:
        raise PublicationError("repository gitlink count violates the fleet topology")
    return record


def load_manifest(path: pathlib.Path) -> dict[str, Any]:
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise PublicationError(f"unable to read fleet manifest: {error}") from error

    if not isinstance(manifest, dict) or set(manifest) != MANIFEST_FIELDS:
        raise PublicationError("unsupported or malformed fleet manifest")
    repositories = manifest.get("repositories")
    if manifest.get("schema_version") != 2 or not isinstance(repositories, list):
        raise PublicationError("unsupported or malformed fleet manifest")
    if manifest.get("generator_sha256") != EXPECTED_GENERATOR_SHA256:
        raise PublicationError("fleet manifest generator checksum changed")
    if manifest.get("default_branch") != "main":
        raise PublicationError("fleet manifest default branch changed")
    if manifest.get("repository_count") != EXPECTED_REPOSITORIES:
        raise PublicationError("fleet manifest repository count changed")
    if manifest.get("repository_count") != len(repositories):
        raise PublicationError("repository_count does not match repositories")
    if manifest.get("total_tracked_files") != EXPECTED_FILES:
        raise PublicationError("fleet manifest tracked-file total changed")
    if manifest.get("total_gitlinks") != EXPECTED_GITLINKS:
        raise PublicationError("fleet manifest gitlink total changed")
    if manifest.get("organizations") != EXPECTED_ORGANIZATIONS:
        raise PublicationError("fleet manifest organization counts changed")
    generated_at = manifest.get("generated_at")
    publication_status = manifest.get("publication_status")
    if (
        not isinstance(generated_at, str)
        or not re.fullmatch(
            r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}"
            r"(?:Z|[+-][0-9]{2}:[0-9]{2})",
            generated_at,
        )
    ):
        raise PublicationError("fleet manifest generated_at is malformed")
    if (
        not isinstance(publication_status, str)
        or publication_status != publication_status.strip()
        or not publication_status
        or len(publication_status) > 200
        or any(ord(character) < 32 or ord(character) == 127 for character in publication_status)
    ):
        raise PublicationError("fleet manifest publication_status is malformed")

    validated = [validate_record(record) for record in repositories]
    full_names = [record["full_name"] for record in validated]
    if len(set(full_names)) != len(full_names):
        raise PublicationError("fleet manifest contains duplicate repositories")
    if sum(record["files"] for record in validated) != EXPECTED_FILES:
        raise PublicationError("repository file counts do not match the fleet total")
    if sum(record["gitlinks"] for record in validated) != EXPECTED_GITLINKS:
        raise PublicationError("repository gitlink counts do not match the fleet total")

    derived_organizations = {
        org: sum(record["org"] == org for record in validated)
        for org in sorted(ALLOWED_ORGS)
    }
    if derived_organizations != EXPECTED_ORGANIZATIONS:
        raise PublicationError("repository organization membership changed")
    for org in sorted(ALLOWED_ORGS):
        org_records = [record for record in validated if record["org"] == org]
        monorepositories = [
            record for record in org_records if record["kind"] == "monorepo"
        ]
        if len(monorepositories) != 1:
            raise PublicationError(f"{org} must contain exactly one monorepository")
        if org_records[-1] is not monorepositories[0]:
            raise PublicationError(f"{org} monorepository must publish last")
        if monorepositories[0]["name"] != f"{org}-monorepo":
            raise PublicationError(f"{org} monorepository name changed")
    return manifest


def select_record(manifest: dict[str, Any], full_name: str) -> dict[str, Any]:
    matches = [record for record in manifest["repositories"] if record["full_name"] == full_name]
    if len(matches) != 1:
        raise PublicationError(f"manifest must contain exactly one {full_name!r} record")
    return validate_record(matches[0])


def staged_gitlinks(repo: pathlib.Path) -> dict[str, str]:
    gitlinks: dict[str, str] = {}
    for line in run(["git", "ls-files", "--stage"], repo).splitlines():
        try:
            metadata, path = line.split("\t", 1)
            mode, object_id, stage = metadata.split()
        except ValueError as error:
            raise PublicationError(f"malformed Git index entry in {repo}") from error
        if stage != "0":
            raise PublicationError(f"unmerged Git index entry in {repo}: {path}")
        if mode == "160000":
            if path in gitlinks or not SHA_PATTERN.fullmatch(object_id):
                raise PublicationError(f"malformed Gitlink entry in {repo}: {path}")
            gitlinks[path] = object_id
    return gitlinks


def gitmodule_entries(repo: pathlib.Path) -> dict[str, str]:
    path = repo / ".gitmodules"
    if not path.is_file():
        return {}
    output = run(
        [
            "git",
            "config",
            "--file",
            ".gitmodules",
            "--get-regexp",
            r"^submodule\..*\.(path|url)$",
        ],
        repo,
        allowed_returncodes=frozenset({0, 1}),
    )

    sections: dict[str, dict[str, str]] = {}
    pattern = re.compile(r"submodule\.(.+)\.(path|url)\Z")
    for line in output.splitlines():
        if not line.strip():
            continue
        try:
            key, value = line.split(maxsplit=1)
        except ValueError as error:
            raise PublicationError(f"malformed {path} entry") from error
        match = pattern.fullmatch(key)
        if match is None or not value:
            raise PublicationError(f"malformed {path} entry")
        section, field = match.groups()
        if field in sections.setdefault(section, {}):
            raise PublicationError(f"duplicate {field} in {path}")
        sections[section][field] = value

    entries: dict[str, str] = {}
    for section, fields in sections.items():
        if set(fields) != {"path", "url"}:
            raise PublicationError(f"incomplete submodule {section!r} in {path}")
        module_path = fields["path"]
        if module_path in entries:
            raise PublicationError(f"duplicate submodule path in {path}: {module_path}")
        entries[module_path] = fields["url"]
    return entries


def unsafe_local_git_config(
    repo: pathlib.Path,
    *,
    allow_core_worktree: bool = False,
) -> list[str]:
    output = run(
        ["git", "config", "--local", "--no-includes", "--name-only", "--list"],
        repo,
    )
    unsafe: list[str] = []
    exact = {
        "core.askpass",
        "core.gitproxy",
        "core.hookspath",
        "core.sshcommand",
        "core.fsmonitor",
        "core.worktree",
        "core.attributesfile",
        "core.alternaterefscommand",
        "core.usereplacerefs",
        "diff.external",
        "interactive.difffilter",
        "remote.origin.pushurl",
    }
    patterns = (
        re.compile(r"credential(?:\..*)?\Z"),
        re.compile(r"http(?:\..*)?\Z"),
        re.compile(r"url\..*\.insteadof\Z"),
        re.compile(
            r"remote\..*\.(?:pushurl|proxy|proxyauthmethod|receivepack|uploadpack)\Z"
        ),
        re.compile(r"protocol(?:\..*)?\Z"),
        re.compile(r"include(?:if)?(?:\..*)?\Z"),
        re.compile(r"filter\..*\.(?:clean|smudge|process|required)\Z"),
        re.compile(r"diff\..*\.(?:command|textconv|cachetextconv)\Z"),
        re.compile(r"merge\..*\.driver\Z"),
        re.compile(r"fsck(?:\..*)?\Z"),
    )
    for line in output.splitlines():
        key = line.strip().casefold()
        if not key:
            continue
        if allow_core_worktree and key == "core.worktree":
            continue
        if key in exact or any(pattern.fullmatch(key) for pattern in patterns):
            unsafe.append(key)
    return sorted(set(unsafe))


def validate_git_storage(repo: pathlib.Path) -> pathlib.Path:
    git_dir = pathlib.Path(
        run(["git", "rev-parse", "--absolute-git-dir"], repo).strip()
    ).resolve()
    if not git_dir.is_dir():
        raise PublicationError(f"Git directory is not a directory: {git_dir}")
    forbidden = (
        git_dir / "objects" / "info" / "alternates",
        git_dir / "objects" / "info" / "http-alternates",
        git_dir / "info" / "grafts",
        git_dir / "shallow",
    )
    for path in forbidden:
        if path.exists():
            raise PublicationError(f"unsupported Git object indirection: {path}")
    replacements = run(
        ["git", "for-each-ref", "--format=%(refname)", "refs/replace"],
        repo,
    ).splitlines()
    if replacements:
        raise PublicationError(f"Git replacement refs are forbidden in {repo}")
    return git_dir


def preflight_source(
    manifest: dict[str, Any],
    record: dict[str, Any],
    source_root: pathlib.Path,
) -> pathlib.Path:
    try:
        resolved_root = source_root.resolve(strict=True)
        repo = (resolved_root / record["org"] / record["name"]).resolve(strict=True)
    except OSError as error:
        raise PublicationError(f"unable to resolve publication source: {error}") from error
    if not repo.is_relative_to(resolved_root):
        raise PublicationError("repository source escaped the approved source root")
    if not repo.is_dir() or not (repo / ".git").is_dir():
        raise PublicationError(f"missing independent Git history: {repo}")
    if unsafe := unsafe_local_git_config(repo):
        raise PublicationError(
            f"unsafe local Git configuration in {record['full_name']}: {', '.join(unsafe)}"
        )
    validate_git_storage(repo)
    top_level = pathlib.Path(
        run(["git", "rev-parse", "--show-toplevel"], repo).strip()
    ).resolve()
    if top_level != repo:
        raise PublicationError(f"unexpected Git worktree root: {top_level}")

    checks = {
        "branch": run(["git", "branch", "--show-current"], repo).strip(),
        "head": run(["git", "rev-parse", "HEAD"], repo).strip(),
        "origin": run(["git", "config", "--local", "--get", "remote.origin.url"], repo).strip(),
        "status": run(["git", "status", "--porcelain"], repo),
        "files": len(run(["git", "ls-files"], repo).splitlines()),
        "history_depth": int(run(["git", "rev-list", "--count", "HEAD"], repo).strip()),
        "remotes": run(["git", "remote"], repo).splitlines(),
    }
    if checks["branch"] != "main":
        raise PublicationError(f"{record['full_name']} is not on main")
    if checks["head"] != record["commit"]:
        raise PublicationError(f"{record['full_name']} head mismatch")
    if checks["origin"] != record["remote"]:
        raise PublicationError(f"{record['full_name']} origin mismatch")
    if checks["remotes"] != ["origin"]:
        raise PublicationError(f"{record['full_name']} must contain only the origin remote")
    if checks["status"]:
        raise PublicationError(f"{record['full_name']} working tree is dirty")
    if checks["files"] != record["files"]:
        raise PublicationError(f"{record['full_name']} tracked-file count mismatch")
    if checks["history_depth"] != 1:
        raise PublicationError(f"{record['full_name']} must be a deterministic root commit")

    gitlinks = staged_gitlinks(repo)
    if len(gitlinks) != record["gitlinks"]:
        raise PublicationError(f"{record['full_name']} gitlink count mismatch")
    modules = gitmodule_entries(repo)
    if set(modules) != set(gitlinks):
        raise PublicationError(f"{record['full_name']} .gitmodules/index mismatch")
    if gitlinks:
        child_by_remote = {
            item["remote"]: item
            for item in manifest["repositories"]
            if item["org"] == record["org"] and item["kind"] != "monorepo"
        }
        if len(child_by_remote) != record["gitlinks"]:
            raise PublicationError(f"{record['full_name']} child topology changed")
        for path, expected_commit in gitlinks.items():
            child = child_by_remote.get(modules[path])
            if child is None:
                raise PublicationError(
                    f"non-canonical submodule URL for {record['full_name']}: {modules[path]}"
                )
            if child["commit"] != expected_commit:
                raise PublicationError(
                    f"submodule Gitlink drift for {path}: {expected_commit} != {child['commit']}"
                )
            checkout = (repo / path).resolve()
            if not checkout.is_relative_to(repo) or not (checkout / ".git").exists():
                raise PublicationError(f"unmaterialized submodule checkout: {path}")
            checkout_top = pathlib.Path(
                run(["git", "rev-parse", "--show-toplevel"], checkout).strip()
            ).resolve()
            if checkout_top != checkout:
                raise PublicationError(f"unexpected submodule worktree root: {path}")
            if unsafe := unsafe_local_git_config(
                checkout,
                allow_core_worktree=True,
            ):
                raise PublicationError(
                    f"unsafe submodule Git configuration for {path}: {', '.join(unsafe)}"
                )
            validate_git_storage(checkout)
            child_checks = {
                "head": run(["git", "rev-parse", "HEAD"], checkout).strip(),
                "status": run(["git", "status", "--porcelain"], checkout),
                "history_depth": int(
                    run(["git", "rev-list", "--count", "HEAD"], checkout).strip()
                ),
            }
            if child_checks["head"] != expected_commit:
                raise PublicationError(
                    f"submodule checkout drift for {path}: "
                    f"{child_checks['head']} != {expected_commit}"
                )
            if child_checks["status"]:
                raise PublicationError(f"submodule checkout is dirty: {path}")
            if child_checks["history_depth"] != 1:
                raise PublicationError(f"submodule history is not deterministic: {path}")
            run(["git", "fsck", "--full", "--no-dangling"], checkout)

    run(["git", "diff", "--check", "HEAD"], repo)
    run(["git", "fsck", "--full", "--no-dangling"], repo)
    return repo


def validate_token(token: str | None) -> str:
    if token is None or not TOKEN_PATTERN.fullmatch(token):
        raise PublicationError(
            f"{TOKEN_ENV} must be a short-lived GitHub App installation token"
        )
    return token


def validate_api_path(path: str) -> None:
    if not isinstance(path, str) or not re.fullmatch(
        r"/[A-Za-z0-9._-]+(?:/[A-Za-z0-9._-]+)*",
        path,
    ):
        raise PublicationError("GitHub API path is not canonical")
    if any(component in {".", ".."} for component in path.split("/")):
        raise PublicationError("GitHub API path contains traversal components")


def request_json(
    method: str,
    path: str,
    token: str,
    body: dict[str, Any] | None = None,
) -> tuple[int, dict[str, Any] | None]:
    if method not in {"GET", "POST", "PATCH"}:
        raise PublicationError(f"unsupported GitHub API method: {method}")
    validate_api_path(path)
    validate_token(token)
    if body is not None and not isinstance(body, dict):
        raise PublicationError("GitHub API request body must be a JSON object")
    data = (
        None
        if body is None
        else json.dumps(body, separators=(",", ":"), sort_keys=True).encode("utf-8")
    )
    if data is not None and len(data) > MAX_API_REQUEST_BYTES:
        raise PublicationError("GitHub API request exceeded 64 KiB")
    request = urllib.request.Request(
        API_BASE + path,
        data=data,
        method=method,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": API_VERSION,
            "User-Agent": USER_AGENT,
        },
    )
    if data is not None:
        request.add_header("Content-Type", "application/json")
    opener = urllib.request.build_opener(NoRedirectHandler())
    try:
        with opener.open(request, timeout=30) as response:
            raw = response.read(MAX_API_RESPONSE_BYTES + 1)
            if len(raw) > MAX_API_RESPONSE_BYTES:
                raise PublicationError("GitHub API response exceeded 256 KiB")
            if not raw:
                return response.status, None
            content_type = response.headers.get_content_type()
            if content_type not in {"application/json", "application/vnd.github+json"}:
                raise PublicationError(
                    f"GitHub API returned unexpected content type: {content_type}"
                )
            try:
                payload = json.loads(raw)
            except (UnicodeError, json.JSONDecodeError) as error:
                raise PublicationError("GitHub API returned malformed JSON") from error
            if not isinstance(payload, dict):
                raise PublicationError("GitHub API returned a non-object JSON payload")
            return response.status, payload
    except urllib.error.HTTPError as error:
        raw = error.read(MAX_ERROR_BYTES).decode("utf-8", errors="replace")
        raw = raw.replace(token, "[REDACTED]")
        if error.code == 404 and method == "GET":
            return error.code, None
        if 300 <= error.code < 400:
            raise PublicationError(
                f"GitHub API redirect rejected for {method} {path}"
            ) from error
        raise PublicationError(
            f"GitHub API unavailable for {method} {path}: "
            f"{sanitize_detail(reason, token=token)}"
        ) from error


def validate_repository_metadata(
    record: dict[str, Any],
    current: Any,
    *,
    require_settings: bool,
) -> dict[str, Any]:
    if not isinstance(current, dict):
        raise PublicationError("GitHub did not return repository metadata")
    repository_id = current.get("id")
    if (
        not isinstance(repository_id, int)
        or isinstance(repository_id, bool)
        or repository_id <= 0
    ):
        raise PublicationError("GitHub returned an invalid repository ID")
    owner = current.get("owner")
    if (
        not isinstance(owner, dict)
        or owner.get("login", "").casefold() != record["org"].casefold()
        or owner.get("type") != "Organization"
    ):
        raise PublicationError("GitHub returned an unexpected repository owner")
    if current.get("full_name", "").casefold() != record["full_name"].casefold():
        raise PublicationError("GitHub returned an unexpected repository")
    if current.get("name", "").casefold() != record["name"].casefold():
        raise PublicationError("GitHub returned an unexpected repository name")
    if current.get("visibility") != record["visibility"]:
        raise PublicationError(
            f"visibility mismatch: {current.get('visibility')} != {record['visibility']}"
        )
    if current.get("private") is not (record["visibility"] == "private"):
        raise PublicationError("GitHub returned inconsistent repository privacy metadata")
    if current.get("clone_url") != record["remote"]:
        raise PublicationError("GitHub returned a non-canonical clone URL")
    if current.get("html_url") != f"https://github.com/{record['full_name']}":
        raise PublicationError("GitHub returned a non-canonical repository URL")
    if current.get("fork") is not False or current.get("archived") is not False:
        raise PublicationError("repository must be an active, non-fork repository")
    if current.get("disabled") is not False:
        raise PublicationError("repository is disabled or its state is unknown")
    if require_settings:
        expected = {
            "description": record["description"],
            "default_branch": "main",
            **REPOSITORY_SETTINGS,
        }
        for field, value in expected.items():
            if current.get(field) != value:
                raise PublicationError(
                    f"repository setting mismatch for {field}: "
                    f"{current.get(field)!r} != {value!r}"
                )
    return current


def ensure_repository(record: dict[str, Any], token: str) -> tuple[dict[str, Any], bool]:
    status, current = request_json("GET", f"/repos/{record['full_name']}", token)
    if status not in {200, 404}:
        raise PublicationError(f"unexpected repository lookup status: {status}")
    created = status == 404
    if created:
        create_status, current = request_json(
            "POST",
            f"/orgs/{record['org']}/repos",
            token,
            {
                "name": record["name"],
                "description": record["description"],
                "private": record["visibility"] == "private",
                "auto_init": False,
                **REPOSITORY_SETTINGS,
            },
        )
        if create_status != 201:
            raise PublicationError(f"unexpected repository creation status: {create_status}")
    return validate_repository_metadata(record, current, require_settings=False), created


def configure_repository(record: dict[str, Any], token: str) -> dict[str, Any]:
    status, current = request_json(
        "PATCH",
        f"/repos/{record['full_name']}",
        token,
        {
            "description": record["description"],
            "default_branch": "main",
            **REPOSITORY_SETTINGS,
        },
    )
    if status != 200:
        raise PublicationError(f"unexpected repository configuration status: {status}")
    return validate_repository_metadata(record, current, require_settings=True)


def remote_main_commit(full_name: str, token: str) -> str | None:
    status, payload = request_json("GET", f"/repos/{full_name}/commits/main", token)
    if status == 404:
        return None
    if status != 200:
        raise PublicationError(f"unexpected main-commit lookup status: {status}")
    if (
        not isinstance(payload, dict)
        or not isinstance(payload.get("sha"), str)
        or not SHA_PATTERN.fullmatch(payload["sha"])
    ):
        raise PublicationError(f"GitHub returned invalid main-commit metadata for {full_name}")
    return payload["sha"]


def verify_monorepo_children(
    manifest: dict[str, Any], record: dict[str, Any], token: str
) -> None:
    if record["kind"] != "monorepo":
        return
    children = [
        item
        for item in manifest["repositories"]
        if item["org"] == record["org"] and item["kind"] != "monorepo"
    ]
    for child in children:
        actual = remote_main_commit(child["full_name"], token)
        if actual != child["commit"]:
            raise PublicationError(
                f"cannot publish {record['full_name']}: child {child['full_name']} "
                f"remote main is {actual!r}, expected {child['commit']}"
            )


def sanitized_git_environment(token: str, askpass: pathlib.Path) -> dict[str, str]:
    environment = base_git_environment()
    environment.update(
        {
            TOKEN_ENV: validate_token(token),
            "GIT_ASKPASS": str(askpass),
            "GIT_ASKPASS_REQUIRE": "force",
        }
    )
    return environment


def push_main(repo: pathlib.Path, remote: str, token: str) -> None:
    directory = pathlib.Path(tempfile.mkdtemp(prefix="fleet-git-askpass-"))
    try:
        directory.chmod(stat.S_IRWXU)
        askpass = directory / "askpass.sh"
        askpass.write_text(
            "#!/bin/sh\n"
            'case "$1" in\n'
            '  *Username*) printf "%s\\n" x-access-token ;;\n'
            f'  *) printf "%s\\n" "${{{TOKEN_ENV}}}" ;;\n'
            "esac\n",
            encoding="utf-8",
        )
        askpass.chmod(stat.S_IRWXU)
        hooks = directory / "hooks"
        hooks.mkdir(mode=stat.S_IRWXU)
        environment = sanitized_git_environment(token, askpass)
        completed = subprocess.run(
            [
                git_binary(),
                "-c",
                f"core.hooksPath={hooks}",
                "-c",
                "credential.helper=",
                "push",
                "--porcelain",
                "--set-upstream",
                remote,
                "main:refs/heads/main",
            ],
            cwd=repo,
            env=environment,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=120,
        )
        if completed.returncode != 0:
            detail = (completed.stderr or completed.stdout).strip()[:MAX_ERROR_BYTES]
            detail = detail.replace(token, "[REDACTED]")
            raise PublicationError(f"git push failed for {repo.name}: {detail}")
    finally:
        shutil.rmtree(directory, ignore_errors=True)


def publish_record(
    manifest: dict[str, Any],
    record: dict[str, Any],
    repo: pathlib.Path,
    token: str,
) -> dict[str, Any]:
    verify_monorepo_children(manifest, record, token)
    current, created = ensure_repository(record, token)
    existing = remote_main_commit(record["full_name"], token)
    if existing not in {None, record["commit"]}:
        raise PublicationError(
            f"refusing to modify divergent remote main for {record['full_name']}: "
            f"{existing} != {record['commit']}"
        )
    pushed = existing is None
    if pushed:
        push_main(repo, record["remote"], token)
    current = configure_repository(record, token)
    actual = remote_main_commit(record["full_name"], token)
    if actual != record["commit"]:
        raise PublicationError(
            f"remote verification failed for {record['full_name']}: "
            f"{actual!r} != {record['commit']}"
        )
    return {
        "published": record["full_name"],
        "repository_id": current["id"],
        "visibility": current["visibility"],
        "default_branch": current["default_branch"],
        "commit": actual,
        "created": created,
        "pushed": pushed,
        "verified": True,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--manifest",
        type=pathlib.Path,
        default=pathlib.Path("repository-fleets/hypesiege-streempilot.json"),
    )
    parser.add_argument("--source-root", type=pathlib.Path)
    parser.add_argument("--repository", required=True, help="exact owner/name")
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--confirm-repository")
    parser.add_argument("--report-out", type=pathlib.Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    manifest = load_manifest(args.manifest)
    record = select_record(manifest, args.repository)
    plan = {
        "mode": "execute" if args.execute else "plan",
        "repository": record["full_name"],
        "commit": record["commit"],
        "visibility": record["visibility"],
        "remote": record["remote"],
        "files": record["files"],
        "gitlinks": record["gitlinks"],
    }
    print(json.dumps(plan, indent=2))
    if not args.execute:
        if args.report_out:
            write_json_atomic(
                args.report_out,
                {**plan, "manifest_sha256": manifest_digest(args.manifest), "network_mutation": False},
            )
        return 0
    if args.confirm_repository != record["full_name"]:
        raise PublicationError("--confirm-repository must exactly equal the requested owner/name")
    if args.source_root is None:
        raise PublicationError("--source-root is required in execute mode")
    repo = preflight_source(manifest, record, args.source_root)
    token = validate_token(os.environ.get(TOKEN_ENV))

    result = publish_record(manifest, record, repo, token)
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except PublicationError as error:
        raise SystemExit(f"publication failed: {error}") from error
