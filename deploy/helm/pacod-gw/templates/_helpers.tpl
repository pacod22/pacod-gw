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
- name: PGW_KEK_ID
  value: {{ $root.Values.core.kekId | quote }}
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
{{- if $root.Values.analytics.enabled }}
- name: PGW_ANALYTICS_URL
  value: {{ printf "http://%s-analytics:%v" (include "pacod-gw.fullname" $root) $root.Values.analytics.service.port | quote }}
- name: PGW_INTERNAL_TOKEN
  valueFrom:
    secretKeyRef:
      name: {{ $secret }}
      key: PGW_INTERNAL_TOKEN
{{- end }}
- name: PGW_SPOOL_DIR
  value: /var/lib/pacod-gw/spool
{{- if $root.Values.core.otlpEndpoint }}
- name: PGW_OTLP_ENDPOINT
  value: {{ $root.Values.core.otlpEndpoint | quote }}
{{- end }}
{{- if $root.Values.core.egressAllowPrivate }}
- name: PGW_EGRESS_ALLOW_PRIVATE
  value: {{ join "," $root.Values.core.egressAllowPrivate | quote }}
{{- end }}
{{- if $root.Values.core.config }}
- name: PGW_CONFIG
  value: /etc/pacod-gw/gateway.toml
{{- end }}
- name: RUST_LOG
  value: {{ $root.Values.core.logLevel | quote }}
- name: PGW_LOG_FORMAT
  value: {{ $root.Values.logFormat | quote }}
{{- with $root.Values.core.extraEnv }}
{{ toYaml . }}
{{- end }}
{{- end }}
