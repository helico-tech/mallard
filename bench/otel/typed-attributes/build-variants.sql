-- Builds the variants from a backfilled lake (see README.md). Replace ${WORK}.
LOAD ducklake;
LOAD json;
SET threads=4;
ATTACH 'ducklake:${WORK}/catalog.ducklake' AS src (DATA_PATH '${WORK}/lake/', READ_ONLY);
ATTACH 'ducklake:${WORK}/v/m.ducklake' AS m (DATA_PATH '${WORK}/v/m/');
ATTACH 'ducklake:${WORK}/v/p.ducklake' AS p (DATA_PATH '${WORK}/v/p/');
ATTACH 'ducklake:${WORK}/v/q.ducklake' AS q (DATA_PATH '${WORK}/v/q/');
ATTACH 'ducklake:${WORK}/v/v.ducklake' AS v (DATA_PATH '${WORK}/v/v/');
CALL m.set_option('parquet_compression','zstd');
CALL p.set_option('parquet_compression','zstd');
CALL q.set_option('parquet_compression','zstd');
CALL v.set_option('parquet_compression','zstd');
-- m: today's table, MAP only
CREATE TABLE m.spans PARTITIONED BY (epoch_day(start_time)) SORTED BY (date_trunc('hour', start_time), service_name, start_time) AS SELECT * FROM src.otel.spans LIMIT 0;
INSERT INTO m.spans SELECT * FROM src.otel.spans;
-- p: MAP plus three promoted columns (NULL = absent)
CREATE TABLE p.spans PARTITIONED BY (epoch_day(start_time)) SORTED BY (date_trunc('hour', start_time), service_name, start_time) AS SELECT *, attributes['http.route'] AS http_route, TRY_CAST(attributes['http.response.status_code'] AS SMALLINT) AS http_status_code, attributes['db.system.name'] AS db_system_name FROM src.otel.spans LIMIT 0;
INSERT INTO p.spans SELECT *, attributes['http.route'], TRY_CAST(attributes['http.response.status_code'] AS SMALLINT), attributes['db.system.name'] FROM src.otel.spans;
-- q: MAP plus http_route with '' = absent, so NULL can mean "row from before the column"
CREATE TABLE q.spans PARTITIONED BY (epoch_day(start_time)) SORTED BY (date_trunc('hour', start_time), service_name, start_time) AS SELECT *, coalesce(attributes['http.route'],'') AS http_route FROM src.otel.spans LIMIT 0;
INSERT INTO q.spans SELECT *, coalesce(attributes['http.route'],'') FROM src.otel.spans;
-- v: attributes as VARIANT (shredded by the Parquet writer), status code typed
CREATE TABLE v.spans PARTITIONED BY (epoch_day(start_time)) SORTED BY (date_trunc('hour', start_time), service_name, start_time) AS SELECT * EXCLUDE (attributes), CAST(NULL AS VARIANT) AS attributes FROM src.otel.spans LIMIT 0;
INSERT INTO v.spans SELECT * EXCLUDE (attributes), regexp_replace(to_json(attributes)::VARCHAR, '"http.response.status_code":"([0-9]+)"', '"http.response.status_code":\1')::JSON::VARIANT FROM src.otel.spans;
SELECT 'm' l, round(sum(file_size_bytes)/1e6,1) mb FROM __ducklake_metadata_m.ducklake_data_file
UNION ALL SELECT 'p', round(sum(file_size_bytes)/1e6,1) FROM __ducklake_metadata_p.ducklake_data_file
UNION ALL SELECT 'q', round(sum(file_size_bytes)/1e6,1) FROM __ducklake_metadata_q.ducklake_data_file
UNION ALL SELECT 'v', round(sum(file_size_bytes)/1e6,1) FROM __ducklake_metadata_v.ducklake_data_file;
