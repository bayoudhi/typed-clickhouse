//! Annotation keys shared by the DDL renderer and the diff.

/// Wraps a column type in `LowCardinality(...)`.
pub(crate) const LOW_CARDINALITY_ANNOTATION: &str = "LowCardinality";

/// Renders a column as `AggregateFunction(...)`.
pub(crate) const AGGREGATION_FUNCTION_ANNOTATION: &str = "aggregationFunction";

/// Renders a column as `SimpleAggregateFunction(...)`.
pub(crate) const SIMPLE_AGGREGATION_FUNCTION_ANNOTATION: &str = "simpleAggregationFunction";

/// Annotations that change generated DDL: exactly the keys
/// `mapper::std_field_type_to_clickhouse_type_mapper` reads. Every other
/// annotation is code-side metadata ClickHouse cannot store, so it must not
/// decide whether a column changed.
pub(crate) const DDL_RELEVANT_ANNOTATIONS: [&str; 3] = [
    LOW_CARDINALITY_ANNOTATION,
    AGGREGATION_FUNCTION_ANNOTATION,
    SIMPLE_AGGREGATION_FUNCTION_ANNOTATION,
];
