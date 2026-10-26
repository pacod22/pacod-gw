{{/* Chart name, shortened to what a label value may hold. */}}
{{- define "pacod-gw.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Base name of every object. The longest suffix added to it is "-gateway-headless". */}}
{{- define "pacod-gw.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 46 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 46 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 46 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/* Labels of every object. */}}
{{- define "pacod-gw.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
app.kubernetes.io/name: {{ include "pacod-gw.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: pacod-gw
{{- end }}

{{/*
Labels that select the pods of one component.
Call with (dict "root" . "component" "gateway").
*/}}
{{- define "pacod-gw.selectorLabels" -}}
app.kubernetes.io/name: {{ include "pacod-gw.name" .root }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end }}

{{- define "pacod-gw.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "pacod-gw.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{- define "pacod-gw.secretName" -}}
{{- default (printf "%s-secrets" (include "pacod-gw.fullname" .)) .Values.secrets.existingSecret }}
{{- end }}

{{/*
Image reference of one component.
Call with (dict "root" . "image" .Values.image.core).
*/}}
{{- define "pacod-gw.image" -}}
{{- $tag := default .root.Chart.AppVersion .root.Values.image.tag }}
{{- if .root.Values.image.registry }}
{{- printf "%s/%s:%s" (trimSuffix "/" .root.Values.image.registry) .image.repository $tag }}
{{- else }}
{{- printf "%s:%s" .image.repository $tag }}
{{- end }}
{{- end }}

{{/*
Annotations that restart the pods when the Secret or the bootstrap file changes.
*/}}
{{- define "pacod-gw.checksums" -}}
{{- if not .Values.secrets.existingSecret }}
checksum/secret: {{ include (print $.Template.BasePath "/secret.yaml") . | sha256sum }}
{{- end }}
{{- if .Values.core.config }}
checksum/config: {{ .Values.core.config | sha256sum }}
{{- end }}
{{- end }}

{{/*
Environment of the core, shared by the gateway and the admin role.
Call with (dict "root" . "role" "gateway").
*/}}
{{- define "pacod-gw.coreEnv" -}}
{{- $root := .root -}}
{{- $secret := include "pacod-gw.secretName" $root -}}
- name: PGW_ROLE
  value: {{ .role | quote }}
- name: PGW_LISTEN
  value: "0.0.0.0:8080"
- name: PGW_ADMIN_LISTEN
  value: "0.0.0.0:8081"
- name: PGW_METRICS_LISTEN
  value: "0.0.0.0:9100"
# The pod name identifies the instance in events, metrics and the instance list.
- name: PGW_INSTANCE_ID
  valueFrom:
    fieldRef:
      fieldPath: metadata.name
- name: PGW_KEK_ID
  value: {{ $root.Values.core.kekId | quote }}
{{- if $root.Values.secrets.mount }}
# The secrets are read from the mounted Secret, one file per key, not from the environment.
- name: PGW_SECRETS_DIR
  value: /var/run/secrets/pacod-gw
{{- else }}
- name: DATABASE_URL
  valueFrom:
    secretKeyRef:
      name: {{ $secret }}
      key: DATABASE_URL
- name: PGW_KEK
  valueFrom:
    secretKeyRef:
      name: {{ $secret }}
      key: PGW_KEK
- name: PGW_KEK_OLD
  valueFrom:
    secretKeyRef:
      name: {{ $secret }}
      key: PGW_KEK_OLD
      optional: true
- name: PGW_KEY_PEPPER
  valueFrom:
    secretKeyRef:
      name: {{ $secret }}
      key: PGW_KEY_PEPPER
{{- end }}
{{- if $root.Values.analytics.enabled }}
- name: PGW_ANALYTICS_URL
  value: {{ printf "http://%s-analytics:%v" (include "pacod-gw.fullname" $root) $root.Values.analytics.service.port | quote }}
{{- if not $root.Values.secrets.mount }}
- name: PGW_INTERNAL_TOKEN
  valueFrom:
    secretKeyRef:
      name: {{ $secret }}
      key: PGW_INTERNAL_TOKEN
{{- end }}
{{- end }}
- name: PGW_SPOOL_DIR
  value: /var/lib/pacod-gw/spool
{{- if $root.Values.core.otlpEndpoint }}
- name: PGW_OTLP_ENDPOINT
  value: {{ $root.Values.core.otlpEndpoint | quote }}
{{- end }}
{{- $egress := $root.Values.core.egressAllowPrivate }}
{{- if $root.Values.loadMock.enabled }}
{{- $egress = append $egress (printf "%s-load-mock" (include "pacod-gw.fullname" $root)) }}
{{- end }}
{{- if $egress }}
- name: PGW_EGRESS_ALLOW_PRIVATE
  value: {{ join "," $egress | quote }}
{{- end }}
{{- if $root.Values.core.trustedProxies }}
- name: PGW_TRUSTED_PROXIES
  value: {{ join "," $root.Values.core.trustedProxies | quote }}
{{- end }}
{{- $db := ternary $root.Values.gateway.dbMaxConnections $root.Values.admin.dbMaxConnections (eq .role "gateway") }}
{{- if $db }}
- name: PGW_DB_MAX_CONNECTIONS
  value: {{ $db | quote }}
{{- end }}
{{- if $root.Values.core.config }}
- name: PGW_CONFIG
  value: /etc/pacod-gw/gateway.toml
{{- end }}
{{- if $root.Values.core.valkeyUrl }}
- name: PGW_VALKEY_URL
  value: {{ $root.Values.core.valkeyUrl | quote }}
{{- if $root.Values.core.valkeyCaFile }}
- name: PGW_VALKEY_CA_FILE
  value: {{ $root.Values.core.valkeyCaFile | quote }}
{{- end }}
{{- if not $root.Values.secrets.mount }}
- name: PGW_VALKEY_PASSWORD
  valueFrom:
    secretKeyRef:
      name: {{ $secret }}
      key: PGW_VALKEY_PASSWORD
      optional: true
{{- end }}
{{- end }}
- name: RUST_LOG
  value: {{ $root.Values.core.logLevel | quote }}
- name: PGW_LOG_FORMAT
  value: {{ $root.Values.logFormat | quote }}
{{- with $root.Values.core.extraEnv }}
{{ toYaml . }}
{{- end }}
{{- end }}

{{/*
The mounted Secret of the core, when secrets.mount is set: the volume mount and the volume.
*/}}
{{- define "pacod-gw.secretsMount" -}}
{{- if .Values.secrets.mount }}
- name: secrets
  mountPath: /var/run/secrets/pacod-gw
  readOnly: true
{{- end }}
{{- end }}

{{- define "pacod-gw.secretsVolume" -}}
{{- if .Values.secrets.mount }}
- name: secrets
  secret:
    secretName: {{ include "pacod-gw.secretName" . }}
    # Readable by the pod's group (fsGroup), not by others.
    defaultMode: 0440
{{- end }}
{{- end }}

{{/*
pacod-id, the subchart under the alias "id" (values id.*). Its templates see only id.*, so the
names they give their objects are computed here the same way: pacod-id.fullname with the
alias as the chart name.
*/}}
{{- define "pacod-gw.id.fullname" -}}
{{- $id := .Values.id -}}
{{- if $id.fullnameOverride }}
{{- $id.fullnameOverride | trunc 54 | trimSuffix "-" }}
{{- else }}
{{- $name := default "id" $id.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 54 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 54 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/* The issuer of the gateway's realm: pacod-id's public address. */}}
{{- define "pacod-gw.id.issuer" -}}
{{- printf "https://%s" (required "ingress.id.host is required with id.enabled: pacod-id's host name, the realm's issuer" .Values.ingress.id.host) }}
{{- end }}

{{/* pacod-id's internal listener, for the core, the console and the tester. */}}
{{- define "pacod-gw.id.internalUrl" -}}
{{- printf "http://%s-internal:8071" (include "pacod-gw.id.fullname" .) }}
{{- end }}

{{/* The Secret pacod-id reads (id.secrets.existingSecret), which this chart writes. */}}
{{- define "pacod-gw.id.secretName" -}}
{{- required "id.secrets.existingSecret is required with id.enabled: the name of pacod-id's Secret" .Values.id.secrets.existingSecret }}
{{- end }}

{{/*
Annotations that restart a pod when pacod-id's Secret (as this chart writes it) changes: the
client secrets in it must stay the same on both sides.
*/}}
{{- define "pacod-gw.id.checksum" -}}
{{- if and .Values.id.enabled (not .Values.secrets.existingSecret) }}
checksum/id-secret: {{ include (print $.Template.BasePath "/id-secret.yaml") . | sha256sum }}
{{- end }}
{{- end }}

{{/*
Refuses values that cannot work: an unknown auth mode, oidc without pacod-id, and a realm
configuration whose issuer or console addresses are not the Ingress hosts.
*/}}
{{- define "pacod-gw.validate" -}}
{{- if not (has .Values.auth (list "local" "oidc")) }}
{{- fail (printf "auth must be local or oidc, not %q" (toString .Values.auth)) }}
{{- end }}
{{- if and (eq .Values.auth "oidc") (not .Values.id.enabled) }}
{{- fail "auth: oidc needs id.enabled: true (pacod-id signs people in)" }}
{{- end }}
{{- if .Values.id.enabled }}
{{- /* The configuration as pacod-id reads it: ${NAME} replaced from id.extraEnv. */}}
{{- $config := .Values.id.config }}
{{- range .Values.id.extraEnv }}
{{- if hasKey . "value" }}
{{- $config = replace (printf "${%s}" .name) (toString .value) $config }}
{{- end }}
{{- end }}
{{- $issuer := include "pacod-gw.id.issuer" . }}
{{- if not (contains (printf "issuer = %q" $issuer) $config) }}
{{- fail (printf "id.config: the realm's issuer must be %q (https://<ingress.id.host>); set PGW_ID_HOST in id.extraEnv to ingress.id.host" $issuer) }}
{{- end }}
{{- $callback := printf "https://%s/auth/callback" (required "ingress.console.host is required" .Values.ingress.console.host) }}
{{- if not (contains (quote $callback) $config) }}
{{- fail (printf "id.config: the console's redirect URI must be %q (https://<ingress.console.host>); set PGW_CONSOLE_HOST in id.extraEnv to ingress.console.host" $callback) }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Whether load tests are set up: an agent in the cluster or agents elsewhere.
*/}}
{{- define "pacod-gw.load.enabled" -}}
{{- if and .Values.tester.enabled (or .Values.loadAgent.enabled .Values.tester.loadAgents) }}true{{- end }}
{{- end }}

{{/*
The agents the tester offers: "name=url,…".
*/}}
{{- define "pacod-gw.load.agents" -}}
{{- $agents := list }}
{{- if .Values.loadAgent.enabled }}
{{- $agents = append $agents (printf "local=http://%s-load-agent:%v" (include "pacod-gw.fullname" .) .Values.loadAgent.service.port) }}
{{- end }}
{{- range .Values.tester.loadAgents }}
{{- $agents = append $agents (printf "%s=%s" (required "tester.loadAgents[].name is required" .name) (required "tester.loadAgents[].url is required" .url)) }}
{{- end }}
{{- join "," $agents }}
{{- end }}
