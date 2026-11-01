LOAD ducklake;
SET threads=4;
ATTACH 'ducklake:${WORK}/bench/catalog.ducklake' AS src (DATA_PATH '${WORK}/bench/lake/', READ_ONLY);
ATTACH 'ducklake:${WORK}/sx/a.ducklake' AS a (DATA_PATH '${WORK}/sx/a/');
ATTACH 'ducklake:${WORK}/sx/b.ducklake' AS b (DATA_PATH '${WORK}/sx/b/');
ATTACH 'ducklake:${WORK}/sx/c.ducklake' AS c (DATA_PATH '${WORK}/sx/c/');
CALL a.set_option('parquet_compression','zstd'); CALL b.set_option('parquet_compression','zstd'); CALL c.set_option('parquet_compression','zstd');
CREATE TABLE a.spans PARTITIONED BY (epoch_day(start_time)) SORTED BY (start_time) AS SELECT * FROM src.otel.spans LIMIT 0;
CREATE TABLE b.spans PARTITIONED BY (epoch_day(start_time)) SORTED BY (service_name, start_time) AS SELECT * FROM src.otel.spans LIMIT 0;
CREATE TABLE c.spans PARTITIONED BY (epoch_day(start_time)) SORTED BY (date_trunc('hour', start_time), service_name, start_time) AS SELECT * FROM src.otel.spans LIMIT 0;
INSERT INTO a.spans SELECT * FROM src.otel.spans;
INSERT INTO b.spans SELECT * FROM src.otel.spans;
INSERT INTO c.spans SELECT * FROM src.otel.spans;
SELECT 'a' l, count(*) files, round(sum(file_size_bytes)/1e6,1) mb FROM __ducklake_metadata_a.ducklake_data_file
UNION ALL SELECT 'b', count(*), round(sum(file_size_bytes)/1e6,1) FROM __ducklake_metadata_b.ducklake_data_file
UNION ALL SELECT 'c', count(*), round(sum(file_size_bytes)/1e6,1) FROM __ducklake_metadata_c.ducklake_data_file;
