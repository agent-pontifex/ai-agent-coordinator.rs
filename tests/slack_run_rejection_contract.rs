use ai_agent_coordinator::slack_run::SlackAgentRunPayload;
use serde_json::{json, Value};

fn valid_payload() -> Value {
    json!({
        "schema_version": 1,
        "run_id": "ores-00112233445566778899aabb",
        "bridge_workflow_id": "workflow-123",
        "provider": "chatgpt",
        "action": "implement",
        "prompt": "Implement DEN-1231 with tests.",
        "origin": {
            "workspace_id": "T012345",
            "channel_id": "C012345",
            "requester_user_id": "U012345"
        },
        "context": {
            "trust": "untrusted_channel_context",
            "selection": "latest_non_bot_channel_messages",
            "messages": [
                {"user_id": "U1", "ts": "1000.000001", "text": "first"},
                {"user_id": "U2", "ts": "1001.000001", "text": "second"}
            ]
        },
        "routing": {
            "repository": "ORESoftware/ai-agent-coordinator.rs",
            "linear_team_id": "team-uuid",
            "linear_project_id": "project-uuid",
            "linear_run_project_id": "run-project-uuid",
            "linear_issue": "DEN-1231",
            "write_policy": "draft_pull_request"
        },
        "broadcast_targets": [
            "slack_run_thread",
            "ai_agent_coordinator_job",
            "ai_agent_bridge_workflow",
            "linear_run_queue",
            "github_branch_pr_checks"
        ]
    })
}

fn rejection(value: &Value) -> String {
    SlackAgentRunPayload::from_value(value).expect_err("payload unexpectedly passed validation")
}

#[test]
fn rejects_context_messages_that_are_not_chronological() {
    let mut value = valid_payload();
    value["context"]["messages"] = json!([
        {"user_id": "U2", "ts": "1001.000001", "text": "second"},
        {"user_id": "U1", "ts": "1000.000001", "text": "first"}
    ]);

    assert_eq!(
        rejection(&value),
        "slack_agent_run context must be chronological"
    );
}

#[test]
fn rejects_context_that_exceeds_the_aggregate_byte_budget() {
    let mut value = valid_payload();
    value["context"]["messages"] = Value::Array(
        (0..9)
            .map(|index| {
                json!({
                    "user_id": "U1",
                    "ts": format!("{}.000001", 1000 + index),
                    "text": "x".repeat(3_900)
                })
            })
            .collect(),
    );

    assert_eq!(
        rejection(&value),
        "slack_agent_run context exceeds total byte limit"
    );
}

#[test]
fn rejects_duplicate_broadcast_targets_even_when_the_set_is_complete() {
    let mut value = valid_payload();
    value["broadcast_targets"] = json!([
        "slack_run_thread",
        "ai_agent_coordinator_job",
        "ai_agent_bridge_workflow",
        "linear_run_queue",
        "github_branch_pr_checks",
        "slack_run_thread"
    ]);

    assert_eq!(
        rejection(&value),
        "slack_agent_run broadcast_targets must be the canonical set"
    );
}

#[test]
fn rejects_repository_urls_git_suffixes_and_path_escape_shapes() {
    for repository in [
        "https://github.com/ORESoftware/ai-agent-coordinator.rs",
        "ORESoftware/ai-agent-coordinator.rs.git",
        "ORESoftware/team/ai-agent-coordinator.rs",
        "/ai-agent-coordinator.rs",
        "ORESoftware/../ai-agent-coordinator.rs",
        "ORESoftware/ai-agent@coordinator.rs",
        "ORESoftware/ai agent coordinator",
        "ORESoftware//ai-agent-coordinator.rs",
    ] {
        let mut value = valid_payload();
        value["routing"]["repository"] = json!(repository);
        assert_eq!(
            rejection(&value),
            "slack_agent_run repository is invalid",
            "repository {repository:?} must fail closed"
        );
    }
}

#[test]
fn rejects_noncanonical_linear_issue_identifiers() {
    for issue in [
        "den-1231",
        "D-1231",
        "DEN-01231",
        "DEN-",
        "DEN-12A",
        "DEN--1231",
        " DEN-1231",
        "DEN-1231 ",
        "DEN_1231",
    ] {
        let mut value = valid_payload();
        value["routing"]["linear_issue"] = json!(issue);
        assert_eq!(
            rejection(&value),
            "slack_agent_run Linear issue identifier is invalid",
            "issue {issue:?} must fail closed"
        );
    }
}

#[test]
fn rejects_noncanonical_run_ids() {
    for run_id in [
        "ores-00112233445566778899AABB",
        "ores-00112233",
        "ores-00112233445566778899aabz",
        "run-00112233445566778899aabb",
        "ores-00112233445566778899aabb00",
        " ores-00112233445566778899aabb",
        "ores-00112233445566778899aabb ",
        "",
    ] {
        let mut value = valid_payload();
        value["run_id"] = json!(run_id);
        assert_eq!(
            rejection(&value),
            "slack_agent_run run_id is invalid",
            "run id {run_id:?} must fail closed"
        );
    }
}

#[test]
fn rejects_invalid_slack_timestamps_and_nul_bearing_text() {
    for timestamp in [
        "1000",
        "1000.",
        ".000001",
        "1000.000001.extra",
        "-1.000001",
        "1000.000 001",
        " 1000.000001",
        "1000.000001 ",
    ] {
        let mut value = valid_payload();
        value["context"]["messages"][0]["ts"] = json!(timestamp);
        assert_eq!(
            rejection(&value),
            "slack_agent_run context timestamp is invalid",
            "timestamp {timestamp:?} must fail closed"
        );
    }

    let mut prompt = valid_payload();
    prompt["prompt"] = json!("implement\u{0000}hidden");
    assert_eq!(rejection(&prompt), "slack_agent_run prompt is invalid");

    let mut message = valid_payload();
    message["context"]["messages"][0]["text"] = json!("visible\u{0000}hidden");
    assert_eq!(
        rejection(&message),
        "slack_agent_run context message is invalid"
    );
}

#[test]
fn rejects_unknown_fields_at_every_security_boundary() {
    let cases = [
        ("top-level", vec!["unexpected"]),
        ("origin", vec!["origin", "unexpected"]),
        ("context", vec!["context", "unexpected"]),
        (
            "context message",
            vec!["context", "messages", "0", "unexpected"],
        ),
        ("routing", vec!["routing", "unexpected"]),
    ];

    for (label, path) in cases {
        let mut value = valid_payload();
        let mut cursor = &mut value;
        for component in &path[..path.len() - 1] {
            cursor = if let Ok(index) = component.parse::<usize>() {
                &mut cursor[index]
            } else {
                &mut cursor[*component]
            };
        }
        cursor[path[path.len() - 1]] = json!("must-not-be-accepted");
        assert_eq!(
            rejection(&value),
            "slack_agent_run payload does not match schema v1",
            "{label} unknown field must fail closed"
        );
    }
}
