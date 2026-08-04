use serde_json::Value;

const BOUNDARY_JSON: &str = include_str!("../docs/platform-boundary.json");
const BOUNDARY_DOC: &str = include_str!("../docs/platform-boundary.md");

fn boundary() -> Value {
    serde_json::from_str(BOUNDARY_JSON).expect("platform boundary must be valid JSON")
}

fn array_contains(document: &Value, pointer: &str, expected: &str) -> bool {
    document
        .pointer(pointer)
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(expected)))
}

fn string_at<'a>(document: &'a Value, pointer: &str) -> &'a str {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string at {pointer}"))
}

#[test]
fn identifies_the_canonical_issue_and_control_plane_role() {
    let document = boundary();

    assert_eq!(document["contract_version"], 1);
    assert_eq!(string_at(&document, "/linear_issue"), "DEN-1873");
    assert_eq!(string_at(&document, "/service"), "ai-agent-coordinator");
    assert_eq!(
        string_at(&document, "/role"),
        "agent-orchestration-control-plane"
    );
}

#[test]
fn separates_agent_policy_from_protocol_ci_cluster_identity_and_raft_authority() {
    let document = boundary();

    assert!(array_contains(
        &document,
        "/owns",
        "model routing and budget enforcement"
    ));
    assert!(array_contains(
        &document,
        "/does_not_own",
        "canonical public Agent Pontifex protocol schema"
    ));
    assert!(array_contains(
        &document,
        "/does_not_own",
        "GitHub Actions workflow semantics"
    ));
    assert!(array_contains(
        &document,
        "/does_not_own",
        "Kubernetes cluster registration or GitOps tenancy"
    ));
    assert!(array_contains(
        &document,
        "/does_not_own",
        "portfolio-wide human identity"
    ));
    assert!(array_contains(
        &document,
        "/does_not_own",
        "Fiducia Raft coordination state"
    ));
}

#[test]
fn consumes_the_canonical_agent_sdk_protocol_without_redefining_it() {
    let document = boundary();

    assert_eq!(
        string_at(&document, "/integrations/agent-sdk/authority"),
        "agent-pontifex/agent-sdk.rs"
    );
    assert!(array_contains(
        &document,
        "/integrations/agent-sdk/required_behavior",
        "consume canonical protocol types rather than redefining them"
    ));
    assert!(array_contains(
        &document,
        "/integrations/agent-sdk/required_behavior",
        "fail closed on an incompatible service role, protocol, or supported version range"
    ));
    assert_eq!(
        string_at(&document, "/task_envelope/public_protocol_authority"),
        "agent-pontifex/agent-sdk.rs"
    );
    assert!(string_at(&document, "/task_envelope/scope").contains("private coordinator"));
}

#[test]
fn requires_installation_scoped_github_credentials_and_blocks_ambient_secrets() {
    let document = boundary();

    assert_eq!(
        string_at(&document, "/integrations/github/authentication"),
        "short-lived installation-scoped GitHub App token"
    );
    assert!(array_contains(
        &document,
        "/integrations/github/forbidden",
        "broad personal access token"
    ));
    assert!(array_contains(
        &document,
        "/integrations/github/forbidden",
        "ambient host credential"
    ));
    assert!(array_contains(
        &document,
        "/task_envelope/must_not_include",
        "GitHub App private key"
    ));
}

#[test]
fn dispatches_only_immutable_reviewed_work_to_the_ci_platform() {
    let document = boundary();

    for field in [
        "repository",
        "immutable_commit_sha",
        "reviewed_workflow_path_or_profile",
        "idempotency_key",
        "traceparent",
    ] {
        assert!(array_contains(
            &document,
            "/integrations/gha-indie-worker/request_fields",
            field
        ));
    }

    for forbidden in [
        "caller-selected shell",
        "mutable branch or tag as execution authority",
        "caller-selected Kubernetes manifest or runner image",
    ] {
        assert!(array_contains(
            &document,
            "/integrations/gha-indie-worker/forbidden",
            forbidden
        ));
    }
}

#[test]
fn gates_irreversible_side_effects_with_idempotency_approval_and_fencing() {
    let document = boundary();

    for field in [
        "immutable_commit_sha",
        "approval_id",
        "fencing_token",
        "causation_id",
    ] {
        assert!(array_contains(
            &document,
            "/task_envelope/side_effect_fields",
            field
        ));
    }

    assert!(array_contains(
        &document,
        "/integrations/fiducia/required_for",
        "lease-protected merge, deploy, release, or credential rotation"
    ));
    assert!(string_at(&document, "/deployment/horizontal_scaling_gate").contains("Fiducia"));
    assert_eq!(
        string_at(&document, "/failure_semantics/ambiguous_external_result"),
        "do not retry blindly; reconcile provider or GitHub state first"
    );
}

#[test]
fn keeps_credentials_secret_payloads_and_high_cardinality_ids_out_of_telemetry() {
    let document = boundary();

    for field in [
        "access_token",
        "authorization_header",
        "GitHub App private key",
        "personal access token",
        "hidden chain-of-thought",
    ] {
        assert!(array_contains(
            &document,
            "/observability/forbidden_fields",
            field
        ));
    }

    for label in [
        "actor_id",
        "tenant_id",
        "repository",
        "request_id",
        "job_id",
        "commit_sha",
    ] {
        assert!(array_contains(
            &document,
            "/observability/high_cardinality_metric_labels_forbidden",
            label
        ));
    }
}

#[test]
fn human_readable_document_matches_the_machine_boundary() {
    for statement in [
        "agent-orchestration control plane",
        "canonical public protocol authority",
        "namespaced private extension",
        "short-lived installation tokens",
        "gha-indie-worker",
        "Fiducia fencing is required",
        "hidden chain-of-thought",
        "immutable digest",
    ] {
        assert!(
            BOUNDARY_DOC.contains(statement),
            "human-readable boundary must mention {statement}"
        );
    }
}
