/**
 * Tests for MaterializedView `refresh` option validation and serialization.
 */

import { expect } from "chai";
import { MaterializedView } from "../src/dmv2/sdk/materializedView";
import type { RefreshConfig } from "../src/dmv2/sdk/materializedView";
import { getInternalRegistry, toInfraMap } from "../src/dmv2/internal";
import { Column } from "../src/dataModels/dataModelTypes";
import type { IJsonSchemaCollection } from "typia";

interface TestData {
  id: string;
  value: number;
}

const createMockSchema = (): IJsonSchemaCollection.IV3_1 => ({
  version: "3.1",
  components: { schemas: {} },
  schemas: [{ type: "object", properties: {} }],
});

const createMockColumns = (fields: string[]): Column[] =>
  fields.map((field) => ({
    name: field as Column["name"],
    data_type: "String" as Column["data_type"],
    required: true,
    unique: false,
    primary_key: false,
    default: null,
    ttl: null,
    codec: null,
    materialized: null,
    alias: null,
    annotations: [],
    comment: null,
  }));

const makeMv = (name: string, refresh: RefreshConfig | undefined) =>
  new MaterializedView<TestData>(
    {
      selectStatement: "SELECT id, value FROM source_table",
      selectTables: [],
      targetTable: { name: `${name}_target` },
      materializedViewName: name,
      refresh,
    },
    createMockSchema(),
    createMockColumns(["id", "value"]),
  );

const mvJson = (name: string) =>
  toInfraMap(getInternalRegistry()).materializedViews[name] as any;

describe("MaterializedView refresh", () => {
  beforeEach(() => {
    const registry = getInternalRegistry();
    registry.tables.clear();
    registry.materializedViews.clear();
  });

  it("serializes every+offset", () => {
    makeMv("mv", { every: "1 DAY", offset: "10 HOUR" });
    expect(mvJson("mv").refresh).to.deep.equal({
      kind: "every",
      interval: { value: 1, unit: "DAY" },
      offset: { value: 10, unit: "HOUR" },
      append: false,
    });
  });

  it("serializes after+randomizeFor+append", () => {
    makeMv("mv", {
      after: "5 MINUTE",
      randomizeFor: "30 SECOND",
      append: true,
    });
    expect(mvJson("mv").refresh).to.deep.equal({
      kind: "after",
      interval: { value: 5, unit: "MINUTE" },
      randomizeFor: { value: 30, unit: "SECOND" },
      append: true,
    });
  });

  it("omits refresh when not set", () => {
    const mv = makeMv("mv", undefined);
    expect(mv.refresh).to.equal(undefined);
    const json = mvJson("mv");
    expect("refresh" in json).to.equal(false);
    expect(JSON.stringify(json)).to.not.include('"refresh":');
  });

  it("stores the user's config as given", () => {
    const cfg: RefreshConfig = { every: "1 DAY" };
    expect(makeMv("mv", cfg).refresh).to.equal(cfg);
  });

  it("accepts plural, lowercase and surrounding whitespace", () => {
    makeMv("mv", { every: "  10 hours " as any, offset: "2 Minutes" as any });
    expect(mvJson("mv").refresh).to.deep.equal({
      kind: "every",
      interval: { value: 10, unit: "HOUR" },
      offset: { value: 2, unit: "MINUTE" },
      append: false,
    });
  });

  for (const bad of [
    "1.5 DAY",
    "-1 DAY",
    "0 DAY",
    "1 FORTNIGHT",
    "DAY",
    "",
    "9007199254740993 DAY",
  ]) {
    it(`rejects bad interval '${bad}'`, () => {
      expect(() => makeMv("bad_mv", { every: bad as any })).to.throw(
        `Invalid refresh interval '${bad}' on materialized view 'bad_mv': expected '<positive integer> <SECOND|MINUTE|HOUR|DAY|WEEK|MONTH|YEAR>'`,
      );
      expect(getInternalRegistry().materializedViews.has("bad_mv")).to.equal(
        false,
      );
    });
  }

  it("validates offset and randomizeFor too", () => {
    expect(() => makeMv("mv", { every: "1 DAY", offset: "0 HOUR" })).to.throw(
      "Invalid refresh interval '0 HOUR'",
    );
    expect(() =>
      makeMv("mv", { after: "1 DAY", randomizeFor: "x" as any }),
    ).to.throw("Invalid refresh interval 'x'");
    expect(getInternalRegistry().materializedViews.has("mv")).to.equal(false);
    expect(getInternalRegistry().tables.size).to.equal(0);
  });

  it("rejects offset with after at runtime", () => {
    expect(() =>
      makeMv("mv", { after: "1 HOUR", offset: "5 MINUTE" } as any),
    ).to.throw(/materialized view 'mv'.*OFFSET is only valid with EVERY/);
    expect(getInternalRegistry().materializedViews.has("mv")).to.equal(false);
  });

  it("offset with after does not typecheck", () => {
    // @ts-expect-error OFFSET is only valid with EVERY
    const bad: RefreshConfig = { after: "1 HOUR", offset: "5 MINUTE" };
    expect(bad).to.be.an("object");
  });
});
