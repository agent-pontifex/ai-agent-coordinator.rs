# Meta Agents repository bootstrap

This overlay is an explicit, one-shot operational tool for creating the canonical public repository `meta-agents-demo/meta-agent-control-plane.rs`.

It is intentionally excluded from `deploy/k8s/kustomization.yaml`. The long-running coordinator deployment must never carry repository-administration work or require the optional `ai-agent-coordinator-admin` Secret.

## Preconditions

- The `ai-agent-coordinator` namespace exists.
- Secret `ai-agent-coordinator-admin` contains a nonempty, whitespace-free `GITHUB_REPOSITORY_ADMIN_TOKEN`.
- The token authenticates exactly as `ORESoftware`.
- `ORESoftware` has active `admin` membership in `meta-agents-demo`.

## Run

```bash
kubectl apply -k deploy/k8s/bootstrap/meta-agent
kubectl -n ai-agent-coordinator wait \
  --for=condition=complete \
  --timeout=5m \
  job/meta-agent-control-plane-repository-bootstrap-20260731
kubectl -n ai-agent-coordinator logs \
  job/meta-agent-control-plane-repository-bootstrap-20260731
```

The Job is idempotent: an existing public repository with the exact canonical full name is accepted. Unexpected API responses, identities, organization membership, visibility, or repository metadata fail closed.

The Job has no retries, a five-minute active deadline, a one-day TTL, no service-account token, no host namespace access, one unprivileged container, and bounded in-memory temporary storage. It must not be placed in a continuously reconciled Argo CD Application. After direct GitHub verification, remove the optional admin Secret if no other approved operation requires it.
The Job has `backoffLimit: 0`, a five-minute active deadline, and a one-day TTL. It must not be placed in a continuously reconciled Argo CD Application. After direct GitHub verification, remove the optional admin Secret if no other approved operation requires it.

## Verify

Completion requires independent GitHub reads proving:

- repository full name `meta-agents-demo/meta-agent-control-plane.rs`;
- public visibility;
- default branch `main` after source publication;
- exact reviewed `main` and implementation branch SHAs;
- connected-app metadata, branch, pull-request, checks, status, and issue access.

Repository creation alone does not complete DEN-1057. The reviewed Rust/Leptos history must still be published through a normal target pull request with green CI and exact-head merge evidence.

## Contract

Run locally:

```bash
ruby -c scripts/validate-meta-agent-bootstrap.rb
ruby scripts/validate-meta-agent-bootstrap.rb
ruby scripts/validate-meta-agent-bootstrap.rb --self-test
```

The contract rejects accidental steady-state inclusion, extra overlay resources, retries, missing deadlines or cleanup, mutable images, service-account tokens, host namespaces, extra containers, privileged containers, plaintext credentials, target drift, weak identity/membership checks, and unbounded temporary storage.
The contract rejects accidental steady-state inclusion, extra resources, retries, mutable images, service-account tokens, privileged containers, plaintext credentials, target drift, weak identity/membership checks, and unbounded temporary storage.
