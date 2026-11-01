//! DuckLake table definitions. See docs/data-model.md for the reasoning.
//!
//! Rules:
//! - Column order here MUST match the Arrow batches built in `otlp::convert`
//!   (the appender maps columns by position). `tests/lake.rs` checks this.
//! - New columns may only be appended at the end; `migrate` adds them with
//!   `ALTER TABLE ... ADD COLUMN`.
//! - All timestamps are `TIMESTAMP` (microseconds, UTC, no time zone).

/// A table in the lake.
pub struct TableDef {
    pub name: &'static str,
    /// Time column used for partitioning, sorting and retention.
    pub time_column: &'static str,
    /// Which retention setting applies.
    pub signal: Signal,
    /// `(name, sql type)` in storage order.
    pub columns: &'static [(&'static str, &'static str)],
    /// `SORTED BY` clause content.
    pub sort_by: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Traces,
    Logs,
    Metrics,
}

impl Signal {
    pub fn as_str(self) -> &'static str {
        match self {
            Signal::Traces => "traces",
            Signal::Logs => "logs",
            Signal::Metrics => "metrics",
        }
    }
}

const ATTRS: &str = "MAP(VARCHAR, VARCHAR)";

pub const SPANS: TableDef = TableDef {
    name: "spans",
    time_column: "start_time",
    signal: Signal::Traces,
    sort_by: "start_time",
    columns: &[
        ("start_time", "TIMESTAMP"),
        ("duration_ns", "BIGINT"),
        ("trace_id", "UUID"),
        ("span_id", "UBIGINT"),
        ("parent_span_id", "UBIGINT"),
        ("service_name", "VARCHAR"),
        ("name", "VARCHAR"),
        ("kind", "VARCHAR"),
        ("status_code", "VARCHAR"),
        ("status_message", "VARCHAR"),
        ("trace_state", "VARCHAR"),
        ("flags", "UINTEGER"),
        ("scope_name", "VARCHAR"),
        ("scope_version", "VARCHAR"),
        ("resource_attributes", ATTRS),
        ("attributes", ATTRS),
        (
            "events",
            "STRUCT(time TIMESTAMP, name VARCHAR, attributes MAP(VARCHAR, VARCHAR))[]",
        ),
        (
            "links",
            "STRUCT(trace_id UUID, span_id UBIGINT, trace_state VARCHAR, attributes MAP(VARCHAR, VARCHAR))[]",
        ),
    ],
};

pub const LOGS: TableDef = TableDef {
    name: "logs",
    time_column: "time",
    signal: Signal::Logs,
    sort_by: "time",
    columns: &[
        ("time", "TIMESTAMP"),
        ("observed_time", "TIMESTAMP"),
        ("trace_id", "UUID"),
        ("span_id", "UBIGINT"),
        ("service_name", "VARCHAR"),
        ("severity_number", "UTINYINT"),
        ("severity_text", "VARCHAR"),
        ("body", "VARCHAR"),
        ("event_name", "VARCHAR"),
        ("flags", "UINTEGER"),
        ("scope_name", "VARCHAR"),
        ("scope_version", "VARCHAR"),
        ("resource_attributes", ATTRS),
        ("attributes", ATTRS),
    ],
};

/// Columns shared by all metric tables, in order.
macro_rules! metric_columns {
    ($($extra:expr),* $(,)?) => {
        &[
            ("time", "TIMESTAMP"),
            ("start_time", "TIMESTAMP"),
            ("service_name", "VARCHAR"),
            ("metric_name", "VARCHAR"),
            ("metric_description", "VARCHAR"),
            ("metric_unit", "VARCHAR"),
            ("temporality", "VARCHAR"),
            ("series_id", "UBIGINT"),
            ("flags", "UINTEGER"),
            ("scope_name", "VARCHAR"),
            ("scope_version", "VARCHAR"),
            ("resource_attributes", ATTRS),
            ("attributes", ATTRS),
            $($extra),*
        ]
    };
}

/// Number data points of Gauge and Sum metrics.
pub const METRIC_POINTS: TableDef = TableDef {
    name: "metric_points",
    time_column: "time",
    signal: Signal::Metrics,
    sort_by: "metric_name, series_id, time",
    columns: metric_columns![
        ("metric_type", "VARCHAR"),
        ("is_monotonic", "BOOLEAN"),
        ("value", "DOUBLE"),
    ],
};

pub const METRIC_HISTOGRAMS: TableDef = TableDef {
    name: "metric_histograms",
    time_column: "time",
    signal: Signal::Metrics,
    sort_by: "metric_name, series_id, time",
    columns: metric_columns![
        ("count", "UBIGINT"),
        ("sum", "DOUBLE"),
        ("min", "DOUBLE"),
        ("max", "DOUBLE"),
        ("bucket_counts", "UBIGINT[]"),
        ("explicit_bounds", "DOUBLE[]"),
    ],
};

pub const METRIC_EXP_HISTOGRAMS: TableDef = TableDef {
    name: "metric_exp_histograms",
    time_column: "time",
    signal: Signal::Metrics,
    sort_by: "metric_name, series_id, time",
    columns: metric_columns![
        ("count", "UBIGINT"),
        ("sum", "DOUBLE"),
        ("min", "DOUBLE"),
        ("max", "DOUBLE"),
        ("scale", "INTEGER"),
        ("zero_count", "UBIGINT"),
        ("zero_threshold", "DOUBLE"),
        ("positive_offset", "INTEGER"),
        ("positive_bucket_counts", "UBIGINT[]"),
        ("negative_offset", "INTEGER"),
        ("negative_bucket_counts", "UBIGINT[]"),
    ],
};

pub const METRIC_SUMMARIES: TableDef = TableDef {
    name: "metric_summaries",
    time_column: "time",
    signal: Signal::Metrics,
    sort_by: "metric_name, series_id, time",
    columns: metric_columns![
        ("count", "UBIGINT"),
        ("sum", "DOUBLE"),
        ("quantiles", "STRUCT(quantile DOUBLE, value DOUBLE)[]"),
    ],
};

pub const TABLES: &[&TableDef] = &[
    &SPANS,
    &LOGS,
    &METRIC_POINTS,
    &METRIC_HISTOGRAMS,
    &METRIC_EXP_HISTOGRAMS,
    &METRIC_SUMMARIES,
];

/// Index of a table in [`TABLES`]; used by the write buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TableId {
    Spans = 0,
    Logs = 1,
    MetricPoints = 2,
    MetricHistograms = 3,
    MetricExpHistograms = 4,
    MetricSummaries = 5,
}

impl TableId {
    pub const ALL: [TableId; 6] = [
        TableId::Spans,
        TableId::Logs,
        TableId::MetricPoints,
        TableId::MetricHistograms,
        TableId::MetricExpHistograms,
        TableId::MetricSummaries,
    ];
    pub fn def(self) -> &'static TableDef {
        TABLES[self as usize]
    }
}

impl TableDef {
    /// `CREATE TABLE IF NOT EXISTS` statement for this table.
    pub fn create_sql(&self, catalog: &str, schema: &str) -> String {
        let cols: Vec<String> = self
            .columns
            .iter()
            .map(|(n, t)| format!("    {n} {t}"))
            .collect();
        format!(
            "CREATE TABLE IF NOT EXISTS {catalog}.{schema}.{name} (\n{cols}\n)\nPARTITIONED BY (epoch_day({time}))\nSORTED BY ({sort})",
            name = self.name,
            cols = cols.join(",\n"),
            time = self.time_column,
            sort = self.sort_by,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddl_renders() {
        let sql = SPANS.create_sql("lake", "otel");
        assert!(sql.contains("lake.otel.spans"));
        assert!(sql.contains("PARTITIONED BY (epoch_day(start_time))"));
        for t in TABLES {
            let names: std::collections::HashSet<_> = t.columns.iter().map(|c| c.0).collect();
            assert_eq!(
                names.len(),
                t.columns.len(),
                "duplicate column in {}",
                t.name
            );
            assert!(names.contains(t.time_column));
        }
        for (i, id) in TableId::ALL.iter().enumerate() {
            assert_eq!(*id as usize, i);
        }
    }
}
