#!/usr/bin/env python3
"""Fill a mallard server with realistic, linked demo data.

A small shop: browser -> checkout-api -> payments / inventory -> postgres.
Spread over the last hour: OTLP traces, logs and metrics, plus Sentry error
events that share trace and span ids with the spans they happened in.

    MALLARD_URL=http://localhost:8080 MALLARD_TOKEN=dev-token scripts/demo-seed.py
    scripts/demo-seed.py --url http://localhost:8080 --token dev-token --otlp http://localhost:4318

Standard library only. Used for README screenshots and to try mallard out.
"""

import argparse
import json
import os
import random
import time
import urllib.error
import urllib.request
import uuid

NOW_NS = time.time_ns()
HOUR_NS = 3600 * 10**9


def post(url, body, headers=None):
    data = body if isinstance(body, bytes) else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method="POST", headers={"content-type": "application/json", **(headers or {})})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read() or b"null")


def get(url, token):
    req = urllib.request.Request(url, headers={"authorization": f"Bearer {token}"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def project(api, token, name, platform):
    for p in get(f"{api}/sentry/v1/projects", token):
        if p["slug"] == name:
            return p
    return post(f"{api}/sentry/v1/projects", {"name": name, "platform": platform}, {"authorization": f"Bearer {token}"})


def attrs(d):
    out = []
    for k, v in d.items():
        if isinstance(v, bool):
            val = {"boolValue": v}
        elif isinstance(v, int):
            val = {"intValue": str(v)}
        elif isinstance(v, float):
            val = {"doubleValue": v}
        else:
            val = {"stringValue": str(v)}
        out.append({"key": k, "value": val})
    return out


def resource(service, version):
    return {"attributes": attrs({"service.name": service, "service.version": version, "deployment.environment": "production"})}


ERRORS = {
    "card_declined": dict(
        project="api", type="PaymentDeclinedError", value="card declined: insufficient funds",
        frames=[("app/payments/stripe.py", "charge", 88), ("app/payments/service.py", "capture_payment", 41),
                ("app/orders/checkout.py", "place_order", 132)], platform="python"),
    "timeout": dict(
        project="api", type="UpstreamTimeout", value="inventory did not answer within 2000ms",
        frames=[("app/clients/inventory.py", "reserve", 57), ("app/orders/checkout.py", "place_order", 119)],
        platform="python"),
    "keyerror": dict(
        project="api", type="KeyError", value="'sku'",
        frames=[("app/inventory/stock.py", "reserve_items", 23), ("app/inventory/api.py", "reserve", 64)],
        platform="python"),
    "frontend": dict(
        project="web", type="TypeError", value="Cannot read properties of undefined (reading 'total')",
        frames=[("src/components/CartSummary.tsx", "CartSummary", 42), ("src/pages/Checkout.tsx", "submitOrder", 87)],
        platform="javascript"),
}


def sentry_event(kind, trace_id, span_id, ts_ns, release, user):
    e = ERRORS[kind]
    frames = [{"filename": f, "function": fn, "lineno": ln, "in_app": True} for f, fn, ln in reversed(e["frames"])]
    return e["project"], {
        "event_id": uuid.uuid4().hex,
        "timestamp": ts_ns / 1e9,
        "platform": e["platform"],
        "level": "error",
        "release": release,
        "environment": "production",
        "user": {"id": user, "ip_address": "{{auto}}"},
        "tags": {"region": random.choice(["eu-west-1", "us-east-1"])},
        "exception": {"values": [{"type": e["type"], "value": e["value"], "stacktrace": {"frames": frames}}]},
        "contexts": {"trace": {"trace_id": trace_id, "span_id": span_id, "op": "http.server"}},
    }


def span(trace_id, span_id, parent, name, kind, start, dur, error=False, **a):
    s = {"traceId": trace_id, "spanId": span_id, "name": name, "kind": kind,
         "startTimeUnixNano": str(start), "endTimeUnixNano": str(start + dur),
         "attributes": attrs(a), "status": {"code": 2 if error else 1}}
    if parent:
        s["parentSpanId"] = parent
    return s


def log(trace_id, span_id, ts, sev, text, body, **a):
    return {"timeUnixNano": str(ts), "severityNumber": sev, "severityText": text, "body": {"stringValue": body},
            "traceId": trace_id, "spanId": span_id, "attributes": attrs(a)}


def sid():
    return uuid.uuid4().hex[:16]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default=os.environ.get("MALLARD_URL", "http://localhost:8080"))
    ap.add_argument("--token", default=os.environ.get("MALLARD_TOKEN", "dev-token"))
    ap.add_argument("--otlp", default=None, help="OTLP/HTTP base URL (default: --url)")
    ap.add_argument("--traces", type=int, default=400)
    ap.add_argument("--seed", type=int, default=7)
    a = ap.parse_args()
    random.seed(a.seed)
    otlp = (a.otlp or a.url).rstrip("/")
    api = a.url.rstrip("/")

    keys = {name: project(api, a.token, name, plat) for name, plat in [("web", "javascript"), ("api", "python")]}
    spans_by_service, logs_by_service, events = {}, {}, []

    def add_span(service, s):
        spans_by_service.setdefault(service, []).append(s)

    def add_log(service, l):
        logs_by_service.setdefault(service, []).append(l)

    for i in range(a.traces):
        # Busier towards "now", with a dip in the middle.
        t0 = NOW_NS - int(HOUR_NS * (random.random() ** 1.3))
        trace = uuid.uuid4().hex
        release = "shop@2.4.0" if t0 > NOW_NS - HOUR_NS // 3 else "shop@2.3.1"
        new_release = release == "shop@2.4.0"
        user = f"user-{random.randint(1, 180)}"
        root, api_span, pay, inv, db1, db2 = sid(), sid(), sid(), sid(), sid(), sid()

        inv_slow = random.random() < (0.09 if new_release else 0.02)
        inv_dur = random.randint(1_900, 2_100) * 10**6 if inv_slow else random.randint(8, 40) * 10**6
        pay_dur = random.randint(60, 240) * 10**6
        declined = not inv_slow and random.random() < 0.07
        keyerr = not inv_slow and random.random() < (0.04 if new_release else 0.0)
        api_err = inv_slow or declined or keyerr
        api_dur = inv_dur + pay_dur + random.randint(5, 20) * 10**6
        root_dur = api_dur + random.randint(20, 80) * 10**6
        fe_err = api_err and random.random() < 0.5

        add_span("web", span(trace, root, None, "POST /checkout", 3, t0, root_dur, fe_err,
                             **{"http.method": "POST", "http.route": "/checkout", "http.status_code": 500 if api_err else 200}))
        a0 = t0 + 10 * 10**6
        add_span("checkout-api", span(trace, api_span, root, "POST /api/orders", 2, a0, api_dur, api_err,
                                      **{"http.method": "POST", "http.route": "/api/orders", "http.status_code": 500 if api_err else 201,
                                         "enduser.id": user}))
        add_span("inventory", span(trace, inv, api_span, "reserve items", 2, a0 + 2 * 10**6, inv_dur, inv_slow or keyerr,
                                   **{"items": random.randint(1, 6)}))
        add_span("postgres", span(trace, db1, inv, "UPDATE stock", 3, a0 + 4 * 10**6, min(inv_dur - 3 * 10**6, random.randint(3, 15) * 10**6),
                                  **{"db.system": "postgresql", "db.statement": "UPDATE stock SET reserved = reserved + $1 WHERE sku = $2"}))
        p0 = a0 + inv_dur + 3 * 10**6
        add_span("payments", span(trace, pay, api_span, "charge card", 3, p0, pay_dur, declined,
                                  **{"payment.provider": "stripe", "payment.amount": round(random.uniform(9, 240), 2)}))
        add_span("postgres", span(trace, db2, api_span, "INSERT orders", 3, p0 + pay_dur + 1 * 10**6, random.randint(2, 9) * 10**6,
                                  **{"db.system": "postgresql", "db.statement": "INSERT INTO orders (user_id, total) VALUES ($1, $2)"}))

        add_log("checkout-api", log(trace, api_span, a0 + 10**6, 9, "INFO", f"placing order for {user}", user=user, release=release))
        if inv_slow:
            add_log("checkout-api", log(trace, api_span, a0 + inv_dur, 17, "ERROR", "inventory did not answer within 2000ms", upstream="inventory"))
            events.append(sentry_event("timeout", trace, api_span, a0 + inv_dur, release, user))
        if keyerr:
            add_log("inventory", log(trace, inv, a0 + inv_dur, 17, "ERROR", "KeyError: 'sku' while reserving items"))
            events.append(sentry_event("keyerror", trace, inv, a0 + inv_dur, release, user))
        if declined:
            add_log("payments", log(trace, pay, p0 + pay_dur, 13, "WARN", "card declined: insufficient funds", provider="stripe"))
            events.append(sentry_event("card_declined", trace, pay, p0 + pay_dur, release, user))
        if not api_err:
            add_log("checkout-api", log(trace, api_span, a0 + api_dur, 9, "INFO", "order placed", user=user))
        if fe_err:
            events.append(sentry_event("frontend", trace, root, t0 + root_dur, release, user))

    for service, spans in spans_by_service.items():
        post(f"{otlp}/v1/traces", {"resourceSpans": [{"resource": resource(service, "2.4.0"), "scopeSpans": [{"scope": {"name": "demo"}, "spans": spans}]}]})
    for service, logs in logs_by_service.items():
        post(f"{otlp}/v1/logs", {"resourceLogs": [{"resource": resource(service, "2.4.0"), "scopeLogs": [{"scope": {"name": "demo"}, "logRecords": logs}]}]})

    # Metrics: requests per minute and memory, one point per minute.
    for service in ["checkout-api", "inventory", "payments"]:
        req_points, mem_points, total = [], [], 0
        for m in range(60, -1, -1):
            ts = NOW_NS - m * 60 * 10**9
            total += random.randint(40, 90) if service == "checkout-api" else random.randint(20, 60)
            req_points.append({"startTimeUnixNano": str(NOW_NS - HOUR_NS), "timeUnixNano": str(ts), "asInt": str(total),
                               "attributes": attrs({"http.route": "/api/orders"})})
            mem_points.append({"timeUnixNano": str(ts), "asDouble": 180e6 + 40e6 * random.random() + (60 - m) * 1.2e6})
        post(f"{otlp}/v1/metrics", {"resourceMetrics": [{"resource": resource(service, "2.4.0"), "scopeMetrics": [{"scope": {"name": "demo"}, "metrics": [
            {"name": "http.server.requests", "unit": "1", "description": "Handled requests",
             "sum": {"aggregationTemporality": 2, "isMonotonic": True, "dataPoints": req_points}},
            {"name": "process.memory.usage", "unit": "By", "description": "Resident memory",
             "gauge": {"dataPoints": mem_points}},
        ]}]}]})

    for proj, ev in events:
        p = keys[proj]
        body = "\n".join([json.dumps({"event_id": ev["event_id"]}), json.dumps({"type": "event"}), json.dumps(ev)]) + "\n"
        try:
            post(f"{api}/sentry/api/{p['id']}/envelope/", body.encode(),
                 {"x-sentry-auth": f"Sentry sentry_version=7, sentry_key={p['public_key']}", "content-type": "application/x-sentry-envelope"})
        except urllib.error.HTTPError as e:
            print("event rejected:", e.code, e.read()[:200])

    n_spans = sum(len(v) for v in spans_by_service.values())
    n_logs = sum(len(v) for v in logs_by_service.values())
    print(f"seeded {a.traces} traces ({n_spans} spans), {n_logs} logs, {len(events)} error events, 6 metric series")


if __name__ == "__main__":
    main()
