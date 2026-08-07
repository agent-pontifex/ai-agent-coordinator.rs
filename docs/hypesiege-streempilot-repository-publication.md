# HypeSiege and StreemPilot repository publication

**Tracking:** DEN-877, DEN-881, DEN-896, and DEN-319  
**Evidence date:** July 31, 2026 (America/Lima)

This document records the publication boundary for the 15-repository HypeSiege family and the 17-repository StreemPilot family. It does not claim that a target repository exists merely because source files, a local Git history, a manifest row, a workflow, or a Linear issue exists.

## Canonical source and semantic reconciliation

Two parallel pull requests initially represented incompatible fleet ledgers:

- one carried the complete repository-content generator and attempted live all-at-once publication, but its workflow had no repository-administration credentials and verified zero remote heads;
- the other carried a reviewable one-repository publisher and manifest, but its 856-file histories did not include the real monorepo gitlinks required by the release model.

The canonical implementation combines the compatible intent instead of selecting either side mechanically:

1. preserve the complete source-generator payload and verify its SHA-256 identity;
2. discard the all-at-once live workflow and organization-secret wiring;
3. reconstruct the 32 source repositories in a caller-owned directory;
4. replace timestamp-dependent histories with deterministic commits using fixed author, committer, and date metadata;
4. replace timestamp-dependent commits with fixed author, committer, and date metadata;
5. seal all 30 child repositories before the two monorepos;
6. materialize clean local child checkouts while committing only exact mode-`160000` gitlinks and canonical `.gitmodules` URLs;
7. compare the regenerated schema-v2 manifest byte-for-byte with the checked-in ledger; and
8. retain the fail-closed, one-repository-at-a-time publisher with exact confirmation and remote-head verification.

The checked-in ledger therefore represents **32 deterministic independent Git histories, 888 tracked files, and 30 immutable gitlinks**. Running the reconstruction twice produces identical commit SHAs for all repositories.

The superseded `deploy/k8s/bootstrap` all-at-once publisher and its bundled generator were removed during reconciliation. That job accepted a broad repository-administration credential and attempted the entire fleet in one execution, contradicting the canonical one-repository confirmation, preflight, and remote-head verification boundary. Each generated repository is sealed from its complete staged tree as a deterministic parentless commit, so preexisting source-history depth cannot silently alter the published content or identity.
The superseded `deploy/k8s/bootstrap` all-at-once publisher and its bundled generator were removed during reconciliation. That job accepted a broad repository-administration credential and attempted the entire fleet in one execution, contradicting the canonical one-repository confirmation, preflight, and remote-head verification boundary.

## Reconstruct and validate locally

The gzip/base64 parts under `repository-fleets/hypesiege-streempilot/` contain the complete reviewed source generator. The reconstruction wrapper checks the decoded generator against SHA-256 `a57b00961ee57ae09bf3bb2e2d09afbdd1ddbbbde832b027802f82a1fc5dfa84` before executing it.
## Reconstruct and validate locally

The gzip/base64 parts under `repository-fleets/hypesiege-streempilot/` contain the complete reviewed source generator. The reconstruction wrapper checks the decoded generator against SHA-256 `50629a57beca1ac85928cfae8fbebbca4f62a6455a7013016f92b1203dcbbd1f` before executing it.
This document records the publication boundary for the 15-repository HypeSiege
family and the 17-repository StreemPilot family. It does not claim that a target
repository exists merely because source files, a local Git history, a manifest
row, a workflow, or a Linear issue exists.

## Canonical source and semantic reconciliation

Parallel implementations represented different parts of the intended system:

- one carried the complete repository-content generator and an unsafe
  all-at-once publication attempt;
- another carried the deterministic ledger, one-repository publisher, and
  child-before-monorepo verification model;
- current `main` contained later unrelated work that neither old branch could be
  allowed to overwrite.

The canonical repair combines the compatible intent instead of choosing a branch
wholesale:

1. preserve the complete six-part source-generator payload;
2. verify its decoded SHA-256 identity as
   `a57b00961ee57ae09bf3bb2e2d09afbdd1ddbbbde832b027802f82a1fc5dfa84`;
3. reconstruct all repositories in a caller-owned directory and relocate the
   generator's tree, archive, and checksum outputs together;
4. preserve each repository's complete final indexed tree;
5. seal all child repositories before the two monorepositories;
6. materialize clean local child checkouts while committing exact mode-`160000`
   gitlinks and canonical `.gitmodules` URLs;
7. compare the regenerated schema-v2 manifest byte-for-byte with the checked-in
   ledger; and
8. retain the fail-closed, one-repository-at-a-time publisher with exact
   confirmation and remote-head verification.

The reviewed generator may use setup commits while assembling a monorepository.
Reconstruction preserves the final indexed tree and latest reviewed commit
message, then creates a parentless fixed-author/fixed-date root commit. This
removes timestamp-dependent setup history without discarding generated files or
child gitlinks.

The checked-in ledger represents **32 deterministic independent Git histories,
888 tracked files, and 30 immutable gitlinks**. Running reconstruction twice
must produce identical commit SHAs for every repository.

The superseded `deploy/k8s/bootstrap` all-at-once publisher and its bundled
payload are not part of the canonical boundary. That path accepted a broad
repository-administration credential and attempted the entire fleet in one
execution, contradicting per-repository confirmation, preflight, publication
order, and remote-head verification.

## Reconstruct and validate locally

The gzip/base64 parts under `repository-fleets/hypesiege-streempilot/` contain
the complete reviewed generator. The wrapper verifies the decoded source before
executing it:

```bash
python scripts/reconstruct_hypesiege_streempilot_fleet.py \
  --output-root /secure/path/to/hypesiege-streempilot-fleet \
  --manifest-out /tmp/reconstructed-manifest.json

cmp /tmp/reconstructed-manifest.json \
  repository-fleets/hypesiege-streempilot.json
```

The reconstruction fails closed on payload drift, malformed generation output, an empty or malformed source history, branch or origin drift, dirty repositories, Git corruption, tracked-file drift, missing or wrong gitlinks, mismatched submodule checkouts, or fleet totals other than 32 repositories, 888 tracked files, and 30 gitlinks.
The reconstruction fails closed on payload drift, malformed generation output, a non-single-commit source history, branch or origin drift, dirty repositories, Git corruption, tracked-file drift, missing or wrong gitlinks, mismatched submodule checkouts, or fleet totals other than 32 repositories, 888 tracked files, and 30 gitlinks.

Transport archives are recovery material only. An archive checksum is not a substitute for the per-repository commit ledger, a remote metadata read, or successful push verification.
Reconstruction fails closed on payload drift, malformed output, missing source
commits, branch or origin drift, dirty worktrees, Git corruption, tracked-file
drift, missing or incorrect gitlinks, mismatched submodule checkouts, manifest
drift, or totals other than 32 repositories, 888 files, and 30 gitlinks.

Transport archives and their checksums are recovery material only. They are not
substitutes for the per-repository commit ledger, authenticated remote metadata,
or post-push head verification.

## Current remote boundary

At the evidence date:

- the connected GitHub App installation is present for the canonical `StreemPilot` organization, but no canonical fleet repository has been verified through that installation;
- the connected GitHub App is not installed for `hypesiege`;
- the existing-repository connector can manage files, branches, pull requests, issues, and checks, but it does not expose organization repository creation;
- the attempted live workflow had no HypeSiege, StreemPilot, or shared repository-administration secret, exited before every create/push operation, and verified `0/32` public remote heads; and
- the protected coordinator repository-bootstrap path still needs a short-lived, least-privilege GitHub App installation token and exact organization allowlist under DEN-319.

Do not redirect HypeSiege repositories into another organization, rename the sealed repositories, create README-only substitutes, or treat an empty organization installation as publication evidence.
- the connected GitHub App installation is present for the canonical
  `StreemPilot` organization, but no canonical fleet repository has been proven
  through that installation;
- the connected GitHub App is not installed for `hypesiege`;
- the existing-repository connector can manage selected repositories, branches,
  files, pull requests, issues, and checks, but does not expose organization
  repository creation; and
- the protected repository-bootstrap path still requires a short-lived,
  least-privilege GitHub App installation token and an exact organization
  allowlist under DEN-319.

Do not redirect HypeSiege repositories into another organization, rename sealed
repositories, create README-only substitutes, or treat an empty organization
installation as publication evidence.

## Safe one-repository publisher

Planning is network-free and requires no credential:

```bash
python scripts/publish_hypesiege_streempilot_fleet.py \
  --repository hypesiege/hypesiege-api-server.rs
```

Live execution requires all of the following:

1. the reconstructed source root containing the exact independent Git history;
2. the exact manifest repository name;
3. `--execute`;
4. `--confirm-repository` exactly equal to that owner/name;
5. a short-lived GitHub App installation token in `GITHUB_REPOSITORY_ADMIN_TOKEN`, injected by an approved secret manager and scoped only to the required organization/repository operations; and
2. the exact manifest owner/name;
3. `--execute`;
4. `--confirm-repository` exactly equal to that owner/name;
5. a short-lived GitHub App installation token in
   `GITHUB_REPOSITORY_ADMIN_TOKEN`, injected by an approved secret manager and
   scoped only to the required organization and operation; and
6. successful local preflight before any GitHub mutation.

```bash
python scripts/publish_hypesiege_streempilot_fleet.py \
  --source-root /secure/path/to/hypesiege-streempilot-fleet \
  --repository hypesiege/hypesiege-api-server.rs \
  --execute \
  --confirm-repository hypesiege/hypesiege-api-server.rs
```

The publisher refuses an unknown organization or repository, malformed ledger, wrong branch, commit or origin drift, dirty tree, tracked-file or gitlink drift, `.gitmodules` mismatch, unmaterialized or changed child checkout, Git corruption, missing confirmation, missing credential, visibility mismatch, bounded GitHub API error, non-fast-forward push, or post-push remote-head mismatch.

## Publication order and monorepo guard

Publish each organization's standalone release units first. The publisher will not publish `hypesiege-monorepo` or `streempilot-monorepo` until every child repository's remote `main` resolves to the exact commit pinned by the ledger. The monorepos must be last.
The publisher refuses unknown owners or repositories, malformed ledgers, wrong
branches, commit/origin drift, dirty trees, file or gitlink drift,
`.gitmodules` mismatch, unmaterialized or changed child checkouts, Git
corruption, missing confirmation, missing credentials, visibility mismatch,
bounded GitHub API errors, non-fast-forward pushes, and post-push remote-head
mismatches.

## Publication order and monorepository guard

Publish each organization's standalone release units first. The publisher must
not publish `hypesiege-monorepo` or `streempilot-monorepo` until every child
repository's remote `main` resolves to the exact commit pinned by the ledger.
The monorepositories are published last.

For every successful publication retain:

- GitHub repository ID, canonical URL, visibility, and default branch;
- exact pushed `main` commit from the ledger;
- an authenticated remote read proving that commit is reachable;
- repository ruleset and security-setting evidence;
- bootstrap CI/check-suite result;
- Linear project mapping and one reversible issue/PR synchronization proof; and
- final monorepo gitlink verification after every child is reachable.

No repository or foundation ticket is complete until those remote reads and checks exist.
- final monorepository gitlink verification after every child is reachable.

No repository or foundation ticket is complete until those remote reads and
checks exist.
