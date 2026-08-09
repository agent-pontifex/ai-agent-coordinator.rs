# HypeSiege/StreemPilot publisher hardening

This document records the security boundary and validation contract for the
one-repository-at-a-time publisher tracked by DEN-877, DEN-881, DEN-896, and
DEN-319.

## Audit result

The original live path verified the selected repository name, commit, and
working tree, but it still trusted two inputs too broadly:

1. The manifest-provided `remote` could be changed to a non-GitHub HTTPS URL.
   A matching local `origin` would then cause Git's askpass flow to send the
   short-lived repository-administration token to that host.
2. Git inherited ambient process variables and local repository configuration.
   Credential helpers, HTTP headers or proxies, URL rewrites, hooks, includes,
   replacement refs, alternates, shallow state, or command-backed Git features
   could therefore change what was inspected or where credentials were sent.

The hardened implementation treats the manifest, generated Git repositories,
GitHub API responses, and operator environment as separate trust boundaries.
It does not publish a fleet in bulk and it never reads a live token in plan or
browser-review mode.

## Enforced publication invariants

Before a token is read, the publisher now verifies all 32 manifest records,
not only the selected record. Every repository must have:

- an approved organization and canonical lowercase repository name;
- the exact `https://github.com/<owner>/<name>.git` remote;
- an explicit visibility, description, `main` default branch, full commit SHA,
  tracked-file count, and topology-compatible Gitlink count;
- a deterministic single-root history with no replacement refs, alternates,
  grafts, or shallow boundary;
- exactly one `origin`, a clean worktree, and no dangerous local Git config;
- for monorepositories, canonical same-organization `.gitmodules` URLs whose
  Gitlinks and materialized child checkouts match the manifest exactly.

The live API path additionally:

- accepts only canonical relative GitHub API paths and JSON-object bodies;
- rejects redirects rather than forwarding authorization to another origin;
- bounds request, response, and error bodies and redacts the token from errors;
- validates repository ID, owner type, canonical web/clone URLs, visibility,
  privacy, active/non-fork state, and required repository settings;
- refuses to push over a divergent remote `main`;
- skips an already exact remote commit idempotently;
- patches and re-reads the required repository settings after publication;
- verifies the final remote commit before reporting success.

For Git itself, the token-bearing process receives a minimal environment. It
has no inherited `GIT_*`, `HOME`, credential-helper, proxy, shell-startup, or
loader variables; system/global Git configuration is disabled; hooks are
redirected to an empty private directory; terminal prompting is disabled; and
the canonical HTTPS remote is passed directly to `git push`.

## Read-only browser review

`scripts/render_hypesiege_streempilot_plan.py` renders an operator-review page
without JavaScript, forms, external assets, credentials, or write controls. The
page contains the ordered 32-repository ledger and a canonical manifest digest.
Its inline style is authorized by an exact SHA-256 CSP hash rather than
`unsafe-inline`, and the HTTP test server adds CSP, no-store, no-referrer,
MIME-sniffing, opener, and resource-policy headers.

The browser test uses pinned Chrome for Testing plus the matching ChromeDriver
through the W3C WebDriver protocol. It verifies:

- DOM order, counts, digest, unique IDs, heading/table semantics, and absence of
  active or externally loaded elements;
- CSP application and security response headers;
- no unexpected network requests or severe browser-console entries;
- desktop rendering and mobile containment without page-level overflow;
- a real PNG screenshot and a machine-readable browser evidence report.

The resulting HTML, screenshot, and report are retained as a short-lived GitHub
Actions artifact. They are evidence only and cannot trigger publication.

## Local validation

The network-free validation path is:

```bash
python -m py_compile \
  scripts/reconstruct_hypesiege_streempilot_fleet.py \
  scripts/publish_hypesiege_streempilot_fleet.py \
  scripts/test_publish_hypesiege_streempilot_fleet.py \
  scripts/render_hypesiege_streempilot_plan.py \
  scripts/test_hypesiege_streempilot_plan_browser.py

python -m unittest -v scripts/test_publish_hypesiege_streempilot_fleet.py
python scripts/render_hypesiege_streempilot_plan.py \
  --output /tmp/hypesiege-streempilot-plan.html
```

The real-browser test requires `CHROME_BIN` and `CHROMEDRIVER_BIN` and is run by
`.github/workflows/repository-fleets.yml` with immutable action references and
read-only repository permissions.

## Operational non-goals

This hardening does not provision organization credentials, create all
repositories in one operation, weaken branch protection, force-push a divergent
repository, or convert browser evidence into an approval. Live execution still
requires a separately issued, short-lived, least-privilege GitHub App
installation token and exact confirmation of one `owner/name`.
