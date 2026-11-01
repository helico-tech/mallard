//! OTLP protobuf -> Arrow record batches, one batch per lake table.
//!
//! Column order must match `store::schema`. Conversion rules (also in
//! docs/data-model.md):
//! - Timestamps: unix nanos -> microseconds. A zero timestamp falls back to the
//!   observed time (logs) or the receive time.
//! - Trace ids -> UUID (16 bytes), span ids -> UBIGINT (big-endian u64).
//!   Invalid (wrong length / all zero) ids become NULL; spans without a valid
//!   trace or span id are dropped and counted as rejected.
//! - Attribute values are stringified: strings verbatim, numbers/bools via
//!   Display, bytes as base64, arrays and maps as JSON. Duplicate keys keep the
//!   last value.

use std::{collections::BTreeMap, collections::HashMap, sync::Arc};

use arrow::{
    array::*,
    buffer::OffsetBuffer,
    datatypes::{DataType, Field, Fields, Schema, TimeUnit},
    record_batch::RecordBatch,
};
use base64::Engine as _;
use opentelemetry_proto::tonic::{
    collector::{
        logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
        trace::v1::ExportTraceServiceRequest,
    },
    common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value},
    metrics::v1::{metric::Data, number_data_point},
    resource::v1::Resource,
};
use xxhash_rust::xxh3::Xxh3;

use crate::store::schema::TableId;

/// Result of converting one OTLP request.
#[derive(Default)]
pub struct Converted {
    pub batches: Vec<(TableId, RecordBatch)>,
    pub accepted: u64,
    pub rejected: u64,
    pub reject_reason: Option<String>,
}

type Attrs = Vec<(String, String)>;

pub const UNKNOWN_SERVICE: &str = "unknown_service";

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

pub fn any_value_to_string(v: &AnyValue) -> String {
    match &v.value {
        None => String::new(),
        Some(any_value::Value::StringValue(s)) => s.clone(),
        Some(any_value::Value::BoolValue(b)) => b.to_string(),
        Some(any_value::Value::IntValue(i)) => i.to_string(),
        Some(any_value::Value::DoubleValue(d)) => d.to_string(),
        Some(any_value::Value::BytesValue(b)) => {
            base64::engine::general_purpose::STANDARD.encode(b)
        }
        Some(_) => any_value_to_json(v).to_string(),
    }
}

fn any_value_to_json(v: &AnyValue) -> serde_json::Value {
    use serde_json::Value as J;
    match &v.value {
        None => J::Null,
        Some(any_value::Value::StringValue(s)) => J::String(s.clone()),
        Some(any_value::Value::BoolValue(b)) => J::Bool(*b),
        Some(any_value::Value::IntValue(i)) => J::from(*i),
        Some(any_value::Value::DoubleValue(d)) => serde_json::Number::from_f64(*d)
            .map(J::Number)
            .unwrap_or(J::Null),
        Some(any_value::Value::BytesValue(b)) => {
            J::String(base64::engine::general_purpose::STANDARD.encode(b))
        }
        Some(any_value::Value::ArrayValue(a)) => {
            J::Array(a.values.iter().map(any_value_to_json).collect())
        }
        Some(any_value::Value::KvlistValue(kv)) => J::Object(
            kv.values
                .iter()
                .map(|e| {
                    (
                        e.key.clone(),
                        e.value.as_ref().map(any_value_to_json).unwrap_or(J::Null),
                    )
                })
                .collect(),
        ),
        // Newer proto versions may add variants; render them as null.
        #[allow(unreachable_patterns)]
        Some(_) => J::Null,
    }
}

/// Deduplicated (last wins), key-sorted attributes.
fn attrs(kvs: &[KeyValue]) -> Attrs {
    if kvs.is_empty() {
        return Vec::new();
    }
    let mut m: BTreeMap<&str, String> = BTreeMap::new();
    for kv in kvs {
        m.insert(
            kv.key.as_str(),
            kv.value
                .as_ref()
                .map(any_value_to_string)
                .unwrap_or_default(),
        );
    }
    m.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn nanos_to_micros(n: u64) -> i64 {
    (n / 1000).min(i64::MAX as u64) as i64
}

fn now_micros() -> i64 {
    chrono::Utc::now().timestamp_micros()
}

fn trace_id(b: &[u8]) -> Option<[u8; 16]> {
    let a: [u8; 16] = b.try_into().ok()?;
    (a != [0; 16]).then_some(a)
}

fn span_id(b: &[u8]) -> Option<u64> {
    let a: [u8; 8] = b.try_into().ok()?;
    let v = u64::from_be_bytes(a);
    (v != 0).then_some(v)
}

struct ResourceCtx {
    service: String,
    attrs: Attrs,
}

impl ResourceCtx {
    fn new(r: Option<&Resource>) -> Self {
        let attrs = r.map(|r| attrs(&r.attributes)).unwrap_or_default();
        let service = attrs
            .iter()
            .find(|(k, _)| k == "service.name")
            .map(|(_, v)| v.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| UNKNOWN_SERVICE.to_string());
        Self { service, attrs }
    }
}

fn scope_parts(s: Option<&InstrumentationScope>) -> (&str, &str) {
    s.map(|s| (s.name.as_str(), s.version.as_str()))
        .unwrap_or(("", ""))
}

// ---------------------------------------------------------------------------
// Column builders
// ---------------------------------------------------------------------------

fn uuid_field(name: &str) -> Field {
    let mut md = HashMap::new();
    md.insert("ARROW:extension:name".to_string(), "arrow.uuid".to_string());
    Field::new(name, DataType::FixedSizeBinary(16), true).with_metadata(md)
}

fn ts_type() -> DataType {
    DataType::Timestamp(TimeUnit::Microsecond, None)
}

/// MAP(VARCHAR, VARCHAR) column.
struct AttrCol(MapBuilder<StringBuilder, StringBuilder>);

impl AttrCol {
    fn new() -> Self {
        Self(MapBuilder::new(
            None,
            StringBuilder::new(),
            StringBuilder::new(),
        ))
    }
    fn append(&mut self, a: &[(String, String)]) {
        for (k, v) in a {
            self.0.keys().append_value(k);
            self.0.values().append_value(v);
        }
        self.0.append(true).expect("map append");
    }
    fn finish(&mut self) -> ArrayRef {
        Arc::new(self.0.finish())
    }
}

/// Builds a `STRUCT(...)[]` column from per-row child values.
struct ListOffsets(Vec<i32>);

impl ListOffsets {
    fn new() -> Self {
        Self(vec![0])
    }
    fn close_row(&mut self, len: usize) {
        let last = *self.0.last().unwrap();
        self.0.push(last + len as i32);
    }
    fn finish(&mut self, struct_fields: Fields, children: Vec<ArrayRef>) -> ArrayRef {
        let offsets = std::mem::replace(&mut self.0, vec![0]);
        let st = StructArray::new(struct_fields.clone(), children, None);
        let item = Arc::new(Field::new("item", DataType::Struct(struct_fields), true));
        Arc::new(ListArray::new(
            item,
            OffsetBuffer::new(offsets.into()),
            Arc::new(st),
            None,
        ))
    }
}

fn batch(cols: Vec<(Field, ArrayRef)>) -> RecordBatch {
    let (fields, arrays): (Vec<Field>, Vec<ArrayRef>) = cols.into_iter().unzip();
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).expect("valid batch")
}

fn f(name: &str, dt: DataType) -> Field {
    Field::new(name, dt, true)
}

fn map_field(name: &str, a: &ArrayRef) -> Field {
    f(name, a.data_type().clone())
}

// ---------------------------------------------------------------------------
// Traces
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SpanCols {
    start: TimestampMicrosecondBuilder,
    duration: Int64Builder,
    trace_id: Vec<[u8; 16]>,
    span_id: UInt64Builder,
    parent: UInt64Builder,
    service: StringBuilder,
    name: StringBuilder,
    kind: StringBuilder,
    status_code: StringBuilder,
    status_message: StringBuilder,
    trace_state: StringBuilder,
    flags: UInt32Builder,
    scope_name: StringBuilder,
    scope_version: StringBuilder,
    res_attrs: Option<AttrCol>,
    attrs: Option<AttrCol>,
    ev_offsets: Option<ListOffsets>,
    ev_time: TimestampMicrosecondBuilder,
    ev_name: StringBuilder,
    ev_attrs: Option<AttrCol>,
    ln_offsets: Option<ListOffsets>,
    ln_trace: Vec<Option<[u8; 16]>>,
    ln_span: UInt64Builder,
    ln_state: StringBuilder,
    ln_attrs: Option<AttrCol>,
}

pub fn span_kind(k: i32) -> &'static str {
    match k {
        1 => "INTERNAL",
        2 => "SERVER",
        3 => "CLIENT",
        4 => "PRODUCER",
        5 => "CONSUMER",
        _ => "UNSPECIFIED",
    }
}

fn status_code(c: i32) -> &'static str {
    match c {
        1 => "OK",
        2 => "ERROR",
        _ => "UNSET",
    }
}

fn fsb(vals: &[[u8; 16]]) -> ArrayRef {
    Arc::new(
        FixedSizeBinaryArray::try_from_iter(vals.iter()).unwrap_or_else(|_| {
            FixedSizeBinaryArray::new_null(16, 0) // only reachable for empty input
        }),
    )
}

fn fsb_opt(vals: &[Option<[u8; 16]>]) -> ArrayRef {
    if vals.is_empty() {
        return Arc::new(FixedSizeBinaryArray::new_null(16, 0));
    }
    if vals.iter().all(Option::is_none) {
        return Arc::new(FixedSizeBinaryArray::new_null(16, vals.len()));
    }
    Arc::new(
        FixedSizeBinaryArray::try_from_sparse_iter_with_size(vals.iter().map(|v| v.as_ref()), 16)
            .expect("16 byte ids"),
    )
}

pub fn convert_traces(req: &ExportTraceServiceRequest) -> Converted {
    let mut c = SpanCols {
        res_attrs: Some(AttrCol::new()),
        attrs: Some(AttrCol::new()),
        ev_offsets: Some(ListOffsets::new()),
        ev_attrs: Some(AttrCol::new()),
        ln_offsets: Some(ListOffsets::new()),
        ln_attrs: Some(AttrCol::new()),
        ..Default::default()
    };
    let (mut accepted, mut rejected) = (0u64, 0u64);
    let recv = now_micros();
    for rs in &req.resource_spans {
        let res = ResourceCtx::new(rs.resource.as_ref());
        for ss in &rs.scope_spans {
            let (scope_name, scope_version) = scope_parts(ss.scope.as_ref());
            for s in &ss.spans {
                let (Some(tid), Some(sid)) = (trace_id(&s.trace_id), span_id(&s.span_id)) else {
                    rejected += 1;
                    continue;
                };
                accepted += 1;
                let start = if s.start_time_unix_nano == 0 {
                    recv
                } else {
                    nanos_to_micros(s.start_time_unix_nano)
                };
                let dur = if s.end_time_unix_nano >= s.start_time_unix_nano
                    && s.start_time_unix_nano != 0
                {
                    (s.end_time_unix_nano - s.start_time_unix_nano).min(i64::MAX as u64) as i64
                } else {
                    0
                };
                c.start.append_value(start);
                c.duration.append_value(dur);
                c.trace_id.push(tid);
                c.span_id.append_value(sid);
                c.parent.append_option(span_id(&s.parent_span_id));
                c.service.append_value(&res.service);
                c.name.append_value(&s.name);
                c.kind.append_value(span_kind(s.kind));
                let (code, msg) = s
                    .status
                    .as_ref()
                    .map(|st| (st.code, st.message.as_str()))
                    .unwrap_or((0, ""));
                c.status_code.append_value(status_code(code));
                c.status_message.append_value(msg);
                c.trace_state.append_value(&s.trace_state);
                c.flags.append_value(s.flags);
                c.scope_name.append_value(scope_name);
                c.scope_version.append_value(scope_version);
                c.res_attrs.as_mut().unwrap().append(&res.attrs);
                c.attrs.as_mut().unwrap().append(&attrs(&s.attributes));
                for e in &s.events {
                    c.ev_time.append_value(if e.time_unix_nano == 0 {
                        start
                    } else {
                        nanos_to_micros(e.time_unix_nano)
                    });
                    c.ev_name.append_value(&e.name);
                    c.ev_attrs.as_mut().unwrap().append(&attrs(&e.attributes));
                }
                c.ev_offsets.as_mut().unwrap().close_row(s.events.len());
                for l in &s.links {
                    c.ln_trace.push(trace_id(&l.trace_id));
                    c.ln_span.append_option(span_id(&l.span_id));
                    c.ln_state.append_value(&l.trace_state);
                    c.ln_attrs.as_mut().unwrap().append(&attrs(&l.attributes));
                }
                c.ln_offsets.as_mut().unwrap().close_row(s.links.len());
            }
        }
    }
    let mut out = Converted {
        accepted,
        rejected,
        ..Default::default()
    };
    if rejected > 0 {
        out.reject_reason = Some(format!(
            "{rejected} spans had an invalid trace_id or span_id"
        ));
    }
    if accepted == 0 {
        return out;
    }
    let res_attrs = c.res_attrs.as_mut().unwrap().finish();
    let span_attrs = c.attrs.as_mut().unwrap().finish();
    let ev_attrs = c.ev_attrs.as_mut().unwrap().finish();
    let ev_fields = Fields::from(vec![
        f("time", ts_type()),
        f("name", DataType::Utf8),
        map_field("attributes", &ev_attrs),
    ]);
    let events = c.ev_offsets.as_mut().unwrap().finish(
        ev_fields,
        vec![
            Arc::new(c.ev_time.finish()),
            Arc::new(c.ev_name.finish()),
            ev_attrs,
        ],
    );
    let ln_attrs = c.ln_attrs.as_mut().unwrap().finish();
    let ln_fields = Fields::from(vec![
        uuid_field("trace_id"),
        f("span_id", DataType::UInt64),
        f("trace_state", DataType::Utf8),
        map_field("attributes", &ln_attrs),
    ]);
    let links = c.ln_offsets.as_mut().unwrap().finish(
        ln_fields,
        vec![
            fsb_opt(&c.ln_trace),
            Arc::new(c.ln_span.finish()),
            Arc::new(c.ln_state.finish()),
            ln_attrs,
        ],
    );
    let b = batch(vec![
        (f("start_time", ts_type()), Arc::new(c.start.finish())),
        (
            f("duration_ns", DataType::Int64),
            Arc::new(c.duration.finish()),
        ),
        (uuid_field("trace_id"), fsb(&c.trace_id)),
        (f("span_id", DataType::UInt64), Arc::new(c.span_id.finish())),
        (
            f("parent_span_id", DataType::UInt64),
            Arc::new(c.parent.finish()),
        ),
        (
            f("service_name", DataType::Utf8),
            Arc::new(c.service.finish()),
        ),
        (f("name", DataType::Utf8), Arc::new(c.name.finish())),
        (f("kind", DataType::Utf8), Arc::new(c.kind.finish())),
        (
            f("status_code", DataType::Utf8),
            Arc::new(c.status_code.finish()),
        ),
        (
            f("status_message", DataType::Utf8),
            Arc::new(c.status_message.finish()),
        ),
        (
            f("trace_state", DataType::Utf8),
            Arc::new(c.trace_state.finish()),
        ),
        (f("flags", DataType::UInt32), Arc::new(c.flags.finish())),
        (
            f("scope_name", DataType::Utf8),
            Arc::new(c.scope_name.finish()),
        ),
        (
            f("scope_version", DataType::Utf8),
            Arc::new(c.scope_version.finish()),
        ),
        (map_field("resource_attributes", &res_attrs), res_attrs),
        (map_field("attributes", &span_attrs), span_attrs),
        (f("events", events.data_type().clone()), events),
        (f("links", links.data_type().clone()), links),
    ]);
    out.batches.push((TableId::Spans, b));
    out
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

pub fn convert_logs(req: &ExportLogsServiceRequest) -> Converted {
    let mut time = TimestampMicrosecondBuilder::new();
    let mut observed = TimestampMicrosecondBuilder::new();
    let mut tids: Vec<Option<[u8; 16]>> = Vec::new();
    let mut sids = UInt64Builder::new();
    let mut service = StringBuilder::new();
    let mut sev_num = UInt8Builder::new();
    let mut sev_text = StringBuilder::new();
    let mut body = StringBuilder::new();
    let mut event_name = StringBuilder::new();
    let mut flags = UInt32Builder::new();
    let mut scope_name = StringBuilder::new();
    let mut scope_version = StringBuilder::new();
    let mut res_attrs = AttrCol::new();
    let mut log_attrs = AttrCol::new();
    let mut n = 0u64;
    let recv = now_micros();
    for rl in &req.resource_logs {
        let res = ResourceCtx::new(rl.resource.as_ref());
        for sl in &rl.scope_logs {
            let (sn, sv) = scope_parts(sl.scope.as_ref());
            for r in &sl.log_records {
                n += 1;
                let obs = if r.observed_time_unix_nano == 0 {
                    recv
                } else {
                    nanos_to_micros(r.observed_time_unix_nano)
                };
                let t = if r.time_unix_nano == 0 {
                    obs
                } else {
                    nanos_to_micros(r.time_unix_nano)
                };
                time.append_value(t);
                observed.append_value(obs);
                tids.push(trace_id(&r.trace_id));
                sids.append_option(span_id(&r.span_id));
                service.append_value(&res.service);
                sev_num.append_value(r.severity_number.clamp(0, 24) as u8);
                sev_text.append_value(&r.severity_text);
                body.append_value(r.body.as_ref().map(any_value_to_string).unwrap_or_default());
                event_name.append_value(&r.event_name);
                flags.append_value(r.flags);
                scope_name.append_value(sn);
                scope_version.append_value(sv);
                res_attrs.append(&res.attrs);
                log_attrs.append(&attrs(&r.attributes));
            }
        }
    }
    let mut out = Converted {
        accepted: n,
        ..Default::default()
    };
    if n == 0 {
        return out;
    }
    let ra = res_attrs.finish();
    let la = log_attrs.finish();
    out.batches.push((
        TableId::Logs,
        batch(vec![
            (f("time", ts_type()), Arc::new(time.finish())),
            (f("observed_time", ts_type()), Arc::new(observed.finish())),
            (uuid_field("trace_id"), fsb_opt(&tids)),
            (f("span_id", DataType::UInt64), Arc::new(sids.finish())),
            (
                f("service_name", DataType::Utf8),
                Arc::new(service.finish()),
            ),
            (
                f("severity_number", DataType::UInt8),
                Arc::new(sev_num.finish()),
            ),
            (
                f("severity_text", DataType::Utf8),
                Arc::new(sev_text.finish()),
            ),
            (f("body", DataType::Utf8), Arc::new(body.finish())),
            (
                f("event_name", DataType::Utf8),
                Arc::new(event_name.finish()),
            ),
            (f("flags", DataType::UInt32), Arc::new(flags.finish())),
            (
                f("scope_name", DataType::Utf8),
                Arc::new(scope_name.finish()),
            ),
            (
                f("scope_version", DataType::Utf8),
                Arc::new(scope_version.finish()),
            ),
            (map_field("resource_attributes", &ra), ra),
            (map_field("attributes", &la), la),
        ]),
    ));
    out
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Columns shared by every metric table.
struct MetricBase {
    time: TimestampMicrosecondBuilder,
    start: TimestampMicrosecondBuilder,
    service: StringBuilder,
    name: StringBuilder,
    description: StringBuilder,
    unit: StringBuilder,
    temporality: StringBuilder,
    series: UInt64Builder,
    flags: UInt32Builder,
    scope_name: StringBuilder,
    scope_version: StringBuilder,
    res_attrs: AttrCol,
    attrs: AttrCol,
    rows: usize,
}

struct MetricCtx<'a> {
    res: &'a ResourceCtx,
    res_hash: u64,
    scope_name: &'a str,
    scope_version: &'a str,
    name: &'a str,
    description: &'a str,
    unit: &'a str,
    temporality: &'static str,
}

impl MetricBase {
    fn new() -> Self {
        Self {
            time: TimestampMicrosecondBuilder::new(),
            start: TimestampMicrosecondBuilder::new(),
            service: StringBuilder::new(),
            name: StringBuilder::new(),
            description: StringBuilder::new(),
            unit: StringBuilder::new(),
            temporality: StringBuilder::new(),
            series: UInt64Builder::new(),
            flags: UInt32Builder::new(),
            scope_name: StringBuilder::new(),
            scope_version: StringBuilder::new(),
            res_attrs: AttrCol::new(),
            attrs: AttrCol::new(),
            rows: 0,
        }
    }

    fn append(
        &mut self,
        m: &MetricCtx,
        time_ns: u64,
        start_ns: u64,
        flags: u32,
        kvs: &[KeyValue],
        recv: i64,
    ) {
        let a = attrs(kvs);
        self.time.append_value(if time_ns == 0 {
            recv
        } else {
            nanos_to_micros(time_ns)
        });
        self.start
            .append_option((start_ns != 0).then(|| nanos_to_micros(start_ns)));
        self.service.append_value(&m.res.service);
        self.name.append_value(m.name);
        self.description.append_value(m.description);
        self.unit.append_value(m.unit);
        self.temporality.append_value(m.temporality);
        self.series.append_value(series_id(m.res_hash, m.name, &a));
        self.flags.append_value(flags);
        self.scope_name.append_value(m.scope_name);
        self.scope_version.append_value(m.scope_version);
        self.res_attrs.append(&m.res.attrs);
        self.attrs.append(&a);
        self.rows += 1;
    }

    fn finish(&mut self) -> Vec<(Field, ArrayRef)> {
        let ra = self.res_attrs.finish();
        let a = self.attrs.finish();
        vec![
            (f("time", ts_type()), Arc::new(self.time.finish())),
            (f("start_time", ts_type()), Arc::new(self.start.finish())),
            (
                f("service_name", DataType::Utf8),
                Arc::new(self.service.finish()),
            ),
            (
                f("metric_name", DataType::Utf8),
                Arc::new(self.name.finish()),
            ),
            (
                f("metric_description", DataType::Utf8),
                Arc::new(self.description.finish()),
            ),
            (
                f("metric_unit", DataType::Utf8),
                Arc::new(self.unit.finish()),
            ),
            (
                f("temporality", DataType::Utf8),
                Arc::new(self.temporality.finish()),
            ),
            (
                f("series_id", DataType::UInt64),
                Arc::new(self.series.finish()),
            ),
            (f("flags", DataType::UInt32), Arc::new(self.flags.finish())),
            (
                f("scope_name", DataType::Utf8),
                Arc::new(self.scope_name.finish()),
            ),
            (
                f("scope_version", DataType::Utf8),
                Arc::new(self.scope_version.finish()),
            ),
            (map_field("resource_attributes", &ra), ra),
            (map_field("attributes", &a), a),
        ]
    }
}

fn hash_attrs(h: &mut Xxh3, a: &[(String, String)]) {
    for (k, v) in a {
        h.update(k.as_bytes());
        h.update(&[0]);
        h.update(v.as_bytes());
        h.update(&[1]);
    }
}

fn resource_hash(res: &ResourceCtx) -> u64 {
    let mut h = Xxh3::new();
    hash_attrs(&mut h, &res.attrs);
    h.digest()
}

/// Stable id of a time series: metric name + resource attributes + point attributes.
fn series_id(res_hash: u64, name: &str, a: &[(String, String)]) -> u64 {
    let mut h = Xxh3::new();
    h.update(&res_hash.to_le_bytes());
    h.update(name.as_bytes());
    h.update(&[2]);
    hash_attrs(&mut h, a);
    h.digest()
}

fn temporality(t: i32) -> &'static str {
    match t {
        1 => "delta",
        2 => "cumulative",
        _ => "",
    }
}

fn u64_list(lists: &[Vec<u64>]) -> ArrayRef {
    let mut b = ListBuilder::new(UInt64Builder::new());
    for l in lists {
        b.values().append_slice(l);
        b.append(true);
    }
    Arc::new(b.finish())
}

fn f64_list(lists: &[Vec<f64>]) -> ArrayRef {
    let mut b = ListBuilder::new(Float64Builder::new());
    for l in lists {
        b.values().append_slice(l);
        b.append(true);
    }
    Arc::new(b.finish())
}

pub fn convert_metrics(req: &ExportMetricsServiceRequest) -> Converted {
    let recv = now_micros();
    // number points
    let mut np = MetricBase::new();
    let mut np_type = StringBuilder::new();
    let mut np_mono = BooleanBuilder::new();
    let mut np_value = Float64Builder::new();
    // histograms
    let mut h = MetricBase::new();
    let (mut h_count, mut h_sum, mut h_min, mut h_max) = (
        UInt64Builder::new(),
        Float64Builder::new(),
        Float64Builder::new(),
        Float64Builder::new(),
    );
    let (mut h_buckets, mut h_bounds): (Vec<Vec<u64>>, Vec<Vec<f64>>) = (vec![], vec![]);
    // exponential histograms
    let mut e = MetricBase::new();
    let (mut e_count, mut e_sum, mut e_min, mut e_max) = (
        UInt64Builder::new(),
        Float64Builder::new(),
        Float64Builder::new(),
        Float64Builder::new(),
    );
    let (mut e_scale, mut e_zero, mut e_zt) = (
        Int32Builder::new(),
        UInt64Builder::new(),
        Float64Builder::new(),
    );
    let (mut e_poff, mut e_noff) = (Int32Builder::new(), Int32Builder::new());
    let (mut e_pos, mut e_neg): (Vec<Vec<u64>>, Vec<Vec<u64>>) = (vec![], vec![]);
    // summaries
    let mut s = MetricBase::new();
    let (mut s_count, mut s_sum) = (UInt64Builder::new(), Float64Builder::new());
    let mut s_q_off = ListOffsets::new();
    let (mut s_q, mut s_v) = (Float64Builder::new(), Float64Builder::new());

    let mut rejected = 0u64;
    for rm in &req.resource_metrics {
        let res = ResourceCtx::new(rm.resource.as_ref());
        let res_hash = resource_hash(&res);
        for sm in &rm.scope_metrics {
            let (sn, sv) = scope_parts(sm.scope.as_ref());
            for m in &sm.metrics {
                let mut ctx = MetricCtx {
                    res: &res,
                    res_hash,
                    scope_name: sn,
                    scope_version: sv,
                    name: &m.name,
                    description: &m.description,
                    unit: &m.unit,
                    temporality: "",
                };
                match &m.data {
                    Some(Data::Gauge(g)) => {
                        for p in &g.data_points {
                            np.append(
                                &ctx,
                                p.time_unix_nano,
                                p.start_time_unix_nano,
                                p.flags,
                                &p.attributes,
                                recv,
                            );
                            np_type.append_value("gauge");
                            np_mono.append_value(false);
                            np_value.append_option(number_value(p.value.as_ref()));
                        }
                    }
                    Some(Data::Sum(sum)) => {
                        ctx.temporality = temporality(sum.aggregation_temporality);
                        for p in &sum.data_points {
                            np.append(
                                &ctx,
                                p.time_unix_nano,
                                p.start_time_unix_nano,
                                p.flags,
                                &p.attributes,
                                recv,
                            );
                            np_type.append_value("sum");
                            np_mono.append_value(sum.is_monotonic);
                            np_value.append_option(number_value(p.value.as_ref()));
                        }
                    }
                    Some(Data::Histogram(hist)) => {
                        ctx.temporality = temporality(hist.aggregation_temporality);
                        for p in &hist.data_points {
                            h.append(
                                &ctx,
                                p.time_unix_nano,
                                p.start_time_unix_nano,
                                p.flags,
                                &p.attributes,
                                recv,
                            );
                            h_count.append_value(p.count);
                            h_sum.append_option(p.sum);
                            h_min.append_option(p.min);
                            h_max.append_option(p.max);
                            h_buckets.push(p.bucket_counts.clone());
                            h_bounds.push(p.explicit_bounds.clone());
                        }
                    }
                    Some(Data::ExponentialHistogram(hist)) => {
                        ctx.temporality = temporality(hist.aggregation_temporality);
                        for p in &hist.data_points {
                            e.append(
                                &ctx,
                                p.time_unix_nano,
                                p.start_time_unix_nano,
                                p.flags,
                                &p.attributes,
                                recv,
                            );
                            e_count.append_value(p.count);
                            e_sum.append_option(p.sum);
                            e_min.append_option(p.min);
                            e_max.append_option(p.max);
                            e_scale.append_value(p.scale);
                            e_zero.append_value(p.zero_count);
                            e_zt.append_value(p.zero_threshold);
                            let pos = p.positive.as_ref();
                            let neg = p.negative.as_ref();
                            e_poff.append_value(pos.map(|b| b.offset).unwrap_or(0));
                            e_pos.push(pos.map(|b| b.bucket_counts.clone()).unwrap_or_default());
                            e_noff.append_value(neg.map(|b| b.offset).unwrap_or(0));
                            e_neg.push(neg.map(|b| b.bucket_counts.clone()).unwrap_or_default());
                        }
                    }
                    Some(Data::Summary(sum)) => {
                        for p in &sum.data_points {
                            s.append(
                                &ctx,
                                p.time_unix_nano,
                                p.start_time_unix_nano,
                                p.flags,
                                &p.attributes,
                                recv,
                            );
                            s_count.append_value(p.count);
                            s_sum.append_value(p.sum);
                            for q in &p.quantile_values {
                                s_q.append_value(q.quantile);
                                s_v.append_value(q.value);
                            }
                            s_q_off.close_row(p.quantile_values.len());
                        }
                    }
                    None => rejected += 1,
                }
            }
        }
    }

    let mut out = Converted {
        rejected,
        ..Default::default()
    };
    if rejected > 0 {
        out.reject_reason = Some(format!("{rejected} metrics had no data"));
    }
    if np.rows > 0 {
        out.accepted += np.rows as u64;
        let mut cols = np.finish();
        cols.push((f("metric_type", DataType::Utf8), Arc::new(np_type.finish())));
        cols.push((
            f("is_monotonic", DataType::Boolean),
            Arc::new(np_mono.finish()),
        ));
        cols.push((f("value", DataType::Float64), Arc::new(np_value.finish())));
        out.batches.push((TableId::MetricPoints, batch(cols)));
    }
    if h.rows > 0 {
        out.accepted += h.rows as u64;
        let mut cols = h.finish();
        cols.push((f("count", DataType::UInt64), Arc::new(h_count.finish())));
        cols.push((f("sum", DataType::Float64), Arc::new(h_sum.finish())));
        cols.push((f("min", DataType::Float64), Arc::new(h_min.finish())));
        cols.push((f("max", DataType::Float64), Arc::new(h_max.finish())));
        let b = u64_list(&h_buckets);
        cols.push((f("bucket_counts", b.data_type().clone()), b));
        let b = f64_list(&h_bounds);
        cols.push((f("explicit_bounds", b.data_type().clone()), b));
        out.batches.push((TableId::MetricHistograms, batch(cols)));
    }
    if e.rows > 0 {
        out.accepted += e.rows as u64;
        let mut cols = e.finish();
        cols.push((f("count", DataType::UInt64), Arc::new(e_count.finish())));
        cols.push((f("sum", DataType::Float64), Arc::new(e_sum.finish())));
        cols.push((f("min", DataType::Float64), Arc::new(e_min.finish())));
        cols.push((f("max", DataType::Float64), Arc::new(e_max.finish())));
        cols.push((f("scale", DataType::Int32), Arc::new(e_scale.finish())));
        cols.push((f("zero_count", DataType::UInt64), Arc::new(e_zero.finish())));
        cols.push((
            f("zero_threshold", DataType::Float64),
            Arc::new(e_zt.finish()),
        ));
        cols.push((
            f("positive_offset", DataType::Int32),
            Arc::new(e_poff.finish()),
        ));
        let b = u64_list(&e_pos);
        cols.push((f("positive_bucket_counts", b.data_type().clone()), b));
        cols.push((
            f("negative_offset", DataType::Int32),
            Arc::new(e_noff.finish()),
        ));
        let b = u64_list(&e_neg);
        cols.push((f("negative_bucket_counts", b.data_type().clone()), b));
        out.batches
            .push((TableId::MetricExpHistograms, batch(cols)));
    }
    if s.rows > 0 {
        out.accepted += s.rows as u64;
        let mut cols = s.finish();
        cols.push((f("count", DataType::UInt64), Arc::new(s_count.finish())));
        cols.push((f("sum", DataType::Float64), Arc::new(s_sum.finish())));
        let qf = Fields::from(vec![
            f("quantile", DataType::Float64),
            f("value", DataType::Float64),
        ]);
        let q = s_q_off.finish(qf, vec![Arc::new(s_q.finish()), Arc::new(s_v.finish())]);
        cols.push((f("quantiles", q.data_type().clone()), q));
        out.batches.push((TableId::MetricSummaries, batch(cols)));
    }
    out
}

fn number_value(v: Option<&number_data_point::Value>) -> Option<f64> {
    match v? {
        number_data_point::Value::AsDouble(d) => Some(*d),
        number_data_point::Value::AsInt(i) => Some(*i as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::{
        common::v1::ArrayValue,
        trace::v1::{ResourceSpans, ScopeSpans, Span, span::Event},
    };

    fn kv(k: &str, v: any_value::Value) -> KeyValue {
        KeyValue {
            key: k.into(),
            value: Some(AnyValue { value: Some(v) }),
            ..Default::default()
        }
    }

    #[test]
    fn stringifies_values() {
        let arr = AnyValue {
            value: Some(any_value::Value::ArrayValue(ArrayValue {
                values: vec![
                    AnyValue {
                        value: Some(any_value::Value::IntValue(1)),
                    },
                    AnyValue {
                        value: Some(any_value::Value::StringValue("a".into())),
                    },
                ],
            })),
        };
        assert_eq!(any_value_to_string(&arr), r#"[1,"a"]"#);
        assert_eq!(
            any_value_to_string(&AnyValue {
                value: Some(any_value::Value::DoubleValue(1.5))
            }),
            "1.5"
        );
        assert_eq!(any_value_to_string(&AnyValue { value: None }), "");
    }

    #[test]
    fn dedupes_attributes() {
        let a = attrs(&[
            kv("b", any_value::Value::StringValue("1".into())),
            kv("a", any_value::Value::StringValue("x".into())),
            kv("b", any_value::Value::StringValue("2".into())),
        ]);
        assert_eq!(a, vec![("a".into(), "x".into()), ("b".into(), "2".into())]);
    }

    #[test]
    fn converts_spans_and_rejects_invalid_ids() {
        let good = Span {
            trace_id: vec![1; 16],
            span_id: vec![2; 8],
            name: "GET /".into(),
            kind: 2,
            start_time_unix_nano: 1_700_000_000_000_000_000,
            end_time_unix_nano: 1_700_000_000_005_000_000,
            events: vec![Event {
                time_unix_nano: 0,
                name: "e".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let bad = Span {
            trace_id: vec![0; 16],
            span_id: vec![2; 8],
            ..Default::default()
        };
        let req = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![kv(
                        "service.name",
                        any_value::Value::StringValue("api".into()),
                    )],
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans: vec![good, bad],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let c = convert_traces(&req);
        assert_eq!((c.accepted, c.rejected), (1, 1));
        let b = &c.batches[0].1;
        assert_eq!(b.num_rows(), 1);
        assert_eq!(b.num_columns(), crate::store::schema::SPANS.columns.len());
        let dur = b
            .column(1)
            .as_primitive::<arrow::datatypes::Int64Type>()
            .value(0);
        assert_eq!(dur, 5_000_000);
    }

    #[test]
    fn series_id_is_stable_and_distinct() {
        let res = ResourceCtx {
            service: "s".into(),
            attrs: vec![("service.name".into(), "s".into())],
        };
        let rh = resource_hash(&res);
        let a = vec![("k".to_string(), "v".to_string())];
        assert_eq!(series_id(rh, "m", &a), series_id(rh, "m", &a));
        assert_ne!(series_id(rh, "m", &a), series_id(rh, "m2", &a));
        assert_ne!(series_id(rh, "m", &a), series_id(rh, "m", &[]));
    }
}
