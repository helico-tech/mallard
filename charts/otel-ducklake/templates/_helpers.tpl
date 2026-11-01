{{- define "otel-ducklake.name" -}}
{{- .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "otel-ducklake.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "otel-ducklake.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{ include "otel-ducklake.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "otel-ducklake.selectorLabels" -}}
app.kubernetes.io/name: {{ include "otel-ducklake.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "otel-ducklake.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "otel-ducklake.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{- define "otel-ducklake.secretName" -}}
{{- default (printf "%s-auth" (include "otel-ducklake.fullname" .)) .Values.auth.existingSecret -}}
{{- end -}}

{{/* Validate mode-specific settings. */}}
{{- define "otel-ducklake.validate" -}}
{{- if not (has .Values.mode (list "single" "shared")) -}}
{{- fail "mode must be 'single' or 'shared'" -}}
{{- end -}}
{{- if eq .Values.mode "shared" -}}
{{- if not .Values.storage.catalog -}}
{{- fail "mode=shared requires storage.catalog (a PostgreSQL catalog, e.g. postgres:host=... dbname=...)" -}}
{{- end -}}
{{- if not (or (hasPrefix "postgres:" .Values.storage.catalog) (hasPrefix "postgresql:" .Values.storage.catalog) (hasPrefix "mysql:" .Values.storage.catalog)) -}}
{{- fail "mode=shared needs a network catalog (postgres:...); DuckDB/SQLite catalog files allow only one writer" -}}
{{- end -}}
{{- if not .Values.storage.dataPath -}}
{{- fail "mode=shared requires storage.dataPath on shared object storage (s3://, gs://, az://, ...)" -}}
{{- end -}}
{{- if not (contains "://" .Values.storage.dataPath) -}}
{{- fail "mode=shared requires storage.dataPath on object storage (s3://, gs://, az://, ...), not a local path" -}}
{{- end -}}
{{- end -}}
{{- if and .Values.auth.enabled (not .Values.auth.existingSecret) (not .Values.auth.apiToken) -}}
{{- fail "auth.enabled requires auth.apiToken or auth.existingSecret (keys api-token, ingest-token)" -}}
{{- end -}}
{{- if and .Values.query.enableSql (not .Values.auth.enabled) -}}
{{- fail "query.enableSql requires auth.enabled" -}}
{{- end -}}
{{- end -}}

{{/* Render a TOML value: strings quoted, numbers/bools as-is, lists of strings. */}}
{{- define "otel-ducklake.tomlValue" -}}
{{- if kindIs "string" . -}}{{ toJson . }}
{{- else if kindIs "bool" . -}}{{ . }}
{{- else if or (kindIs "float64" .) (kindIs "int" .) (kindIs "int64" .) -}}{{ . }}
{{- else if kindIs "slice" . -}}[{{ range $i, $v := . }}{{ if $i }}, {{ end }}{{ toJson $v }}{{ end }}]
{{- else -}}{{ toJson (toString .) }}
{{- end -}}
{{- end -}}

{{/* Pod template shared by the StatefulSet (single) and Deployment (shared). */}}
{{- define "otel-ducklake.podTemplate" -}}
metadata:
  labels:
    {{- include "otel-ducklake.selectorLabels" . | nindent 4 }}
    {{- with .Values.podLabels }}
    {{- toYaml . | nindent 4 }}
    {{- end }}
  annotations:
    checksum/config: {{ include (print $.Template.BasePath "/configmap.yaml") . | sha256sum }}
    {{- with .Values.podAnnotations }}
    {{- toYaml . | nindent 4 }}
    {{- end }}
spec:
  serviceAccountName: {{ include "otel-ducklake.serviceAccountName" . }}
  {{- with .Values.imagePullSecrets }}
  imagePullSecrets:
    {{- toYaml . | nindent 4 }}
  {{- end }}
  securityContext:
    {{- toYaml .Values.podSecurityContext | nindent 4 }}
  terminationGracePeriodSeconds: {{ .Values.terminationGracePeriodSeconds }}
  containers:
    - name: otel-ducklake
      image: "{{ .Values.image.repository }}:{{ .Values.image.tag | default .Chart.AppVersion }}"
      imagePullPolicy: {{ .Values.image.pullPolicy }}
      args: ["serve"]
      securityContext:
        {{- toYaml .Values.securityContext | nindent 8 }}
      env:
        - name: ODL_CONFIG
          value: /etc/otel-ducklake/config.toml
        {{- if .Values.auth.enabled }}
        - name: ODL_API_TOKEN
          valueFrom:
            secretKeyRef:
              name: {{ include "otel-ducklake.secretName" . }}
              key: api-token
        - name: ODL_INGEST_TOKEN
          valueFrom:
            secretKeyRef:
              name: {{ include "otel-ducklake.secretName" . }}
              key: ingest-token
        {{- end }}
        {{- with .Values.extraEnv }}
        {{- toYaml . | nindent 8 }}
        {{- end }}
      {{- with .Values.envFrom }}
      envFrom:
        {{- toYaml . | nindent 8 }}
      {{- end }}
      ports:
        - name: otlp-grpc
          containerPort: 4317
          protocol: TCP
        - name: otlp-http
          containerPort: 4318
          protocol: TCP
        - name: api
          containerPort: 8080
          protocol: TCP
      startupProbe:
        httpGet:
          path: /api/v1/health
          port: api
        periodSeconds: 2
        failureThreshold: 60
      livenessProbe:
        httpGet:
          path: /api/v1/health
          port: api
        periodSeconds: 15
        timeoutSeconds: 5
      readinessProbe:
        httpGet:
          path: /api/v1/health
          port: api
        periodSeconds: 5
        timeoutSeconds: 3
      resources:
        {{- toYaml .Values.resources | nindent 8 }}
      volumeMounts:
        - name: config
          mountPath: /etc/otel-ducklake
          readOnly: true
        - name: data
          mountPath: /data
        - name: tmp
          mountPath: /tmp
  volumes:
    - name: config
      configMap:
        name: {{ include "otel-ducklake.fullname" . }}
    - name: tmp
      emptyDir: {}
    {{- if or (eq .Values.mode "shared") (not .Values.persistence.enabled) }}
    - name: data
      emptyDir: {}
    {{- end }}
  {{- with .Values.nodeSelector }}
  nodeSelector:
    {{- toYaml . | nindent 4 }}
  {{- end }}
  {{- with .Values.affinity }}
  affinity:
    {{- toYaml . | nindent 4 }}
  {{- end }}
  {{- with .Values.tolerations }}
  tolerations:
    {{- toYaml . | nindent 4 }}
  {{- end }}
  {{- with .Values.topologySpreadConstraints }}
  topologySpreadConstraints:
    {{- toYaml . | nindent 4 }}
  {{- end }}
{{- end -}}
