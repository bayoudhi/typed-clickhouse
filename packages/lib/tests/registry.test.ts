/**
 * Test suite for the dmv2 registry functions
 */

import { expect } from "chai";
import {
  OlapTable,
  SqlResource,
  WebApp,
  getTables,
  getTable,
  getSqlResources,
  getSqlResource,
  getWebApps,
  getWebApp,
} from "../src/dmv2/index";
import { getInternalRegistry } from "../src/dmv2/internal";

describe("Registry Functions", () => {
  beforeEach(() => {
    // Clear the registry before each test
    const registry = getInternalRegistry();
    registry.tables.clear();
    registry.sqlResources.clear();
    registry.webApps.clear();
  });

  describe("Tables", () => {
    it("should register and retrieve tables", () => {
      interface TestData {
        id: string;
        value: number;
      }

      const table = new OlapTable<TestData>("TestTable", {
        orderByFields: ["id"],
      });

      const tables = getTables();
      expect(tables.size).to.equal(1);
      expect(tables.get("TestTable")).to.equal(table);

      const retrieved = getTable("TestTable");
      expect(retrieved).to.equal(table);
      expect(retrieved?.name).to.equal("TestTable");
    });

    it("should return undefined for non-existent table", () => {
      expect(getTable("NonExistent")).to.be.undefined;
    });
  });

  describe("SQL Resources", () => {
    it("should register and retrieve SQL resources", () => {
      interface TestData {
        id: string;
      }

      const table = new OlapTable<TestData>("TestTable");
      const resource = new SqlResource(
        "TestResource",
        ["CREATE VIEW test AS SELECT * FROM TestTable"],
        ["DROP VIEW test"],
        {
          pullsDataFrom: [table],
        },
      );

      const resources = getSqlResources();
      expect(resources.size).to.equal(1);
      expect(resources.get("TestResource")).to.equal(resource);

      const retrieved = getSqlResource("TestResource");
      expect(retrieved).to.equal(resource);
      expect(retrieved?.name).to.equal("TestResource");
    });

    it("should return undefined for non-existent SQL resource", () => {
      expect(getSqlResource("NonExistent")).to.be.undefined;
    });
  });

  describe("WebApps", () => {
    it("should register and retrieve web apps", () => {
      const handler = async () => {};
      const app = new WebApp("TestApp", handler, {
        mountPath: "/test",
      });

      const apps = getWebApps();
      expect(apps.size).to.equal(1);
      expect(apps.get("TestApp")).to.equal(app);

      const retrieved = getWebApp("TestApp");
      expect(retrieved).to.equal(app);
      expect(retrieved?.name).to.equal("TestApp");
    });

    it("should return undefined for non-existent web app", () => {
      expect(getWebApp("NonExistent")).to.be.undefined;
    });
  });
});
