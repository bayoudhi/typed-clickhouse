import { expect } from "chai";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { readProjectConfig, ConfigError } from "../src/config/configFile";

/**
 * Verifies that project config discovery mirrors the Rust CLI's fallback
 * behaviour (see `apps/cli/src/project.rs`): `tch.config.toml` is preferred,
 * `moose.config.toml` is read when the new name is absent, and neither being
 * present produces a clear error naming both files.
 */
describe("readProjectConfig config file fallback", function () {
  let tempDir: string;
  let originalCwd: string;

  const minimalConfig = `
language = "typescript"

[clickhouse_config]
host = "localhost"
host_port = 18123
user = "default"
password = ""
db_name = "local"
`;

  beforeEach(() => {
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "tch-config-fallback-"));
    originalCwd = process.cwd();
    process.chdir(tempDir);
  });

  afterEach(() => {
    process.chdir(originalCwd);
    fs.rmSync(tempDir, { recursive: true, force: true });
  });

  it("finds tch.config.toml when only the new name is present", async () => {
    fs.writeFileSync(
      path.join(tempDir, "tch.config.toml"),
      minimalConfig.replace('db_name = "local"', 'db_name = "new_name"'),
    );

    const config = await readProjectConfig();
    expect(config.clickhouse_config.db_name).to.equal("new_name");
  });

  it("falls back to moose.config.toml when the new name is absent", async () => {
    fs.writeFileSync(
      path.join(tempDir, "moose.config.toml"),
      minimalConfig.replace('db_name = "local"', 'db_name = "legacy_name"'),
    );

    const config = await readProjectConfig();
    expect(config.clickhouse_config.db_name).to.equal("legacy_name");
  });

  it("prefers tch.config.toml when both names are present", async () => {
    fs.writeFileSync(
      path.join(tempDir, "tch.config.toml"),
      minimalConfig.replace('db_name = "local"', 'db_name = "new_name"'),
    );
    fs.writeFileSync(
      path.join(tempDir, "moose.config.toml"),
      minimalConfig.replace('db_name = "local"', 'db_name = "legacy_name"'),
    );

    const config = await readProjectConfig();
    expect(config.clickhouse_config.db_name).to.equal("new_name");
  });

  it("throws a ConfigError naming both files when neither is present", async () => {
    try {
      await readProjectConfig();
      expect.fail("expected readProjectConfig to throw");
    } catch (error) {
      expect(error).to.be.instanceOf(ConfigError);
      const message = (error as ConfigError).message;
      expect(message).to.include("tch.config.toml");
      expect(message).to.include("moose.config.toml");
    }
  });
});
