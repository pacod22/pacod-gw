# pacod-gw Helm chart

Deploys pacod-gw on Kubernetes:

| Workload | Kind | Port | Reachable from |
|---|---|---|---|
| `gateway` | StatefulSet | 8080 data plane, 9100 metrics | the Ingress (API host) and applications in the cluster |
| `admin` | Deployment | 8081 management API, 9100 metrics | the console and the analytics service only |
| `analytics` | Deployment | 8090 | the gateway and admin pods only |
| `ui` | Deployment | 3000 | the Ingress (console host) |

The gateway is a StatefulSet because every instance keeps its event spool on its own
persistent volume. An instance that restarts ships what was left in its spool.

PostgreSQL and ClickHouse are not part of the chart. Use managed services or run them with
their own operators, and give the chart the connection settings.

The compose stack in `deploy/` is the packaging of the first release. This chart follows the
same contract (ports, environment, health endpoints). It has been linted and its manifests
validated against the Kubernetes schemas, but it has not been installed on a cluster yet.

## Requirements

- Kubernetes 1.27 or newer and Helm 3 or 4. The chart was checked with Helm 4.3 and its
  manifests against the schemas of Kubernetes 1.30 and of the current release.
- PostgreSQL with a database and a user that owns it. The compose stack runs PostgreSQL 18.
  The core creates and migrates its schema at start.
- ClickHouse with a user that may create a database and tables (only if `analytics.enabled`).
- An Ingress controller. The default annotations are for ingress-nginx.
- A storage class for the spool volumes (8 Gi per gateway instance by default).
- The three images in a registry the cluster can pull from. Build them with
  `deploy/docker/rust.Dockerfile` (targets `core` and `analytics`) and
  `deploy/docker/ui.Dockerfile`.
- For `metrics.serviceMonitor` and `metrics.prometheusRule`: the Prometheus Operator CRDs.
- For `networkPolicy`: a network plugin that enforces NetworkPolicy.

## Install

1. Generate the secrets. `docs/runbook.md` explains each one.

   ```sh
   kubectl create namespace pacod-gw
   kubectl -n pacod-gw create secret generic pacod-gw-secrets \
     --from-literal=DATABASE_URL='postgres://pacod_gw:PASSWORD@postgres.example.internal:5432/pacod_gw?sslmode=require' \
     --from-literal=PGW_KEK="$(openssl rand -base64 32)" \
     --from-literal=PGW_KEY_PEPPER="$(openssl rand -base64 32)" \
     --from-literal=PGW_INTERNAL_TOKEN="$(openssl rand -hex 32)" \
     --from-literal=PGW_ADMIN_PASSWORD="$(openssl rand -base64 18)" \
     --from-literal=CLICKHOUSE_PASSWORD='PASSWORD'
   ```

   Read `PGW_KEK` back and store a copy outside the cluster and outside the database
   backups. Provider credentials are encrypted under it; without it they cannot be read.

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
   ```

3. Install and wait.

   ```sh
   helm upgrade --install pacod-gw deploy/helm/pacod-gw -n pacod-gw -f my-values.yaml --wait --timeout 10m
   ```

   `--wait` returns when every pod answers `/readyz`: the core has loaded a configuration
   snapshot and can write its spool, and the analytics service has reached ClickHouse.

4. Open `https://console.example.com/`, log in as `core.adminEmail` with
   `PGW_ADMIN_PASSWORD`, and change the password.

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
| `ingress.api.host`, `ingress.console.host` | `api.example.com`, `console.example.com` | The two host names. |
| `ui.xffDepth` | `1` | Proxies in front of the console that add to `X-Forwarded-For`. |
| `networkPolicy.enabled` | `true` | Keeps the management API and analytics internal. |
| `networkPolicy.metricsFrom` | every pod in the cluster | Who may scrape port 9100. |
| `metrics.serviceMonitor.enabled` | `false` | Scrape configuration for the Prometheus Operator. |
| `metrics.prometheusRule.enabled` | `false` | The alert rules of `deploy/prometheus/rules`. |

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
