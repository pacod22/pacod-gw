{{/* Chart name, shortened to what a label value may hold. */}}
{{- define "pacod-id.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Base name of every object. The longest suffix added to it is "-internal". */}}
{{- define "pacod-id.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 54 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 54 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 54 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/* Labels of every object. */}}
{{- define "pacod-id.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{ include "pacod-id.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/* Labels that select the pods. */}}
{{- define "pacod-id.selectorLabels" -}}
app.kubernetes.io/name: {{ include "pacod-id.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "pacod-id.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "pacod-id.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{- define "pacod-id.secretName" -}}
{{- default (printf "%s-secrets" (include "pacod-id.fullname" .)) .Values.secrets.existingSecret }}
{{- end }}

{{- define "pacod-id.image" -}}
{{- $tag := default .Chart.AppVersion .Values.image.tag }}
{{- if .Values.image.registry }}
{{- printf "%s/%s:%s" (trimSuffix "/" .Values.image.registry) .Values.image.repository $tag }}
{{- else }}
{{- printf "%s:%s" .Values.image.repository $tag }}
{{- end }}
{{- end }}
