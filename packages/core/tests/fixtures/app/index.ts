// Fixture for the golden-output baseline, ClickHouse resources only.
//
// The plugin transforms `new X<T>(...)` for the names in `typesToArgsLength`
// (packages/lib/src/dmv2/dataModelMetadata.ts). Of those, OlapTable
// and MaterializedView are the only ones that survive the narrowing to a
// ClickHouse-only product; Stream, DeadLetterQueue, IngestPipeline, IngestApi,
// Api and Task are all removed, so none of them may appear here.
import { Key, MaterializedView, OlapTable } from "@typed-clickhouse/core";

export interface EventRow {
  id: Key<string>;
  createdAt: Date;
  count: number;
  label: string;
  optionalNote?: string;
  tags: string[];
}

export interface AggregateRow {
  bucket: Key<string>;
  total: number;
  updatedAt: Date;
}

// name + config: argument count === expected
export const eventTable = new OlapTable<EventRow>("events", {
  orderByFields: ["id", "createdAt"],
});

// name only: argument count === expected - 1; both shapes are transformed
export const auditTable = new OlapTable<AggregateRow>("audit");

// MaterializedView takes exactly one argument (typesToArgsLength maps it to 1)
export const aggregateMv = new MaterializedView<AggregateRow>({
  selectStatement:
    "SELECT label AS bucket, count() AS total, max(createdAt) AS updatedAt FROM events GROUP BY label",
  selectTables: [eventTable],
  targetTable: { name: "aggregates" },
  materializedViewName: "aggregates_mv",
});
