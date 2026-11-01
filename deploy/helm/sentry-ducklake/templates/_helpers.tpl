{{- define "sdl.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "sdl.fullname" -}}
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

{{- define "sdl.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{ include "sdl.selectorLabels" . }}
app.kubernetes.io/version: {{ .Values.image.tag | default .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "sdl.selectorLabels" -}}
app.kubernetes.io/name: {{ include "sdl.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "sdl.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "sdl.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/* Secret holding the API token. */}}
{{- define "sdl.tokenSecret" -}}
{{- default (include "sdl.fullname" .) .Values.existingSecret -}}
{{- end -}}

{{/* Secret holding storage credentials ("" = none). */}}
{{- define "sdl.storageSecret" -}}
{{- if .Values.storage.credentials.existingSecret -}}
{{- .Values.storage.credentials.existingSecret -}}
{{- else if .Values.storage.credentials.env -}}
{{- printf "%s-storage" (include "sdl.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/* Secret holding the postgres catalog string. */}}
{{- define "sdl.catalogSecret" -}}
{{- default (printf "%s-catalog" (include "sdl.fullname" .)) .Values.catalog.postgres.existingSecret -}}
{{- end -}}

{{- define "sdl.pvcName" -}}
{{- default (printf "%s-data" (include "sdl.fullname" .)) .Values.catalog.duckdb.persistence.existingClaim -}}
{{- end -}}

{{- define "sdl.replicas" -}}
{{- if eq .Values.catalog.type "duckdb" -}}1{{- else -}}{{ .Values.replicaCount }}{{- end -}}
{{- end -}}

{{/* Fail early on invalid combinations. */}}
{{- define "sdl.validate" -}}
{{- if not .Values.publicUrl -}}
{{- fail "publicUrl is required (the URL SDKs use to reach the server, e.g. https://sentry.example.com)" -}}
{{- end -}}
{{- if and (not .Values.apiToken) (not .Values.existingSecret) -}}
{{- fail "set apiToken or existingSecret: the server refuses to start without an API token" -}}
{{- end -}}
{{- if not (has .Values.catalog.type (list "duckdb" "postgres")) -}}
{{- fail "catalog.type must be duckdb or postgres" -}}
{{- end -}}
{{- if and (eq .Values.catalog.type "postgres") (not .Values.catalog.postgres.connectionString) (not .Values.catalog.postgres.existingSecret) -}}
{{- fail "catalog.type=postgres needs catalog.postgres.connectionString or catalog.postgres.existingSecret" -}}
{{- end -}}
{{- if not .Values.storage.url -}}
{{- fail "storage.url is required" -}}
{{- end -}}
{{- if and (eq .Values.catalog.type "postgres") (not (regexMatch "^(s3a?|gs|gcs|az|azure)://" .Values.storage.url)) -}}
{{- fail "catalog.type=postgres needs object storage (s3://, gs:// or az://): a local storage path is not shared between pods" -}}
{{- end -}}
{{- end -}}
