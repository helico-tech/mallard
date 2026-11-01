-- Timed queries (see README.md). Replace ${WORK} and ${END} (max(start_time) of the data).
-- In the DuckDB CLI: .timer on, run each a few times, take the median.
LOAD ducklake;
LOAD json;
SET threads=4;
ATTACH 'ducklake:${WORK}/v/m.ducklake' AS m (DATA_PATH '${WORK}/v/m/', READ_ONLY);
ATTACH 'ducklake:${WORK}/v/p.ducklake' AS p (DATA_PATH '${WORK}/v/p/', READ_ONLY);
ATTACH 'ducklake:${WORK}/v/q.ducklake' AS q (DATA_PATH '${WORK}/v/q/', READ_ONLY);
ATTACH 'ducklake:${WORK}/v/v.ducklake' AS v (DATA_PATH '${WORK}/v/v/', READ_ONLY);
-- bench: A trace search route=/checkout 1h map
SELECT trace_id, max(start_time) AS last FROM m.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND start_time <= TIMESTAMP '${END}' AND (attributes['http.route'] = '/checkout' OR resource_attributes['http.route'] = '/checkout') GROUP BY trace_id ORDER BY last DESC LIMIT 20;
-- bench: A trace search route=/checkout 1h variant
SELECT trace_id, max(start_time) AS last FROM v.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND start_time <= TIMESTAMP '${END}' AND (variant_extract(attributes,'http.route')::VARCHAR = '/checkout' OR resource_attributes['http.route'] = '/checkout') GROUP BY trace_id ORDER BY last DESC LIMIT 20;
-- bench: A trace search route=/checkout 1h column
SELECT trace_id, max(start_time) AS last FROM p.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND start_time <= TIMESTAMP '${END}' AND http_route = '/checkout' GROUP BY trace_id ORDER BY last DESC LIMIT 20;
-- bench: B same, service=frontend map
SELECT trace_id, max(start_time) AS last FROM m.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND start_time <= TIMESTAMP '${END}' AND service_name='frontend' AND (attributes['http.route'] = '/checkout' OR resource_attributes['http.route'] = '/checkout') GROUP BY trace_id ORDER BY last DESC LIMIT 20;
-- bench: B same, service=frontend variant
SELECT trace_id, max(start_time) AS last FROM v.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND start_time <= TIMESTAMP '${END}' AND service_name='frontend' AND (variant_extract(attributes,'http.route')::VARCHAR = '/checkout' OR resource_attributes['http.route'] = '/checkout') GROUP BY trace_id ORDER BY last DESC LIMIT 20;
-- bench: B same, service=frontend column
SELECT trace_id, max(start_time) AS last FROM p.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND start_time <= TIMESTAMP '${END}' AND service_name='frontend' AND http_route = '/checkout' GROUP BY trace_id ORDER BY last DESC LIMIT 20;
-- bench: C count status>=500 1h map
SELECT count(*) FROM m.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND start_time <= TIMESTAMP '${END}' AND TRY_CAST(attributes['http.response.status_code'] AS INTEGER) >= 500;
-- bench: C count status>=500 1h variant
SELECT count(*) FROM v.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND start_time <= TIMESTAMP '${END}' AND variant_extract(attributes,'http.response.status_code')::INTEGER >= 500;
-- bench: C count status>=500 1h column
SELECT count(*) FROM p.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND start_time <= TIMESTAMP '${END}' AND http_status_code >= 500;
-- bench: D p95 by route 2h map
SELECT attributes['http.route'], approx_quantile(duration_ns,0.95) FROM m.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 2 HOUR AND start_time <= TIMESTAMP '${END}' GROUP BY 1;
-- bench: D p95 by route 2h variant
SELECT variant_extract(attributes,'http.route')::VARCHAR, approx_quantile(duration_ns,0.95) FROM v.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 2 HOUR AND start_time <= TIMESTAMP '${END}' GROUP BY 1;
-- bench: D p95 by route 2h column
SELECT http_route, approx_quantile(duration_ns,0.95) FROM p.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 2 HOUR AND start_time <= TIMESTAMP '${END}' GROUP BY 1;
-- bench: E spans by db.system.name 2h map
SELECT attributes['db.system.name'], count(*) FROM m.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 2 HOUR AND start_time <= TIMESTAMP '${END}' GROUP BY 1;
-- bench: E spans by db.system.name 2h variant
SELECT variant_extract(attributes,'db.system.name')::VARCHAR, count(*) FROM v.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 2 HOUR AND start_time <= TIMESTAMP '${END}' GROUP BY 1;
-- bench: E spans by db.system.name 2h column
SELECT db_system_name, count(*) FROM p.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 2 HOUR AND start_time <= TIMESTAMP '${END}' GROUP BY 1;
-- bench: F no attributes: RED by service 2h map
SELECT service_name, count(*), approx_quantile(duration_ns,0.95) FROM m.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 2 HOUR AND start_time <= TIMESTAMP '${END}' GROUP BY 1;
-- bench: F no attributes: RED by service 2h variant
SELECT service_name, count(*), approx_quantile(duration_ns,0.95) FROM v.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 2 HOUR AND start_time <= TIMESTAMP '${END}' GROUP BY 1;
-- bench: F no attributes: RED by service 2h column
SELECT service_name, count(*), approx_quantile(duration_ns,0.95) FROM p.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 2 HOUR AND start_time <= TIMESTAMP '${END}' GROUP BY 1;
-- bench: 1 map, API form (OR resource)
SELECT count(*) FROM m.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND (attributes['http.route'] = '/checkout' OR resource_attributes['http.route'] = '/checkout');
-- bench: 2 map, without the OR on resource
SELECT count(*) FROM m.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND attributes['http.route'] = '/checkout';
-- bench: 3 column only (ignores old rows and resource)
SELECT count(*) FROM p.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND http_route = '/checkout';
-- bench: 4 column OR resource map
SELECT count(*) FROM p.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND (http_route = '/checkout' OR resource_attributes['http.route'] = '/checkout');
-- bench: 5 column, NULL=absent, fallback to map for old rows
SELECT count(*) FROM p.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND (http_route = '/checkout' OR (http_route IS NULL AND attributes['http.route'] = '/checkout'));
-- bench: 6 column, ''=absent, fallback to map for old rows
SELECT count(*) FROM q.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND (http_route = '/checkout' OR (http_route IS NULL AND attributes['http.route'] = '/checkout'));
-- bench: 7 column, ''=absent, fallback + resource (full API semantics)
SELECT count(*) FROM q.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND (http_route = '/checkout' OR (http_route IS NULL AND attributes['http.route'] = '/checkout') OR resource_attributes['http.route'] = '/checkout');
-- bench: 8 variant_extract
SELECT count(*) FROM v.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND variant_extract(attributes,'http.route')::VARCHAR = '/checkout';
-- bench: 9 variant dotted access
SELECT count(*) FROM v.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND attributes."http.route"::VARCHAR = '/checkout';
-- bench: 10 variant_extract_string
SELECT count(*) FROM v.spans WHERE start_time >= TIMESTAMP '${END}' - INTERVAL 1 HOUR AND variant_extract_string(attributes,'http.route') = '/checkout';
