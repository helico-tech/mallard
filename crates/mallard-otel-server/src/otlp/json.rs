//! OTLP/JSON decoding that accepts every protojson encoding of a double.
//!
//! Protobuf's JSON mapping writes a `double` as a number or as a string:
//! `"NaN"`, `"Infinity"`, `"-Infinity"`, or a number in quotes (`"1.5"`).
//! opentelemetry-proto's serde code (0.33) only takes numbers for most
//! doubles. Where the double sits in a flattened oneof (`AnyValue`) one
//! `"NaN"` failed the whole request with 400; in a gauge or sum point
//! (`asDouble`) the value was silently dropped (stored as NULL).
//!
//! Silent, because `Metric.data`, `NumberDataPoint.value` and `Exemplar.value`
//! are flattened `Option`s: a value they cannot read becomes `None`, and that
//! also takes away everything else in the metric (a quoted `sum`, or a quoted
//! double in an attribute of a point).
//!
//! [`decode`] keeps the fast path: the plain decoder, then a look at the result
//! ([`OtlpJson::maybe_lost_values`], a few pointer chases per point). It costs
//! nothing else, because the body is not searched. When the plain decoder fails
//! or may have lost a value, it parses the body into a [`Value`] and decodes
//! again through [`Lenient`], a deserializer that hands such strings to the
//! field as an `f64` when they sit under a key that is a double in OTLP
//! ([`DOUBLE_KEYS`]). Every other string stays a string, so an invalid payload
//! still fails.
//!
//! Unknown keys are ignored on both paths, also inside an `AnyValue`.

use serde::{
    de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor},
    forward_to_deserialize_any,
};
use serde_json::Value;

use opentelemetry_proto::tonic::{
    collector::{
        logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
        trace::v1::ExportTraceServiceRequest,
    },
    metrics::v1::{Exemplar, NumberDataPoint, metric::Data},
};

/// JSON keys whose value is a protobuf `double` (or a list of them) in the
/// OTLP messages. Only these may carry a double as a string.
const DOUBLE_KEYS: &[&str] = &[
    "doubleValue",    // AnyValue
    "asDouble",       // NumberDataPoint, Exemplar
    "sum",            // histogram, exponential histogram, summary points
    "min",            // histogram, exponential histogram
    "max",            // histogram, exponential histogram
    "zeroThreshold",  // exponential histogram
    "explicitBounds", // histogram (a list)
    "quantile",       // summary
    "value",          // summary quantile value (KeyValue.value is an object, untouched)
];

/// A request type [`decode`] takes.
pub trait OtlpJson: DeserializeOwned {
    /// Whether the plain decoder may have dropped a value without an error.
    /// True also for a value that was never sent; the lenient decoder then
    /// gives the same result, only slower.
    fn maybe_lost_values(&self) -> bool {
        false
    }
}

/// Spans and logs have no flattened `Option` around a double: the plain decoder fails.
impl OtlpJson for ExportTraceServiceRequest {}
impl OtlpJson for ExportLogsServiceRequest {}

impl OtlpJson for ExportMetricsServiceRequest {
    fn maybe_lost_values(&self) -> bool {
        let exemplars = |e: &[Exemplar]| e.iter().any(|e| e.value.is_none());
        let numbers = |p: &[NumberDataPoint]| p.iter().any(|p| p.value.is_none() || exemplars(&p.exemplars));
        let mut metrics = self.resource_metrics.iter().flat_map(|r| &r.scope_metrics).flat_map(|s| &s.metrics);
        metrics.any(|m| match &m.data {
            None => true,
            Some(Data::Gauge(g)) => numbers(&g.data_points),
            Some(Data::Sum(s)) => numbers(&s.data_points),
            Some(Data::Histogram(h)) => h.data_points.iter().any(|p| exemplars(&p.exemplars)),
            Some(Data::ExponentialHistogram(h)) => h.data_points.iter().any(|p| exemplars(&p.exemplars)),
            Some(Data::Summary(_)) => false,
        })
    }
}

/// Decode an OTLP/JSON request body.
pub fn decode<T: OtlpJson>(raw: &[u8]) -> Result<T, serde_json::Error> {
    let first = match serde_json::from_slice::<T>(raw) {
        Ok(v) if !v.maybe_lost_values() => return Ok(v),
        Ok(_) => None,
        Err(e) => Some(e),
    };
    match serde_json::from_slice::<Value>(raw) {
        Ok(value) => T::deserialize(Lenient { v: &value, double: false }),
        // Not JSON at all: the first error has a position.
        Err(e) => Err(first.unwrap_or(e)),
    }
}

/// A protojson string form of a double, or `None` for any other string.
fn special_double(s: &str) -> Option<f64> {
    match s {
        "NaN" => Some(f64::NAN),
        "Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        // A quoted number. `f64::from_str` also takes "inf" and "nan", which
        // protojson does not, so only allow number characters.
        _ if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b)) => s.parse().ok(),
        _ => None,
    }
}

/// Deserializes a borrowed [`Value`]. `double`: the value sits under a key
/// in [`DOUBLE_KEYS`], so a string that is a protojson double is an `f64`.
struct Lenient<'a> {
    v: &'a Value,
    double: bool,
}

impl<'de> de::Deserializer<'de> for Lenient<'de> {
    type Error = serde_json::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        match self.v {
            Value::Null => visitor.visit_unit(),
            Value::Bool(b) => visitor.visit_bool(*b),
            Value::Number(n) => {
                if let Some(u) = n.as_u64() {
                    visitor.visit_u64(u)
                } else if let Some(i) = n.as_i64() {
                    visitor.visit_i64(i)
                } else {
                    visitor.visit_f64(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            Value::String(s) => match self.double.then(|| special_double(s)).flatten() {
                Some(d) => visitor.visit_f64(d),
                None => visitor.visit_borrowed_str(s),
            },
            // A list under a double key holds doubles (`explicitBounds`).
            Value::Array(a) => visitor.visit_seq(Seq { it: a.iter(), double: self.double }),
            Value::Object(m) => visitor.visit_map(Entries { it: m.iter(), value: None }),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        match self.v {
            Value::Null => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        // OTLP oneofs are flattened (they go through `deserialize_any`); a
        // plain enum is a unit variant name or `{variant: value}`.
        de::Deserializer::deserialize_enum(self.v, name, variants, visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct identifier
    }
}

struct Seq<'a> {
    it: std::slice::Iter<'a, Value>,
    double: bool,
}

impl<'de> SeqAccess<'de> for Seq<'de> {
    type Error = serde_json::Error;

    fn next_element_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<Option<S::Value>, Self::Error> {
        self.it.next().map(|v| seed.deserialize(Lenient { v, double: self.double })).transpose()
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.it.len())
    }
}

struct Entries<'a> {
    it: serde_json::map::Iter<'a>,
    value: Option<(&'a str, &'a Value)>,
}

impl<'de> MapAccess<'de> for Entries<'de> {
    type Error = serde_json::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error> {
        let Some((k, v)) = self.it.next() else { return Ok(None) };
        self.value = Some((k.as_str(), v));
        seed.deserialize(de::value::BorrowedStrDeserializer::<Self::Error>::new(k)).map(Some)
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, Self::Error> {
        let (k, v) = self.value.take().ok_or_else(|| de::Error::custom("value without a key"))?;
        seed.deserialize(Lenient { v, double: DOUBLE_KEYS.contains(&k) })
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.it.len())
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry_proto::tonic::{
        common::v1::{AnyValue, any_value::Value as V},
        metrics::v1::{exemplar, number_data_point},
    };

    use super::*;

    fn double(v: &AnyValue) -> f64 {
        match v.value {
            Some(V::DoubleValue(d)) => d,
            ref other => panic!("not a double: {other:?}"),
        }
    }

    #[test]
    fn non_finite_doubles_in_span_attributes() {
        let body = br#"{"resourceSpans":[{"resource":{"attributes":[
            {"key":"r","value":{"doubleValue":"Infinity"}}]},
          "scopeSpans":[{"spans":[{"traceId":"0102030405060708090a0b0c0d0e0f10","spanId":"0102030405060708",
            "name":"s","startTimeUnixNano":"1","endTimeUnixNano":"2",
            "attributes":[{"key":"a","value":{"doubleValue":"NaN"}},
                          {"key":"b","value":{"doubleValue":"-Infinity"}},
                          {"key":"c","value":{"doubleValue":"2.5"}},
                          {"key":"d","value":{"doubleValue":1.5}},
                          {"key":"s","value":{"stringValue":"NaN"}}]}]}]}]}"#;
        let req: ExportTraceServiceRequest = decode(body).unwrap();
        let rs = &req.resource_spans[0];
        assert_eq!(double(rs.resource.as_ref().unwrap().attributes[0].value.as_ref().unwrap()), f64::INFINITY);
        let a = &rs.scope_spans[0].spans[0].attributes;
        assert!(double(a[0].value.as_ref().unwrap()).is_nan());
        assert_eq!(double(a[1].value.as_ref().unwrap()), f64::NEG_INFINITY);
        assert_eq!(double(a[2].value.as_ref().unwrap()), 2.5);
        assert_eq!(double(a[3].value.as_ref().unwrap()), 1.5);
        // A string attribute stays a string.
        assert_eq!(a[4].value.as_ref().unwrap().value, Some(V::StringValue("NaN".into())));
        assert_eq!(rs.scope_spans[0].spans[0].span_id, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn non_finite_doubles_in_log_body_and_nested_values() {
        let body = br#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[
            {"timeUnixNano":"1","body":{"doubleValue":"NaN"}},
            {"body":{"kvlistValue":{"values":[
                {"key":"x","value":{"doubleValue":"Infinity"}},
                {"key":"l","value":{"arrayValue":{"values":[{"doubleValue":"-Infinity"},{"intValue":"7"}]}}}]}},
             "attributes":[{"key":"arr","value":{"arrayValue":{"values":[{"doubleValue":"NaN"}]}}}]}]}]}]}"#;
        let req: ExportLogsServiceRequest = decode(body).unwrap();
        let logs = &req.resource_logs[0].scope_logs[0].log_records;
        assert!(double(logs[0].body.as_ref().unwrap()).is_nan());
        let Some(V::KvlistValue(kv)) = &logs[1].body.as_ref().unwrap().value else { panic!("kvlist") };
        assert_eq!(double(kv.values[0].value.as_ref().unwrap()), f64::INFINITY);
        let Some(V::ArrayValue(arr)) = &kv.values[1].value.as_ref().unwrap().value else { panic!("array") };
        assert_eq!(double(&arr.values[0]), f64::NEG_INFINITY);
        assert_eq!(arr.values[1].value, Some(V::IntValue(7)));
        let Some(V::ArrayValue(arr)) = &logs[1].attributes[0].value.as_ref().unwrap().value else { panic!("array") };
        assert!(double(&arr.values[0]).is_nan());
    }

    #[test]
    fn non_finite_metric_values() {
        let body = br#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[
            {"name":"g","gauge":{"dataPoints":[{"timeUnixNano":"1","asDouble":"NaN",
                "attributes":[{"key":"k","value":{"doubleValue":"Infinity"}}]}]}},
            {"name":"h","histogram":{"aggregationTemporality":2,"dataPoints":[{"timeUnixNano":"1",
                "count":"2","sum":"Infinity","min":"-Infinity","max":"NaN",
                "bucketCounts":["1","1"],"explicitBounds":["Infinity"]}]}}]}]}]}"#;
        let req: ExportMetricsServiceRequest = decode(body).unwrap();
        let m = &req.resource_metrics[0].scope_metrics[0].metrics;
        let Some(Data::Gauge(g)) = &m[0].data else { panic!("gauge") };
        let Some(number_data_point::Value::AsDouble(d)) = g.data_points[0].value else { panic!("asDouble") };
        assert!(d.is_nan());
        assert_eq!(double(g.data_points[0].attributes[0].value.as_ref().unwrap()), f64::INFINITY);
        let Some(Data::Histogram(h)) = &m[1].data else { panic!("histogram") };
        let p = &h.data_points[0];
        assert_eq!((p.sum, p.min), (Some(f64::INFINITY), Some(f64::NEG_INFINITY)));
        assert!(p.max.unwrap().is_nan());
        assert_eq!(p.explicit_bounds, vec![f64::INFINITY]);
        assert_eq!(p.bucket_counts, vec![1, 1]);
    }

    /// The plain decoder accepts `"asDouble": "NaN"` but drops the value.
    #[test]
    fn gauge_nan_alone_keeps_its_value() {
        let body = br#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[
            {"name":"g","gauge":{"dataPoints":[{"timeUnixNano":"1","asDouble":"NaN"}]}}]}]}]}"#;
        let req: ExportMetricsServiceRequest = decode(body).unwrap();
        let Some(Data::Gauge(g)) = &req.resource_metrics[0].scope_metrics[0].metrics[0].data else { panic!("gauge") };
        assert!(matches!(g.data_points[0].value, Some(number_data_point::Value::AsDouble(d)) if d.is_nan()));
    }

    /// `"1.5"` decodes without an error and loses the value (issue 0138).
    #[test]
    fn quoted_finite_doubles_keep_their_value() {
        let body = |metric: &str| {
            format!(r#"{{"resourceMetrics":[{{"scopeMetrics":[{{"metrics":[{{"name":"m",{metric}}}]}}]}}]}}"#)
        };
        let data = |metric: &str| {
            let req: ExportMetricsServiceRequest = decode(body(metric).as_bytes()).unwrap();
            req.resource_metrics[0].scope_metrics[0].metrics[0].data.clone().expect("metric data")
        };

        for quoted in [r#""asDouble":"1.5""#, r#""asDouble" : "1.5""#] {
            let Data::Gauge(g) = data(&format!(r#""gauge":{{"dataPoints":[{{{quoted}}}]}}"#)) else { panic!("gauge") };
            assert_eq!(g.data_points[0].value, Some(number_data_point::Value::AsDouble(1.5)));
        }
        let Data::Sum(s) = data(r#""sum":{"dataPoints":[{"asInt":"3","exemplars":[{"asDouble":"2.5"}]}]}"#) else {
            panic!("sum")
        };
        assert_eq!(s.data_points[0].value, Some(number_data_point::Value::AsInt(3)));
        assert_eq!(s.data_points[0].exemplars[0].value, Some(exemplar::Value::AsDouble(2.5)));
        let Data::Histogram(h) = data(r#""histogram":{"dataPoints":[{"exemplars":[{"asDouble":"2.5"}]}]}"#) else {
            panic!("histogram")
        };
        assert_eq!(h.data_points[0].exemplars[0].value, Some(exemplar::Value::AsDouble(2.5)));

        let Data::Histogram(h) =
            data(r#""histogram":{"dataPoints":[{"sum":"3","min":"0.5","max":"2.5","explicitBounds":["1.5"]}]}"#)
        else {
            panic!("histogram")
        };
        let p = &h.data_points[0];
        assert_eq!((p.sum, p.min, p.max), (Some(3.0), Some(0.5), Some(2.5)));
        assert_eq!(p.explicit_bounds, vec![1.5]);
        let Data::ExponentialHistogram(h) = data(r#""exponentialHistogram":{"dataPoints":[{"zeroThreshold":"0.5"}]}"#)
        else {
            panic!("exponential histogram")
        };
        assert_eq!(h.data_points[0].zero_threshold, 0.5);
        let Data::Summary(s) =
            data(r#""summary":{"dataPoints":[{"quantileValues":[{"quantile":"0.5","value":"3.5"}]}]}"#)
        else {
            panic!("summary")
        };
        assert_eq!(
            (s.data_points[0].quantile_values[0].quantile, s.data_points[0].quantile_values[0].value),
            (0.5, 3.5)
        );
        // A quoted double in an attribute takes the whole metric with it.
        let Data::Gauge(g) =
            data(r#""gauge":{"dataPoints":[{"asDouble":1,"attributes":[{"key":"k","value":{"doubleValue":"0.5"}}]}]}"#)
        else {
            panic!("gauge")
        };
        assert_eq!(double(g.data_points[0].attributes[0].value.as_ref().unwrap()), 0.5);
    }

    /// Normal bodies never pay for the lenient decoder.
    #[test]
    fn complete_metrics_take_the_fast_path() {
        let complete = br#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[
            {"name":"g","gauge":{"dataPoints":[{"asDouble":1.5},{"asInt":"2","exemplars":[{"asDouble":1}]}]}},
            {"name":"h","histogram":{"dataPoints":[{"count":"1","sum":1.5,"exemplars":[{"asInt":"1"}]}]}},
            {"name":"e","exponentialHistogram":{"dataPoints":[{"count":"1","sum":1.5}]}},
            {"name":"s","summary":{"dataPoints":[{"count":"1","sum":1.5}]}}]}]}]}"#;
        let req: ExportMetricsServiceRequest = serde_json::from_slice(complete).unwrap();
        assert!(!req.maybe_lost_values());
    }

    /// A body the plain decoder reads as "no value" stays that way.
    #[test]
    fn values_that_were_never_sent_stay_missing() {
        let body = br#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[
            {"name":"none"},{"name":"g","gauge":{"dataPoints":[{"timeUnixNano":"1"},{"asDouble":"abc"}]}}]}]}]}"#;
        let req: ExportMetricsServiceRequest = decode(body).unwrap();
        let m = &req.resource_metrics[0].scope_metrics[0].metrics;
        assert!(m[0].data.is_none());
        let Some(Data::Gauge(g)) = &m[1].data else { panic!("gauge") };
        assert!(g.data_points.iter().all(|p| p.value.is_none()));
    }

    /// Also when the body takes the lenient path, and with a value that has
    /// to be skipped as a whole (issue 0138).
    #[test]
    fn unknown_keys_in_any_value_are_ignored() {
        let values = [
            (r#"{"stringValue":"x","future":1}"#, Some(V::StringValue("x".into()))),
            (r#"{"future":{"a":[1,{"b":2}]},"intValue":"7"}"#, Some(V::IntValue(7))),
            (r#"{"future":null,"doubleValue":"NaN"}"#, None),
            (r#"{"future":1}"#, None),
        ];
        for (value, want) in values {
            let body = format!(
                r#"{{"resourceLogs":[{{"scopeLogs":[{{"logRecords":[{{"body":{value},
                    "attributes":[{{"key":"k","value":{value}}}]}}]}}]}}]}}"#
            );
            let req: ExportLogsServiceRequest = decode(body.as_bytes()).unwrap();
            let log = &req.resource_logs[0].scope_logs[0].log_records[0];
            if value.contains("NaN") {
                assert!(double(log.body.as_ref().unwrap()).is_nan());
            } else {
                assert_eq!(log.body.as_ref().unwrap().value, want);
                assert_eq!(log.attributes[0].value.as_ref().unwrap().value, want);
            }
        }
        // Inside a metric's data, where a decode error would lose the metric.
        let body = br#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[{"name":"g","gauge":{"dataPoints":[
            {"asDouble":"1.5","attributes":[{"key":"k","value":{"stringValue":"v","future":1}}]}]}}]}]}]}"#;
        let req: ExportMetricsServiceRequest = decode(body).unwrap();
        let Some(Data::Gauge(g)) = &req.resource_metrics[0].scope_metrics[0].metrics[0].data else { panic!("gauge") };
        assert_eq!(g.data_points[0].value, Some(number_data_point::Value::AsDouble(1.5)));
        assert_eq!(g.data_points[0].attributes[0].value.as_ref().unwrap().value, Some(V::StringValue("v".into())));
    }

    #[test]
    fn invalid_payloads_still_fail() {
        assert!(decode::<ExportTraceServiceRequest>(b"not json").is_err());
        // Only protojson spellings: "inf" is not a double.
        let bad = br#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[{"body":{"doubleValue":"inf"}}]}]}]}"#;
        assert!(decode::<ExportLogsServiceRequest>(bad).is_err());
        let bad = br#"{"resourceSpans":[{"scopeSpans":[{"spans":[{"traceId":"zz"}]}]}]}"#;
        assert!(decode::<ExportTraceServiceRequest>(bad).is_err());
    }
}
