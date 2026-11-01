{{- define "mallard.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "mallard.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "mallard.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{ include "mallard.selectorLabels" . }}
app.kubernetes.io/version: {{ .Values.image.tag | default .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "mallard.selectorLabels" -}}
app.kubernetes.io/name: {{ include "mallard.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "mallard.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "mallard.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/* Secret holding the API (and ingest) tokens. */}}
{{- define "mallard.tokenSecret" -}}
{{- default (include "mallard.fullname" .) .Values.existingSecret -}}
{{- end -}}

{{/* Secret holding storage credentials ("" = none). */}}
{{- define "mallard.storageSecret" -}}
{{- if .Values.storage.credentials.existingSecret -}}
{{- .Values.storage.credentials.existingSecret -}}
{{- else if .Values.storage.credentials.env -}}
{{- printf "%s-storage" (include "mallard.fullname" .) -}}
{{- end -}}
{{- end -}}

{{- define "mallard.catalogSecret" -}}
{{- default (printf "%s-catalog" (include "mallard.fullname" .)) .Values.catalog.postgres.existingSecret -}}
{{- end -}}

{{- define "mallard.pvcName" -}}
{{- default (printf "%s-data" (include "mallard.fullname" .)) .Values.catalog.duckdb.persistence.existingClaim -}}
{{- end -}}

{{- define "mallard.replicas" -}}
{{- if eq .Values.catalog.type "duckdb" -}}1{{- else -}}{{ .Values.replicaCount }}{{- end -}}
{{- end -}}

{{/* True ("true") when any storage location is on S3. */}}
{{- define "mallard.usesS3" -}}
{{- if or (regexMatch "^s3a?://" .Values.storage.dataPath) (regexMatch "^s3a?://" (.Values.storage.artifactsUrl | default "")) -}}true{{- end -}}
{{- end -}}

{{/* Fail early on invalid combinations. */}}
{{- define "mallard.validate" -}}
{{- if not .Values.publicUrl -}}
{{- fail "publicUrl is required (the URL SDKs use to reach the HTTP port, e.g. https://mallard.example.com)" -}}
{{- end -}}
{{- if and (not .Values.apiToken) (not .Values.existingSecret) -}}
{{- fail "set apiToken or existingSecret: the Sentry API rejects every request without a token" -}}
{{- end -}}
{{- if not (has .Values.catalog.type (list "duckdb" "postgres")) -}}
{{- fail "catalog.type must be duckdb or postgres" -}}
{{- end -}}
{{- if and (eq .Values.catalog.type "postgres") (not .Values.catalog.postgres.connectionString) (not .Values.catalog.postgres.existingSecret) -}}
{{- fail "catalog.type=postgres needs catalog.postgres.connectionString or catalog.postgres.existingSecret" -}}
{{- end -}}
{{- if not .Values.storage.dataPath -}}
{{- fail "storage.dataPath is required" -}}
{{- end -}}
{{- if and (eq .Values.catalog.type "postgres") (not (regexMatch "^[a-z0-9]+://" .Values.storage.dataPath)) -}}
{{- fail "catalog.type=postgres needs object storage for storage.dataPath (s3://, gs://, az://): a local path is not shared between pods" -}}
{{- end -}}
{{- end -}}
