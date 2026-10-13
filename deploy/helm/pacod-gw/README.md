# pacod-gw Helm chart

Deploys pacod-gw on Kubernetes:

| Workload | Kind | Port | Reachable from |
|---|---|---|---|
| `gateway` | StatefulSet | 8080 data plane, 9100 metrics | the Ingress (API host) and applications in the cluster |
| `admin` | Deployment | 8081 management API, 9100 metrics | the console and the analytics service only |
| `analytics` | Deployment | 8090 | the gateway and admin pods only |
| `ui` | Deployment | 3000 | the Ingress (console host) |
| `tester` | Deployment | 8095 | the console only |
| `pacod-id` (subchart `id`) | Deployment | 8070 sign-in, 8071 internal, 9170 metrics | 8070: the Ingress (id host); 8071: the admin, console and tester pods |

The gateway is a StatefulSet because every instance keeps its event spool on its own
persistent volume. An instance that restarts ships what was left in its spool.

pacod-id signs people in (`auth: oidc`). It is pacod-id's own chart, vendored at
`vendor/pacod-id/deploy/helm/pacod-id` and packaged in `charts/`; see
[Signing in: pacod-id](#signing-in-pacod-id).

PostgreSQL and ClickHouse are not part of the chart. Use managed services or run them with
their own operators, and give the chart the connection settings.

The compose stack in `deploy/` is the packaging of the first release. This chart follows the
same contract (ports, environment, health endpoints). It has been linted and its manifests
validated against the Kubernetes schemas, but it has not been installed on a cluster yet.

## Requirements

- Kubernetes 1.27 or newer and Helm 3 or 4. The chart was checked with Helm 4.3 and its
  manifests against the schemas of Kubernetes 1.30 and of the current release.
- PostgreSQL with a database and a user that owns it. The compose stack runs PostgreSQL 18.
  The core creates and migrates its schema at start. pacod-id needs a database and a user of
  its own (`pacod_id`, as `deploy/scripts/create-id-role.sh` makes them); it migrates too.
- ClickHouse with a user that may create a database and tables (only if `analytics.enabled`).
- An Ingress controller. The default annotations are for ingress-nginx.
- A storage class for the spool volumes (8 Gi per gateway instance by default).
- The images in a registry the cluster can pull from. Build them with
  `deploy/docker/rust.Dockerfile` (targets `core` and `analytics`),
  `deploy/docker/ui.Dockerfile` and `deploy/docker/tester.Dockerfile`; pacod-id's image with
  `vendor/pacod-id/deploy/docker/pacod-id.Dockerfile` (`make image` in `vendor/pacod-id`).
- For `metrics.serviceMonitor`, `metrics.prometheusRule` and `metrics.alertmanagerConfig`:
  the Prometheus Operator CRDs.
- For `networkPolicy`: a network plugin that enforces NetworkPolicy.

## Install

1. Generate the secrets. `docs/runbook.md` explains each one.

   ```sh
   kubectl create namespace pacod-gw
   pepper="$(openssl rand -base64 32)"
   kubectl -n pacod-gw create secret generic pacod-gw-secrets \
     --from-literal=DATABASE_URL='postgres://pacod_gw:PASSWORD@postgres.example.internal:5432/pacod_gw?sslmode=require' \
     --from-literal=PGW_KEK="$(openssl rand -base64 32)" \
     --from-literal=PGW_KEY_PEPPER="$pepper" \
     --from-literal=PGW_INTERNAL_TOKEN="$(openssl rand -hex 32)" \
     --from-literal=PGW_ADMIN_PASSWORD="$(openssl rand -base64 18)" \
     --from-literal=CLICKHOUSE_PASSWORD='PASSWORD' \
     --from-literal=PGW_UI_SESSION_SECRET="$(openssl rand -hex 32)"
   kubectl -n pacod-gw create secret generic pacod-id-secrets \
     --from-literal=PID_DATABASE_URL='postgres://pacod_id:PASSWORD@postgres.example.internal:5432/pacod_id?sslmode=require' \
     --from-literal=PID_KEK="$(openssl rand -base64 32)" \
     --from-literal=PID_TOKEN_PEPPER="$(openssl rand -hex 32)" \
     --from-literal=PID_LEGACY_PEPPERS="gw:$pepper" \
     --from-literal=PGW_ID_CLIENT_SECRET="$(openssl rand -hex 32)" \
     --from-literal=PGW_UI_CLIENT_SECRET="$(openssl rand -hex 32)" \
     --from-literal=TESTER_ID_CLIENT_SECRET="$(openssl rand -hex 32)"
   ```

   Read `PGW_KEK` and `PID_KEK` back and store copies outside the cluster and outside the
   database backups. Provider credentials and pacod-id's signing keys are encrypted under
   them; without them they cannot be read.

   ```sh
   kubectl -n pacod-gw get secret pacod-gw-secrets -o jsonpath='{.data.PGW_KEK}' | base64 -d
   ```

2. Write a values file.

   ```yaml
   image:
     registry: registry.example.com/platform
     tag: "0.1.0"
   secrets:
     existingSecret: pacod-gw-secrets
   core:
     kekId: "2026-10"
     adminEmail: admin@example.com
   clickhouse:
     url: http://clickhouse.example.internal:8123
   ingress:
     className: nginx
     api:
       host: api.example.com
       tls:
         secretName: api-example-com-tls
     console:
       host: console.example.com
       tls:
         secretName: console-example-com-tls
     id:
       host: id.example.com
       tls:
         secretName: id-example-com-tls
   id:
     image:
       registry: registry.example.com/platform
     # The same host names as ingress.id.host and ingress.console.host.
     extraEnv:
       - name: PGW_ID_HOST
         value: id.example.com
       - name: PGW_CONSOLE_HOST
         value: console.example.com
   ```

3. Install and wait.

   ```sh
   helm upgrade --install pacod-gw deploy/helm/pacod-gw -n pacod-gw -f my-values.yaml --wait --timeout 10m
   ```

   `--wait` returns when every pod answers `/readyz`: the core has loaded a configuration
   snapshot and can write its spool, and the analytics service has reached ClickHouse.

4. Open `https://console.example.com/`, log in as `core.adminEmail` with
   `PGW_ADMIN_PASSWORD`, and change the password. That is `auth: local`, the default; for
   signing in through pacod-id see [Signing in: pacod-id](#signing-in-pacod-id).

Instead of `existingSecret` the secrets can be given as values (`secrets.kek` and so on).
The chart then creates the Secret. Do that only if the values file is itself kept secret.

With `secrets.mount: true` the core reads its secrets from the Secret mounted as files
instead of from environment variables, so they do not show in the process environment or in
`kubectl describe`. Optional keys (`PGW_KEK_OLD`, `PGW_ADMIN_PASSWORD`, `PGW_SMTP_PASSWORD`)
may be missing from the Secret.

## Values

`values.yaml` documents every value. The ones most installations set:

| Value | Default | Meaning |
|---|---|---|
| `image.registry`, `image.tag` | empty, chart `appVersion` | Where the images are and which version. |
| `secrets.existingSecret` | empty | A Secret you manage. Its keys are listed in `values.yaml`. |
| `secrets.mount` | `false` | The core reads the Secret as mounted files, not from its environment. |
| `core.kekId` | `default` | Name of the key-encryption key in use. |
| `core.adminEmail` | empty | The first administrator. |
| `core.otlpEndpoint` | empty | OTLP over HTTP endpoint for traces. |
| `core.egressAllowPrivate` | `[]` | Private hosts or ranges where providers, or sources of the price job, may live. |
| `core.trustedProxies` | `[private_ranges]` | Load balancers whose `X-Forwarded-For` the gateways believe: the ingress controller. Empty: the peer address counts. |
| `core.config` | empty | Content of the optional bootstrap file (`config/gateway.example.toml`). |
| `gateway.replicas` | `2` | Gateway instances. |
| `gateway.spool.size`, `gateway.spool.storageClass` | `8Gi`, cluster default | The spool volume of each instance. |
| `gateway.dbMaxConnections`, `admin.dbMaxConnections` | empty: 4 and 10 | PostgreSQL connections per pod. PostgreSQL must accept their sum plus a few. |
| `analytics.enabled` | `true` | `false` gives the minimal setup without reports and audit mode. |
| `clickhouse.url`, `.database`, `.user` | empty, `pacod_gw`, `pacod_gw` | The external ClickHouse. |
| `auth` | `local` | Who signs people in to the console: `local` (the gateway) or `oidc` (pacod-id). `PGW_AUTH` of the core, the console and the tester. |
| `ingress.api.host`, `ingress.console.host`, `ingress.id.host` | `api.example.com`, `console.example.com`, `id.example.com` | The three host names. `https://<ingress.id.host>` is the realm's issuer. |
| `secrets.uiSessionSecret` | empty | Key of the console's session cookie; required with `auth: oidc`. |
| `id.enabled` | `true` | Deploy pacod-id. `false` requires `auth: local`. |
| `id.extraEnv` | `PGW_ID_HOST`, `PGW_CONSOLE_HOST` | The host names `id.config` uses; they must match the Ingress hosts. |
| `id.config` | the gateway's realm and its three clients | pacod-id's TOML, as `deploy/pacod-id/pacod-id.toml`. |
| `id.secrets.existingSecret` | `pacod-id-secrets` | The name of pacod-id's Secret, which this chart writes (unless `secrets.existingSecret` is set). One per release in a namespace. |
| `id.secrets.databaseUrl`, `.kek`, `.tokenPepper`, `.extra.*` | empty | pacod-id's database, key, pepper and the three client secrets (each at least 32 characters). |
| `id.email.*` | empty | pacod-id's mail; set it to the gateway's `email.*`. |
| `id.bootstrapAdmin.email`, `id.secrets.bootstrapAdminPassword` | empty | A first platform admin, on a new installation only. |
| `ui.xffDepth` | `1` | Proxies in front of the console that add to `X-Forwarded-For`. |
| `networkPolicy.enabled` | `true` | Keeps the management API and analytics internal. |
| `networkPolicy.metricsFrom` | every pod in the cluster | Who may scrape port 9100. |
| `metrics.serviceMonitor.enabled` | `false` | Scrape configuration for the Prometheus Operator. |
| `metrics.prometheusRule.enabled` | `false` | The alert rules of `deploy/prometheus/rules`. |
| `metrics.alertmanagerConfig.enabled` | `false` | Delivers them: an AlertmanagerConfig with `emailTo` (the SMTP account of `email.*`) and `webhookUrl`. |

## What the chart sets up

**Probes.** Every pod has a startup and a readiness probe on `/readyz` and a liveness probe
on `/healthz`. The gateway serves them on 8080, the admin role on 8081, analytics on 8090.
The console is probed on `/`.

**Network policies.** Three policies: the admin pods accept the management port from the
console and analytics pods only; the analytics pods accept their port from the gateway and
admin pods only; the gateway pods accept the data plane from anywhere and nothing else.
The metrics port is open to `networkPolicy.metricsFrom`. No Ingress points at the
management API. To reach it from outside the cluster, use `kubectl port-forward`, or add
your Ingress controller's pods to `networkPolicy.adminExtraFrom` and write an Ingress with
an address allow-list.

**pacod-id's network policy.** The subchart's policy opens 8070 to anyone (the Ingress
controller), 8071 to `id.networkPolicy.internalFrom` (the gateway's admin, console and tester
pods) and 9170 to `id.networkPolicy.metricsFrom`. The gateway's policies restrict incoming
traffic only, so its pods need no rule to reach pacod-id.

**The console behind the Ingress.** The console derives its origin from the `Host` header
and `X-Forwarded-Proto`. The Ingress controller must pass `Host` unchanged and set
`X-Forwarded-Proto` and `X-Forwarded-For`. If the login form answers "Cross-site POST form
submissions are forbidden", one of the two is missing or `ui.xffDepth` is wrong.

**Streaming.** The data plane answers with server-sent events and accepts bodies of tens of
megabytes. The default annotations switch off buffering in ingress-nginx, raise the body
limit to 64 MB and the timeouts to 11 minutes. With another controller, set the equivalent.

**Rolling changes.** A change to the Secret created by the chart or to `core.config`
restarts the pods. With `existingSecret` restart them yourself:
`kubectl -n pacod-gw rollout restart statefulset,deployment -l app.kubernetes.io/instance=pacod-gw`.
pacod-id reads its Secret through `existingSecret`, so it is never restarted by a change to it;
the admin, console and tester pods are (when the chart writes it).

## Signing in: pacod-id

pacod-id is the subchart `id` (pacod-id's own chart, `vendor/pacod-id/deploy/helm/pacod-id`;
its runbook is `vendor/pacod-id/docs/runbook.md`). It runs with either `auth`, so that a
gateway's people can be moved to it before the switch to `auth: oidc` (the gateway's
runbook). It is the Compose stack's `id` service in Kubernetes form.

A subchart sees only its own values (`id.*`) and takes them literally, so the gateway's settings
cannot flow into it. The chart bridges that in three ways:

- **One Secret for both sides.** The chart writes pacod-id's Secret (named by
  `id.secrets.existingSecret`) from `id.secrets.*`, with `PID_LEGACY_PEPPERS` =
  `gw:<secrets.keyPepper>` (old `pgwa_` admin tokens keep working) and `PID_SMTP_PASSWORD` =
  `secrets.smtpPassword` filled in. pacod-id reads it as files; the core
  (`PGW_ID_CLIENT_SECRET`), the console (`PGW_UI_CLIENT_SECRET`) and the tester
  (`TESTER_ID_CLIENT_SECRET`) read their client secret from the same Secret. With
  `secrets.mount` the core's is projected into its mounted secrets. The console's
  session key (`PGW_UI_SESSION_SECRET`) stays in the gateway's Secret.
- **Checked copies.** `id.config` takes the host names from `id.extraEnv`; the chart refuses
  to render when the issuer is not `https://<ingress.id.host>` or the console's redirect URI is
  not `https://<ingress.console.host>/auth/callback`.
- **Copies by hand.** `id.email.*` must repeat the gateway's `email.*` (the notes warn when it
  does not), and `id.networkPolicy.internalFrom` selects the pods by `app.kubernetes.io/name:
  pacod-gw`, which changes with `nameOverride`.

The core gets `PGW_AUTH`, `PGW_ID_ISSUER`, `PGW_ID_INTERNAL_URL`
(`http://<release>-pacod-id-internal:8071`) and `PGW_ID_CLIENT_SECRET`; the console the same
with its own secret, `PGW_UI_SESSION_SECRET` and `PGW_CONSOLE_URL`; the tester `PGW_AUTH`,
`PGW_ID_INTERNAL_URL` and `TESTER_ID_CLIENT_SECRET`. The gateways need none of it. The id host
is routed by this chart's Ingress, like the other two; the subchart's own Ingress stays off.

## Operations

- **Upgrade:** set `image.tag` and run `helm upgrade`. The core migrates PostgreSQL at start
  and the analytics service migrates ClickHouse. Instances are replaced one at a time; an
  instance one version behind still reads the current configuration.
- **Scale:** `gateway.replicas`. Each new instance gets its own spool volume. Scaling down
  leaves the volume claim in place; an instance that comes back ships what was in it. Delete
  the claim only after checking that `gw_spool_bytes` of that instance was zero.
- **Several instances** behave as in `docs/runbook.md`, section 9: the console's Monitoring
  page adds up every pod, a key's concurrency cap is shared out over the gateway pods, and
  the ServiceMonitor names every series by its pod, which is also the instance id the
  console shows. A stopping gateway pod fails its readiness probe for six seconds while it
  still serves, then drains (`terminationGracePeriodSeconds` covers both).
- **Admin spool:** the admin pods keep their event spool in an `emptyDir`. They record only
  the console's playground calls and the price job's page reads; what has not been
  delivered when an admin pod is replaced is lost.
- **Key rotation, backup and restore:** as in `docs/runbook.md`. The secrets live in the
  Kubernetes Secret instead of `deploy/.env`; the database procedures apply to your external
  PostgreSQL and ClickHouse.

## Checks

pacod-id's chart is read from `charts/pacod-id-<version>.tgz`. After `vendor/pacod-id`
changes, package it again (the repository root must be mounted, since the dependency lives
outside `deploy/helm`):

```sh
docker run --rm -v "$PWD:/repo" -w /repo/deploy/helm alpine/helm:4.3.0 dependency update pacod-gw
```

`make deploy-check` in the repository root runs these with pinned tool images:

```sh
helm lint deploy/helm/pacod-gw -f deploy/helm/pacod-gw/ci/test-values.yaml --strict
helm template pacod-gw deploy/helm/pacod-gw -f deploy/helm/pacod-gw/ci/test-values.yaml \
  | kubeconform -strict -summary -schema-location default \
      -schema-location 'https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json'
cmp deploy/prometheus/rules/pacod-gw.rules.yml deploy/helm/pacod-gw/files/pacod-gw.rules.yml
```

`files/pacod-gw.rules.yml` is a copy of the alert rules of the compose stack. Change the
file in `deploy/prometheus/rules/` and copy it here; CI fails when the two differ.
