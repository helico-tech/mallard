import subprocess, time, statistics, sys, json, os
S=sys.argv[1]
def run(lake, sql, n=5):
    script=f"""LOAD ducklake; SET threads=4;
ATTACH 'ducklake:{S}/sx/{lake}.ducklake' AS l (DATA_PATH '{S}/sx/{lake}/', READ_ONLY);
USE l;
.timer on
""" + "\n".join([sql]*n)
    out=subprocess.run([f"{S}/cli/duckdb","-no-agent","-noheader","-list"],input=script,capture_output=True,text=True)
    if "Run Time" not in out.stdout: print(out.stdout[-500:], out.stderr[-500:])
    out=out.stdout
    ts=[float(l.split("real")[1].split()[0]) for l in out.splitlines() if l.startswith("Run Time")]
    return statistics.median(ts)*1000
end=subprocess.run([f"{S}/cli/duckdb","-no-agent","-noheader","-list"],input=f"LOAD ducklake; ATTACH 'ducklake:{S}/sx/a.ducklake' AS l (DATA_PATH '{S}/sx/a/', READ_ONLY); SELECT max(start_time) FROM l.spans;",capture_output=True,text=True).stdout.strip()
tid=subprocess.run([f"{S}/cli/duckdb","-no-agent","-noheader","-list"],input=f"LOAD ducklake; ATTACH 'ducklake:{S}/sx/a.ducklake' AS l (DATA_PATH '{S}/sx/a/', READ_ONLY); SELECT trace_id FROM l.spans WHERE start_time > TIMESTAMP '{end}' - INTERVAL 30 MINUTE LIMIT 1;",capture_output=True,text=True).stdout.strip()
E=f"TIMESTAMP '{end}'"
Q={
 "RED by service, last 15m": f"SELECT service_name, count(*), approx_quantile(duration_ns,0.95) FROM spans WHERE start_time >= {E} - INTERVAL 15 MINUTE GROUP BY 1;",
 "RED by service, 2h": f"SELECT service_name, count(*), approx_quantile(duration_ns,0.95) FROM spans WHERE start_time >= {E} - INTERVAL 2 HOUR GROUP BY 1;",
 "checkout errors, last 15m": f"SELECT trace_id FROM spans WHERE start_time >= {E} - INTERVAL 15 MINUTE AND service_name='checkout' AND status_code='ERROR' LIMIT 50;",
 "one service series, 2h": f"SELECT epoch(start_time)//60, count(*) FROM spans WHERE start_time >= {E} - INTERVAL 2 HOUR AND service_name='payment' GROUP BY 1;",
 "trace by id, 2h": f"SELECT * FROM spans WHERE trace_id = '{tid}'::UUID AND start_time >= {E} - INTERVAL 2 HOUR;",
 "attr filter, 1h": f"SELECT count(*) FROM spans WHERE start_time >= {E} - INTERVAL 1 HOUR AND attributes['http.route'] = '/checkout';",
}
print(f"{'query':32} {'a: time':>10} {'b: svc,time':>12} {'c: hour,svc,time':>17}")
for name,sql in Q.items():
    r=[run(l,sql) for l in "abc"]
    print(f"{name:32} {r[0]:>8.1f}ms {r[1]:>10.1f}ms {r[2]:>15.1f}ms")
