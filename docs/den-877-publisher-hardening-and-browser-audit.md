# DEN-877 publisher hardening and browser audit

**Tracking:** DEN-877, DEN-881, DEN-896, and DEN-319
**Scope:** the deterministic 15-repository HypeSiege and 17-repository StreemPilot fleet

This change hardens the existing one-repository-at-a-time publisher without expanding its authority. It does not claim that any repository exists remotely. A checked-in manifest, a generated plan, a green unit test, or an HTML report is not publication evidence; authenticated GitHub metadata and exact remote `main` verification remain mandatory.

## Fail-closed publisher boundary

Plan mode remains credential-free and network-free:

```bash
python scripts/publish_hypesiege_streempilot_fleet.py \
  --repository hypesiege/hypesiege-api-server.rs \
  --report-out /tmp/hypesiege-api-plan.json
```

Live execution still requires one exact repository name, one exact confirmation, an independently reconstructed source root, and a short-lived least-privilege GitHub App installation token supplied only through `GITHUB_REPOSITORY_ADMIN_TOKEN`:

```bash
python scripts/publish_hypesiege_streempilot_fleet.py \
  --source-root /secure/path/to/hypesiege-streempilot-fleet \
  --repository hypesiege/hypesiege-api-server.rs \
  --execute \
  --confirm-repository hypesiege/hypesiege-api-server.rs \
  --report-out /secure/evidence/hypesiege-api-publication.json
```

The hardening layer adds these invariants:

- the manifest must be a bounded regular non-symlink UTF-8 file with exact top-level and repository keys;
- duplicate JSON keys, unknown fields, order drift, aggregate drift, malformed identities, private visibility, and noncanonical or credential-bearing remotes are rejected before planning;
- API calls are restricted to known methods and relative allowlisted paths, redirects are denied, request and response bodies are bounded, response media types and JSON are validated, duplicate response keys are rejected, and diagnostics redact credentials;
- existing repositories must return the exact positive repository ID, owner, name, public visibility, and non-fork/non-archived/non-disabled state;
- a divergent `main` or any other preexisting branch is refused instead of overwritten;
- local preflight requires exact `main`, exact commit and origin, one deterministic root commit, a clean tree, exact file and gitlink counts, materialized clean child checkouts, `git diff --check`, and `git fsck --full --no-dangling`;
- Git pushes disable system and global configuration, credential helpers, hooks, redirects, SSH fallback, and terminal prompts; pushes remain non-force and target only `HEAD:refs/heads/main`;
- an exact remote is reported idempotently as `already_verified`, while a new verified push is `published`; both states reapply and verify the reviewed repository settings; and
- plan and execution evidence can be written atomically to bounded JSON files without serializing the credential.

Monorepositories remain last. The publisher verifies every child repository's remote `main` against the checked-in ledger before permitting the corresponding monorepo publication.

## Static review artifact

Generate a deterministic credential-free report:

```bash
python scripts/render_hypesiege_streempilot_audit_report.py \
  --manifest repository-fleets/hypesiege-streempilot.json \
  --output /tmp/hypesiege-streempilot-audit-report.html \
  --metadata-out /tmp/hypesiege-streempilot-audit-report.json
```

The report contains all 32 repositories, exact commits, canonical remotes, file counts, gitlink counts, descriptions, aggregate totals, and the manifest SHA-256. Manifest text is escaped. The document has no scripts, images, forms, or external assets and carries a restrictive meta Content Security Policy. Its visible boundary explicitly says that locally sealed histories do not prove remote publication.

## Browser automation and GitHub Actions

The `Repository fleet manifests` workflow now:

1. validates itself with the repository's pinned `actionlint` image;
2. runs the existing full reconstruction suite plus new negative manifest, API, remote-state, push-isolation, and atomic-output tests;
3. renders and compares a credential-free plan for every one of the 32 repositories;
4. generates the HTML report and machine-readable metadata;
5. installs exact Python Playwright dependencies and a matching Chromium runtime;
6. exercises the report at desktop and mobile viewport sizes;
7. verifies 32 rows, two monorepos, exact totals, canonical links, responsive table scrolling, no page-level mobile overflow, no scripts or images, no browser console errors, no outgoing requests, and no credential markers; and
8. uploads plans, report metadata, the report, and desktop/mobile screenshots as seven-day GitHub Actions artifacts.

A green browser job proves only that the checked-in ledger is reviewable and that its rendered evidence is complete, static, responsive, and credential-free. Remote publication remains a separate protected operation under DEN-319.
