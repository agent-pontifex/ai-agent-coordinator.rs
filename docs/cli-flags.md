# Non-secret CLI flags

The coordinator keeps credentials environment-only while allowing operators and agents to express ordinary runtime settings as command-line flags through [`ORESoftware/flags-2-env`](https://github.com/ORESoftware/flags-2-env).

The root `.cli-flags.toml` is the single reviewed CLI-to-environment contract. Unknown options and invalid values fail closed.

## Usage

Provide an existing `flags2env` binary through `FLAGS2ENV_BIN`, install it on `PATH`, or place a source checkout under `vendor/flags-2-env`, `tools/flags-2-env`, or a sibling `flags-2-env` directory.

```bash
bash scripts/with-flags audit

bash scripts/with-flags \
  --config=coordinator.yaml \
  --json-logs=true \
  --repository-admin-enabled=false \
  --email-attention-enabled=false \
  --email-attention-timezone=America/New_York \
  --rust-log=ai_agent_coordinator=info,tower_http=info \
  -- cargo run --locked --release --
```

The longer repository-administration aliases from the GitHub adapter are also supported:

```bash
bash scripts/with-flags \
  --github-repository-admin-enabled=false \
  --github-repository-admin-allowed-orgs=fiducia-cloud,sonus-auris \
  --log-filter=warn \
  -- ./target/release/ai-agent-coordinator
```

The wrapper exports only values declared by `.cli-flags.toml`, rejects unknown options and parse errors, and then replaces itself with the requested command. The Rust CLI continues reading `COORDINATOR_CONFIG` and `COORDINATOR_JSON_LOGS` through Clap's environment support.

## Defaults

When no reviewed flags are supplied, the contract exports fail-closed operational defaults:

- `COORDINATOR_CONFIG=coordinator.yaml`
- `COORDINATOR_JSON_LOGS=false`
- `EMAIL_ATTENTION_ENABLED=false`
- `EMAIL_ATTENTION_TIMEZONE=America/New_York`
- `EMAIL_ATTENTION_WEEKDAYS=mon,tue,wed,thu,fri`
- `EMAIL_ATTENTION_LOCAL_HOUR=9`
- `EMAIL_ATTENTION_LOCAL_MINUTE=0`
- `GITHUB_REPOSITORY_ADMIN_ENABLED=false`
- `GITHUB_API_BASE_URL=https://api.github.com`
- `GITHUB_API_ALLOWED_HOSTS=api.github.com`
- `GITHUB_API_VERSION=2022-11-28`
- `GITHUB_API_USER_AGENT=ai-agent-coordinator`
- `RUST_LOG=ai_agent_coordinator=info,tower_http=info`

Repository administration and email scanning therefore remain disabled until they are explicitly enabled after reviewed dry runs and complete runtime configuration.

## Email-attention runtime boundary

The schedule, size bounds, notification endpoint, and other non-secret controls may be expressed through reviewed CLI flags. The mailbox source array deliberately is not a command-line flag: configure `EMAIL_ATTENTION_SOURCES_JSON` through the runtime environment so mailbox aliases, internal connector endpoints, and source token-variable names are not copied into shell history or process arguments.

Actual connector and notification tokens remain environment- or secret-manager-only. See [`email-attention-agent.md`](email-attention-agent.md) for the connector request/response contract, safe manual test, deduplication semantics, and production activation checklist.

## Credential boundary

These values are deliberately excluded from the command-line contract:

- `COORDINATOR_API_TOKEN`
- `GITHUB_WEBHOOK_SECRET`
- `GITHUB_REPOSITORY_ADMIN_TOKEN`
- `LINEAR_API_TOKEN`
- `EMAIL_ATTENTION_NOTIFICATION_TOKEN`
- mailbox connector bearer tokens named by `EMAIL_ATTENTION_SOURCES_JSON`
- `MISTRAL_API_KEY`
- `OPENROUTER_API_KEY`
- `OPENAI_API_KEY`

Supply them through the runtime secret manager or environment injection. Do not place them in shell history, process arguments, repository files, workflow arguments, logs, or Linear issues.

The wrapper explicitly rejects credential-like options before invoking `flags2env`, and CI proves rejected values are not echoed. CI also checks out a reviewed `flags-2-env` commit, builds it from source, audits the configuration, validates explicit mappings and defaults, exercises both alias families, proves unknown options fail closed, and proves the credential variables remain unset in the launched process.
