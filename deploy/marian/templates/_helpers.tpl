{{- define "marian.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "marian.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{- define "marian.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
app.kubernetes.io/name: {{ include "marian.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "marian.selectorLabels" -}}
app.kubernetes.io/name: {{ include "marian.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "marian.secretName" -}}
{{- if .Values.discord.existingSecret }}
{{- .Values.discord.existingSecret }}
{{- else }}
{{- include "marian.fullname" . }}
{{- end }}
{{- end }}

{{- define "marian.publicUrl" -}}
{{- if .Values.publicUrl }}
{{- .Values.publicUrl }}
{{- else if and .Values.ingress.enabled .Values.ingress.host }}
{{- printf "https://%s" .Values.ingress.host }}
{{- end }}
{{- end }}
