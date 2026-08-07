#!/usr/bin/env python3
"""Validate MemeBank's PostgreSQL desired-state and safety invariants."""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import sys
from pathlib import Path
from typing import Any

RENDER_PATH = Path(__file__).with_name("render_schema.py")
SPEC = importlib.util.spec_from_file_location("memebank_render_schema", RENDER_PATH)
assert SPEC is not None and SPEC.loader is not None
RENDER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = RENDER
SPEC.loader.exec_module(RENDER)


class SchemaValidationError(ValueError):
    pass


EXPECTED_TABLES = {
    "memebank.libraries",
    "memebank.library_memberships",
    "memebank.assets",
    "memebank.asset_variants",
    "memebank.perceptual_hashes",
    "memebank.storage_connections",
    "memebank.storage_locations",
    "memebank.storage_location_events",
    "memebank.enrichment_observations",
    "memebank.ocr_regions",
    "memebank.tags",
    "memebank.asset_tag_decisions",
    "memebank.embedding_models",
    "memebank.embedding_search_routes",
    "memebank.asset_search_documents",
    "memebank.asset_embeddings_384",
    "memebank.asset_embeddings_768",
    "memebank.asset_embeddings_1024",
    "memebank.jobs",
    "memebank.job_attempts",
    "memebank.job_events",
    "memebank.outbox_events",
    "memebank.export_requests",
    "memebank.deletion_requests",
    "memebank.reconciliation_runs",
    "memebank.audit_events",
    "memebank_private.blobs",
}

TENANT_TABLES = EXPECTED_TABLES - {
    "memebank.libraries",
    "memebank.embedding_models",
    "memebank.embedding_search_routes",
    "memebank_private.blobs",
}

APP_READABLE_TABLES = EXPECTED_TABLES - {
    "memebank.outbox_events",
    "memebank_private.blobs",
}

WORKER_SCOPED_TABLES = TENANT_TABLES

FORBIDDEN_PATTERNS = {
    "destructive DROP": re.compile(r"(?im)^\s*DROP\s+"),
    "destructive TRUNCATE": re.compile(r"(?im)^\s*TRUNCATE\s+"),
    "table IF NOT EXISTS": re.compile(r"(?i)CREATE\s+TABLE\s+IF\s+NOT\s+EXISTS"),
    "disabled RLS": re.compile(r"(?i)DISABLE\s+ROW\s+LEVEL\s+SECURITY"),
    "application BYPASSRLS": re.compile(r"(?i)ALTER\s+ROLE\s+(mb_app|mb_worker)[^;]*\bBYPASSRLS\b"),
    "psql command in desired state": re.compile(r"(?m)^\s*\\[A-Za-z]"),
}

FORBIDDEN_SECRET_FIELDS = re.compile(
    r"(?i)\b(access_token|refresh_token|password|private_key|secret_value|presigned_url|download_url|object_url)\b"
)


def read_text(path: Path) -> str:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise SchemaValidationError(f"cannot read {path}: {error}") from error
    if "\r" in text:
        raise SchemaValidationError(f"CRLF is not permitted: {path}")
    if not text.endswith("\n"):
        raise SchemaValidationError(f"file must end in newline: {path}")
    return text


def table_bodies(sql: str) -> dict[str, str]:
    return {
        match.group(1).lower(): match.group(2)
        for match in re.finditer(
            r"(?ims)^CREATE\s+TABLE\s+((?:memebank|memebank_private)\.[a-z0-9_]+)\s*\((.*?)\n\);",
            sql,
        )
    }


def names_after(pattern: str, sql: str) -> set[str]:
    return {match.lower() for match in re.findall(pattern, sql, re.IGNORECASE | re.MULTILINE)}


def validate_transactions(paths: list[Path]) -> None:
    for path in paths:
        text = read_text(path)
        if len(re.findall(r"(?im)^BEGIN;\s*$", text)) != 1:
            raise SchemaValidationError(f"{path.name} must contain exactly one BEGIN")
        if len(re.findall(r"(?im)^COMMIT;\s*$", text)) != 1:
            raise SchemaValidationError(f"{path.name} must contain exactly one COMMIT")
        if not text.lstrip().startswith("BEGIN;") or not text.rstrip().endswith("COMMIT;"):
            raise SchemaValidationError(
                f"{path.name} must be transaction-bounded from BEGIN through COMMIT"
            )


def validate_forbidden(sql: str, root: Path) -> None:
    for label, pattern in FORBIDDEN_PATTERNS.items():
        if pattern.search(sql):
            raise SchemaValidationError(f"forbidden {label} found in desired state")
    if FORBIDDEN_SECRET_FIELDS.search(sql):
        raise SchemaValidationError("secret-bearing field name found in desired state")

    for path in sorted((root / "seeds").glob("*.sql")) + sorted(
        (root / "queries").glob("*.sql")
    ):
        text = read_text(path)
        if FORBIDDEN_SECRET_FIELDS.search(text):
            raise SchemaValidationError(f"secret-bearing content found in {path}")
        if re.search(r"(?i)https://[^\s'\"]+(amazonaws|cloudflarestorage)\.", text):
            raise SchemaValidationError(f"private provider URL found in {path}")


def validate_extensions(sql: str) -> None:
    extensions = names_after(
        r"CREATE\s+EXTENSION\s+IF\s+NOT\s+EXISTS\s+([a-z0-9_]+)", sql
    )
    if extensions != {"pgcrypto", "pg_trgm", "vector"}:
        raise SchemaValidationError(
            f"extensions must be exactly pgcrypto, pg_trgm, vector; got {sorted(extensions)}"
        )


def validate_tables_and_tenancy(sql: str) -> dict[str, str]:
    bodies = table_bodies(sql)
    actual = set(bodies)
    missing = sorted(EXPECTED_TABLES - actual)
    unexpected = sorted(actual - EXPECTED_TABLES)
    if missing or unexpected:
        raise SchemaValidationError(
            f"table contract mismatch; missing={missing}, unexpected={unexpected}"
        )
    for table in sorted(TENANT_TABLES):
        if not re.search(r"(?im)^\s*library_id\s+uuid\s+NOT\s+NULL\b", bodies[table]):
            raise SchemaValidationError(f"tenant table lacks NOT NULL library_id: {table}")
    for table in sorted(
        {
            "memebank.libraries",
            "memebank.library_memberships",
            "memebank.assets",
            "memebank.asset_variants",
            "memebank.storage_connections",
            "memebank.storage_locations",
            "memebank.enrichment_observations",
            "memebank.tags",
            "memebank.asset_tag_decisions",
            "memebank.export_requests",
            "memebank.deletion_requests",
        }
    ):
        body = bodies[table]
        if not re.search(r"(?im)^\s*revision\s+bigint\s+NOT\s+NULL", body):
            raise SchemaValidationError(f"mutable table lacks revision fencing: {table}")
        if not re.search(r"(?im)^\s*updated_at\s+timestamptz\s+NOT\s+NULL", body):
            raise SchemaValidationError(f"mutable table lacks updated_at: {table}")
    return bodies


def validate_rls(sql: str) -> None:
    enabled = names_after(
        r"ALTER\s+TABLE\s+((?:memebank|memebank_private)\.[a-z0-9_]+)\s+ENABLE\s+ROW\s+LEVEL\s+SECURITY",
        sql,
    )
    forced = names_after(
        r"ALTER\s+TABLE\s+((?:memebank|memebank_private)\.[a-z0-9_]+)\s+FORCE\s+ROW\s+LEVEL\s+SECURITY",
        sql,
    )
    if enabled != EXPECTED_TABLES or forced != EXPECTED_TABLES:
        raise SchemaValidationError(
            "every expected table must enable and force RLS; "
            f"enabled_missing={sorted(EXPECTED_TABLES - enabled)}, "
            f"forced_missing={sorted(EXPECTED_TABLES - forced)}"
        )

    policy_pairs = {
        (match.group(2).lower(), match.group(1).lower())
        for match in re.finditer(
            r"(?im)^CREATE\s+POLICY\s+([a-z0-9_]+)\s+ON\s+((?:memebank|memebank_private)\.[a-z0-9_]+)",
            sql,
        )
    }
    policy_tables = {table for table, _ in policy_pairs}
    missing_policy_tables = sorted(EXPECTED_TABLES - policy_tables)
    if missing_policy_tables:
        raise SchemaValidationError(f"tables without policies: {missing_policy_tables}")

    for table in sorted(APP_READABLE_TABLES):
        table_sql = "\n".join(
            match.group(0)
            for match in re.finditer(
                rf"(?ims)^CREATE\s+POLICY\s+[a-z0-9_]+\s+ON\s+{re.escape(table)}\b.*?;",
                sql,
            )
        )
        if "TO mb_app" not in table_sql or "FOR SELECT" not in table_sql:
            raise SchemaValidationError(f"app SELECT policy missing for {table}")

    for table in sorted(WORKER_SCOPED_TABLES):
        table_sql = "\n".join(
            match.group(0)
            for match in re.finditer(
                rf"(?ims)^CREATE\s+POLICY\s+[a-z0-9_]+\s+ON\s+{re.escape(table)}\b.*?;",
                sql,
            )
        )
        if "TO mb_worker" not in table_sql:
            raise SchemaValidationError(f"worker policy missing for {table}")
        if "worker_has_library_access(library_id)" not in table_sql:
            raise SchemaValidationError(f"worker policy is not library scoped for {table}")

    if "NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS" not in read_text(
        Path(__file__).resolve().parents[1] / "bootstrap" / "roles.sql"
    ):
        raise SchemaValidationError("application role bootstrap is not fail closed")
    bootstrap = read_text(Path(__file__).resolve().parents[1] / "bootstrap" / "roles.sql")
    if not re.search(r"(?i)mb_policy_owner\s+NOLOGIN[^;]*\bBYPASSRLS\b", bootstrap):
        raise SchemaValidationError("mb_policy_owner must be no-login BYPASSRLS")
    if re.search(r"(?i)GRANT\s+mb_policy_owner\s+TO", bootstrap):
        raise SchemaValidationError("mb_policy_owner must never be granted to a login role")


def validate_security_definers(sql: str) -> None:
    definitions = list(
        re.finditer(
            r"(?ims)^CREATE\s+FUNCTION\s+((?:memebank|memebank_private)\.[a-z0-9_]+)\s*\(.*?\)\s*RETURNS\s+.*?\$\$.*?\$\$;",
            sql,
        )
    )
    security_definers = [
        match for match in definitions if re.search(r"(?i)SECURITY\s+DEFINER", match.group(0))
    ]
    required = {
        "memebank_private.has_library_access",
        "memebank_private.validate_embedding_model",
        "memebank_private.refresh_asset_search_document",
        "memebank.claim_jobs",
        "memebank.renew_job_lease",
    }
    actual = {match.group(1).lower() for match in security_definers}
    if not required.issubset(actual):
        raise SchemaValidationError(
            f"missing security-definer functions: {sorted(required - actual)}"
        )
    for match in security_definers:
        block = match.group(0)
        if not re.search(
            r"(?i)SET\s+search_path\s*=\s*pg_catalog,\s*memebank,\s*memebank_private",
            block,
        ):
            raise SchemaValidationError(
                f"security-definer function lacks restricted search_path: {match.group(1)}"
            )
        if re.search(r"(?i)EXECUTE\s+format\s*\(", block):
            raise SchemaValidationError(
                f"security-definer function uses dynamic SQL: {match.group(1)}"
            )


def validate_search_and_vectors(sql: str, bodies: dict[str, str]) -> None:
    for dimension in (384, 768, 1024):
        table = f"memebank.asset_embeddings_{dimension}"
        if f"embedding vector({dimension}) NOT NULL" not in bodies[table]:
            raise SchemaValidationError(f"{table} must use vector({dimension})")
        for opclass, metric in (
            ("vector_cosine_ops", "cosine"),
            ("vector_ip_ops", "inner_product"),
            ("vector_l2_ops", "l2"),
        ):
            if not re.search(
                rf"(?is)ON\s+{re.escape(table)}\s+USING\s+hnsw\s*\(embedding\s+{opclass}\).*?WHERE\s+metric\s*=\s*'{metric}'",
                sql,
            ):
                raise SchemaValidationError(
                    f"missing {metric} HNSW index for dimension {dimension}"
                )
    if re.search(r"(?i)\bembedding\s+vector\s+NOT\s+NULL", sql):
        raise SchemaValidationError("dimensionless vector column is forbidden")

    search_body = bodies["memebank.asset_search_documents"]
    required_weights = {
        "title_text": "A",
        "confirmed_tags_text": "A",
        "note_text": "B",
        "ocr_text": "B",
        "selected_caption_text": "C",
    }
    for column, weight in required_weights.items():
        pattern = rf"setweight\s*\(\s*to_tsvector\s*\([^;]*?{column}[^;]*?\)\s*,\s*'{weight}'\s*\)"
        if not re.search(pattern, search_body, re.IGNORECASE | re.DOTALL):
            raise SchemaValidationError(
                f"search source {column} must retain weight {weight}"
            )
    if "GENERATED ALWAYS AS" not in search_body or ") STORED" not in search_body:
        raise SchemaValidationError("search_vector must be a stored generated column")

    if "cutover_state" not in bodies["memebank.embedding_search_routes"]:
        raise SchemaValidationError("embedding routes must expose cutover state")
    if "shadow_model_id" not in bodies["memebank.embedding_search_routes"]:
        raise SchemaValidationError("embedding routes must support side-by-side shadowing")


def validate_jobs_and_lifecycle(sql: str, bodies: dict[str, str]) -> None:
    jobs = bodies["memebank.jobs"]
    for token in (
        "idempotency_key",
        "lease_owner",
        "lease_epoch",
        "lease_expires_at",
        "attempt_count",
        "max_attempts",
    ):
        if token not in jobs:
            raise SchemaValidationError(f"jobs table lacks {token}")
    claim_block = re.search(
        r"(?ims)^CREATE\s+FUNCTION\s+memebank\.claim_jobs\s*\(.*?\$\$;",
        sql,
    )
    if claim_block is None:
        raise SchemaValidationError("claim_jobs function missing")
    for token in ("FOR UPDATE SKIP LOCKED", "lease_epoch + 1", "current_worker_library_id"):
        if token not in claim_block.group(0):
            raise SchemaValidationError(f"claim_jobs lacks {token}")
    for table in (
        "memebank.export_requests",
        "memebank.deletion_requests",
        "memebank.reconciliation_runs",
    ):
        if "state " not in bodies[table] or "completed_at" not in bodies[table]:
            raise SchemaValidationError(f"lifecycle table lacks explicit state: {table}")
    for table in (
        "memebank.storage_location_events",
        "memebank.job_events",
        "memebank.audit_events",
    ):
        if not re.search(
            rf"(?is)CREATE\s+TRIGGER\s+[a-z0-9_]+.*?ON\s+{re.escape(table)}.*?forbid_append_only_mutation",
            sql,
        ):
            raise SchemaValidationError(f"append-only mutation guard missing for {table}")


def validate_queries(root: Path) -> None:
    hybrid = read_text(root / "queries" / "hybrid_search_768.sql")
    for placeholder in ("$1", "$2", "$3", "$4", "$5"):
        if placeholder not in hybrid:
            raise SchemaValidationError(f"hybrid query lacks {placeholder}")
    for token in (
        "websearch_to_tsquery",
        "vector(768)",
        "LIMIT ($5 * 4)",
        "reciprocal_rank_score",
        "embedding.metric = 'cosine'",
    ):
        if token not in hybrid:
            raise SchemaValidationError(f"hybrid query lacks {token}")
    if re.search(r"\{\{|\$\{|%\([^)]+\)s", hybrid):
        raise SchemaValidationError("hybrid query contains interpolation syntax")

    cutover = read_text(root / "queries" / "cutover_embedding_model.sql")
    for token in ("shadow_model_id", "route.revision = $4", "model.status = 'active'"):
        if token not in cutover:
            raise SchemaValidationError(f"model cutover query lacks {token}")


def validate_tests_and_seed(root: Path) -> None:
    seed = read_text(root / "seeds" / "representative.sql")
    for token in (
        "Alpha Library",
        "Beta Library",
        "asset_embeddings_768",
        "storage_locations",
        "memebank.jobs",
    ):
        if token not in seed:
            raise SchemaValidationError(f"representative seed lacks {token}")
    rls = read_text(root / "tests" / "rls.sql")
    for token in (
        "SET LOCAL ROLE mb_app",
        "SET LOCAL ROLE mb_worker",
        "cross_tenant_update_blocked",
        "worker_cannot_see_beta",
        "claim_jobs('rls-fixture-worker'",
    ):
        if token not in rls:
            raise SchemaValidationError(f"RLS test lacks {token}")
    verify = read_text(root / "tests" / "verify_schema.sql")
    for token in ("relrowsecurity", "relforcerowsecurity", "vector(384)", "mb_policy_owner"):
        if token not in verify:
            raise SchemaValidationError(f"structural verification lacks {token}")


def validate_root(root: Path) -> dict[str, Any]:
    root = root.resolve()
    try:
        paths = RENDER.load_order(root)
        bundle, render_report = RENDER.render(root)
    except RENDER.RenderError as error:
        raise SchemaValidationError(str(error)) from error
    validate_transactions(paths)
    validate_forbidden(bundle, root)
    validate_extensions(bundle)
    bodies = validate_tables_and_tenancy(bundle)
    validate_rls(bundle)
    validate_security_definers(bundle)
    validate_search_and_vectors(bundle, bodies)
    validate_jobs_and_lifecycle(bundle, bodies)
    validate_queries(root)
    validate_tests_and_seed(root)
    return {
        "schema_version": 1,
        "status": "valid",
        "schema_bundle_sha256": render_report["schema_bundle_sha256"],
        "schema_file_count": len(render_report["schema_files"]),
        "table_count": len(render_report["tables"]),
        "index_count": len(render_report["indexes"]),
        "policy_count": len(render_report["policies"]),
        "query_count": len(render_report["queries"]),
        "vector_dimensions": [384, 768, 1024],
        "real_database_execution_required": True,
    }


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    report = validate_root(args.root)
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SchemaValidationError as error:
        print(f"schema validation failed: {error}", file=sys.stderr)
        raise SystemExit(2) from error
