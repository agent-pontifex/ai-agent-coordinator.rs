# GitHub repository-administration security contract

**Tracking:** DEN-602 and DEN-319
**Evidence date:** August 1, 2026 (America/Lima)

The coordinator exposes `POST /v1/github/repositories` for narrowly governed
organization-repository creation. This is a consequential external mutation, so
its policy boundary is stricter than an ordinary authenticated API call.

## Authority and request gates

Repository administration remains disabled unless
`GITHUB_REPOSITORY_ADMIN_ENABLED=true`. Live requests additionally require:

- a short-lived GitHub App installation token supplied only through
  `GITHUB_REPOSITORY_ADMIN_TOKEN`;
- an exact organization match in `GITHUB_REPOSITORY_ADMIN_ALLOWED_ORGS`;
- an exact `confirm_repository` value matching `organization/name`;
- a valid GitHub organization and repository-name shape;
- a bounded, control-character-free description;
- an explicit visibility and initialization mode.

`dry_run` defaults to `true` and performs no upstream GitHub request.

## Upstream transport boundary

The administration client has a bounded timeout and does not follow redirects.
This prevents a GitHub or test endpoint from redirecting the bearer credential
to another origin.

`GITHUB_API_BASE_URL` accepts HTTPS only when its exact hostname appears in
`GITHUB_API_ALLOWED_HOSTS` (default: `api.github.com`). A GitHub Enterprise host
must therefore be explicitly reviewed and allowlisted before it can receive the
administration credential. Plain HTTP is accepted only for an exact loopback
host used by local tests:

- `localhost`;
- any literal IPv4 loopback address;
- any literal IPv6 loopback address.

Prefix lookalikes such as `localhost.attacker.example`, unlisted HTTPS hosts,
user-info authorities, queries, fragments, non-loopback HTTP hosts, and non-HTTP
schemes are rejected before the client is constructed.

## Idempotency and response validation

The coordinator first looks up the exact target repository. An existing
repository is returned only when its `full_name` and visibility match the
validated request.

GitHub may report a conflict or validation error if another actor creates the
same repository between lookup and create. For those two statuses, the
coordinator performs one bounded follow-up lookup. It treats the operation as an
idempotent success only when the exact repository identity and visibility now
match; otherwise the original GitHub error remains authoritative.

A successful create response is subject to the same identity and visibility
validation. Repository identifiers and default-branch values are bounded, and
the returned HTML link is derived from the trusted API base instead of accepting
an upstream-provided link. The coordinator never substitutes an unexpected
repository returned by an upstream service.

Upstream error bodies are bounded, control characters are normalized, and the
active bearer credential is redacted before an error can reach an API response.

## Browser and Actions evidence

`.github/workflows/github-repository-admin-browser.yml` starts:

- PostgreSQL 17 from a digest-pinned image;
- the real debug coordinator binary built from the pull-request head;
- deterministic loopback GitHub API and redirect-sink doubles;
- headless Chromium through a pinned Playwright Python package.

Chromium exercises the actual authenticated HTTP route and verifies:

1. health, authorization-error, dry-run, existing, and created responses carry
   `no-store` and request IDs;
2. dry runs, invalid confirmation, and unlisted organizations make no GitHub
   request;
3. live creation sends the expected bounded GitHub request and merge settings;
4. a create race resolves through one exact follow-up lookup;
5. mismatched repository identity, visibility, and oversized responses fail
   closed, while untrusted upstream links are never propagated;
6. upstream error messages cannot echo the active bearer credential;
7. redirects are not followed and the redirect sink receives no authorization
   header.

The workflow uploads the Playwright trace, browser screenshot, tested-revision
manifest, Playwright version, and coordinator log for 14 days. It never uses a
live GitHub, Linear, provider, or customer credential.

## Operational limits

This contract governs the coordinator API. It does not itself authorize an
organization, install a GitHub App, provision a token, create an absent GitHub
organization, or prove that production Argo applications are healthy. Those
operational gates remain tracked under DEN-319 and must be verified separately
before live repository creation is enabled.
