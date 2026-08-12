import { expect } from "chai";
import { OlapTable, WebApp } from "../src/dmv2/index";
import { getInternalRegistry, toInfraMap } from "../src/dmv2/internal";
import { sql } from "../src/index";

describe("Lineage Analysis", function () {
  this.timeout(30000);

  beforeEach(() => {
    const registry = getInternalRegistry();
    registry.tables.clear();
    registry.sqlResources.clear();
    registry.webApps.clear();
    registry.materializedViews.clear();
    registry.views.clear();
  });

  it("infers webapp lineage from handler call chains", () => {
    interface WebAppRow {
      id: string;
      value: number;
    }

    const table = new OlapTable<WebAppRow>("LineageWebAppTable");

    const readHelper = () => sql`SELECT ${table.columns.id} FROM ${table}`;
    const writeHelper = async () => {
      await table.insert([{ id: "1", value: 1 }]);
    };

    const app = {
      handle: async (_req: any, res: any) => {
        readHelper();
        await writeHelper();
        res.end("ok");
      },
    };

    new WebApp("lineageWebApp", app, { mountPath: "/lineage-webapp" });

    const infra = toInfraMap(getInternalRegistry());
    expect(infra.webApps.lineageWebApp.pullsDataFrom).to.deep.include({
      id: "LineageWebAppTable",
      kind: "Table",
    });
    expect(infra.webApps.lineageWebApp.pushesDataTo).to.deep.include({
      id: "LineageWebAppTable",
      kind: "Table",
    });
  });
});
