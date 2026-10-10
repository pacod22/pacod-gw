# Threat model

This is the threat model of concept section 16, checked against the code as it is built.
Each row says what protects against the threat, where that lives, and what risk remains.
Reports of weaknesses go to the contact in [SECURITY.md](../SECURITY.md).

## What is worth protecting

- **Provider keys.** Every provider account of the installation. Whoever has one spends
  money in its name and can reach the files and batches stored with it.
- **Prompts and answers.** They pass through every gateway, and audit mode stores them.
- **Gateway keys and console sessions.** Whoever holds one acts as that workspace or user.
- **Tenant separation.** What one workspace sends, stores, spends and reads must stay its
  own.
- **The accounting.** Usage, cost and limits must be right, so that limits hold and bills
  can be trusted.

## Who and what is trusted

| Party | Trusted for |
|---|---|
| Callers with a gateway key | Nothing beyond their key's workspace, modes, models and limits. |
| Console users and admin tokens | What their roles allow (concept 5.4), nothing more. |
| Operators of the host or cluster | Everything. They hold the key-encryption key and the databases. |
| Providers | Their own answers. Their error messages are passed on. |
| The analytics service | Its own database. It parses pages from outside (price sources), so it holds no provider keys, and its token opens only the job endpoints of the core. |
| PostgreSQL and ClickHouse | Storage. They hold only encrypted provider keys and keyed hashes of gateway keys. |

## Threats and controls

| Threat | Control as built | Remaining risk |
|---|---|---|
| Provider keys stolen from storage | Envelope encryption: an AES-256-GCM data key per credential, wrapped by the key-encryption key, with the credential id bound in (`crypto/envelope.rs`). The key-encryption key comes from the environment or a file (`PGW_KEK`, `PGW_KEK_FILE`, `PGW_SECRETS_DIR`), never from the database, and backups leave it out (`deploy/scripts/backup.sh`). Rotation: `PGW_KEK_OLD` and `rewrap-secrets`, which refuses while an instance has not confirmed the new key. | With compose the key-encryption key sits in `deploy/.env` on the same host as the database. Keep that file and the backups apart; with Helm use a Secret or a mounted file. |
| Provider keys leaked through the API or logs | Secrets are write-only; reads show a fingerprint (the last four characters and a hash prefix). The audit log stores hashes of documents, not documents. Configuration exports leave secrets out. The caller's gateway key is never forwarded. `make smoke-live` scans every log for the provider keys it used. | The provider's own error text is passed on, up to 300 characters, and stored in the usage event's `error_message`. A provider that echoed a key would leak it; none is known to. |
| Gateway keys in logs | Keys are not logged by the gateway. Caddy redacts `Authorization` itself, and its access logs on the API host drop `X-Api-Key` and `X-Goog-Api-Key` (`deploy/caddy/Caddyfile`). | A load balancer or proxy of your own in front of Caddy must do the same. |
| Guessing or flooding gateway keys | 32 random bytes per key; lookup by HMAC-SHA256 with a pepper, no slow hash; malformed keys are refused before hashing. Failed attempts are limited per client address (60 a minute) on the data plane and for admin tokens. Logins lock an account after 10 failures in 15 minutes, and unknown accounts take as long as known ones. | The failure limit is per instance and per exact address; a client with many addresses (an IPv6 /64) gets more tries. A key's 256 bits make guessing hopeless anyway. Anyone who knows an email address can lock that account for 15 minutes. |
| Management API exposed | Its own listener, not served by `--role gateway`. Compose publishes no core port, and the backend network is internal; Helm's network policy lets only the console, analytics and tester reach the admin port. Passwords use Argon2id. Sessions are random tokens stored as hashes, ended by logout and by a password change. The console keeps the token in an httpOnly, SameSite=Lax cookie and relies on SvelteKit's origin check against cross-site form posts. | The console sends no Content-Security-Policy. |
| Server-side request forgery through admin-supplied URLs | Provider URLs and price sources are checked when saved, and every connection is checked again after name resolution (`pacod-gw-events/src/egress.rs`). Refused: private, loopback, link-local (cloud metadata), carrier-grade NAT, reserved and documentation ranges, and IPv6 forms that lead to them (IPv4-mapped and -compatible, NAT64, 6to4, Teredo, site-local). `PGW_EGRESS_ALLOW_PRIVATE` opens named hosts or ranges, such as a local Ollama. Price pages are fetched by the analytics service, which re-checks every redirect. | Alert webhooks come from the operator's settings and are not checked. |
| A compromised analytics service | It holds no provider keys. Its internal token opens only the job endpoints of the core (lease, result, extraction); it is not accepted as a principal of the management API. | Through the extraction endpoint it can have the `_system` workspace call a model, and its job results can propose or, within the auto-apply bound, change prices. |
| One workspace reading another's data | The workspace comes from the key, never from the request. Every report goes through the core, which replaces any scope the caller sent with the scopes their roles allow. The `/events` stream carries only changes of scopes the caller may read. | Prometheus metrics carry organisation and workspace labels, and the metrics port has no authentication: whoever reaches Prometheus or Grafana sees usage of every tenant. Keep them to operators. |
| One workspace using another's files or batches | Files and batches created through the gateway are recorded with their workspace (`dataplane/objects.rs`); lists show a workspace's own, and other ids answer 404. A body that names a `file_id` is checked against that record in both modes, with escaped and repeated keys read as the provider reads them; on the shared credential an unknown id is refused too (`dataplane/files.rs`). | On a workspace's or organisation's own credential an unknown id passes, a blind relay is not read, and proxy-mode file and batch paths are relayed unrecorded. Workspaces that share an organisation's credential are therefore separated only for what goes through the managed endpoints. Only Anthropic file ids are checked. |
| A caller making the provider reach servers of its choosing | Provider tools (web search and fetch, code execution, the MCP connector, Gemini's grounding) are allowed per workspace by type, checked on the body the provider receives in both modes (`dataplane/tools.rs`). A `tools` list the gateway cannot read is refused when a workspace restricts tools, and blind relay is refused there. | No restriction by default: a workspace, its organisation or the global settings must set the list. |
| Disagreement between gateway and provider about a request | Bodies are checked as strict JSON (`json/scan.rs`); any repeated top-level key is refused in both modes. Objects the gateway writes into must not repeat keys either. | Repeated keys deeper in a translated body keep their last value. |
| Calling a model server directly | Not provided by the deployment files: neither compose nor the Helm chart contains a model server. | A local model server must be reachable only from the gateways: its own network in compose, or an egress and ingress policy in Kubernetes. |
| Prompts in telemetry | Logs, spans and usage events carry metadata only: model, tokens, timing, ids, tags. | `error_message` can quote request content when a provider's error does. |
| Stored prompts and answers (audit mode) | Off by default; an organisation can forbid or require it. Credential headers are stored by name only, and the gateway key by its id. Only admins and auditors of the scope read records; reading, exporting and deleting them is written to the audit log. Records expire (14 days by default) and are deleted by ClickHouse. | Listing records without their bodies is not logged. Bodies are not scanned for secrets a caller put into a prompt. |
| Compromised dependency or image | Cargo.lock and `--locked` everywhere, `npm ci`; `cargo audit`, `cargo deny` (licences, bans, sources; `deny.toml`) and `npm audit` in CI, weekly as well. CI writes an SPDX bill of materials of every image. A pushed version tag builds the images, pushes them to GHCR, signs them by digest with cosign (keyless) and attests their bill of materials. The Rust services run on distroless images pinned by digest, as an unprivileged user, with a read-only root and no capabilities. | Third-party images are pinned by tag, not digest; the console's base image too, and the tester's (`python:3.13-slim`) floats within its minor version. The tester's Python dependencies are locked (`uv.lock`) but not audited, and licences are checked for the Rust dependencies only. Nothing in `deploy/` checks a signature before an image runs: do it as the runbook shows (section 6). |
| Abuse by an administrator | Roles from platform admin down to viewer; every configuration change is written to the audit log in the same transaction, with its actor and the hashes before and after. Logins and failed logins, key re-encryption and audit-record reads are logged too. | The audit log is an ordinary table: a database administrator can change it. Ship it elsewhere if that matters. |

## Not covered by the gateway

- The host, the cluster and their administrators.
- TLS between the services inside the stack; Caddy terminates TLS at the edge.
- Denial of service beyond the per-key throttles, concurrency caps and body limits.
- Secrets that callers put into prompts.
