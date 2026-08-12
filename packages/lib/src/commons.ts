import {
  existsSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "fs";
import nodePath from "path";
import { createClient } from "@clickhouse/client";

/**
 * Utility function for compiler-related logging that can be disabled via environment variable.
 * Set TCH_DISABLE_COMPILER_LOGS=true to suppress these logs (useful for testing environments).
 */

/**
 * Returns true if the value is a common truthy string: "1", "true", "yes", "on" (case-insensitive).
 */
function isTruthy(value: string | undefined): boolean {
  if (!value) return false;
  switch (value.trim().toLowerCase()) {
    case "1":
    case "true":
    case "yes":
    case "on":
      return true;
    default:
      return false;
  }
}

export const compilerLog = (message: string) => {
  if (!isTruthy(process.env.TCH_DISABLE_COMPILER_LOGS)) {
    console.log(message);
  }
};

export const antiCachePath = (path: string) =>
  `${path}?num=${Math.random().toString()}&time=${Date.now()}`;

export const getFileName = (filePath: string) => {
  const regex = /\/([^\/]+)\.ts/;
  const matches = filePath.match(regex);
  if (matches && matches.length > 1) {
    return matches[1];
  }
  return "";
};

interface ClientConfig {
  username: string;
  password: string;
  database: string;
  useSSL: string;
  host: string;
  port: string;
}

export const getClickhouseClient = ({
  username,
  password,
  database,
  useSSL,
  host,
  port,
}: ClientConfig) => {
  const protocol =
    useSSL === "1" || useSSL.toLowerCase() === "true" ? "https" : "http";
  console.log(`Connecting to Clickhouse at ${protocol}://${host}:${port}`);
  return createClient({
    url: `${protocol}://${host}:${port}`,
    username: username,
    password: password,
    database: database,
    application: "tch",
    // Note: wait_end_of_query is configured per operation type, not globally
    // to preserve SELECT query performance while ensuring INSERT/DDL reliability
  });
};

export type CliLogData = {
  message_type?: "Info" | "Success" | "Warning" | "Error" | "Highlight";
  action: string;
  message: string;
};

export const cliLog: (log: CliLogData) => void = (log) => {
  const level =
    log.message_type === "Error" ? "error"
    : log.message_type === "Warning" ? "warn"
    : "info";

  const structuredLog = {
    __tch_structured_log__: true,
    level,
    message: log.message,
    resource_type: "runtime",
    cli_action: log.action,
    cli_message_type: log.message_type ?? "Info",
    timestamp: new Date().toISOString(),
  };

  process.stderr.write(JSON.stringify(structuredLog) + "\n");
};

/**
 * Method to change .ts, .cts, and .mts to .js, .cjs, and .mjs
 * This is needed because 'import' does not support .ts, .cts, and .mts
 */
export function mapTstoJs(filePath: string): string {
  return filePath
    .replace(/\.ts$/, ".js")
    .replace(/\.cts$/, ".cjs")
    .replace(/\.mts$/, ".mjs");
}

/**
 * Walks a directory recursively and returns all files matching the given extensions.
 * Skips node_modules directories.
 *
 * @param dir - Directory to walk
 * @param extensions - File extensions to include (e.g., [".js", ".mjs"])
 * @returns Array of file paths
 */
function walkDirectory(dir: string, extensions: string[]): string[] {
  const results: string[] = [];

  if (!existsSync(dir)) {
    return results;
  }

  try {
    const entries = readdirSync(dir, { withFileTypes: true });

    for (const entry of entries) {
      const fullPath = nodePath.join(dir, entry.name);

      if (entry.isDirectory()) {
        // Skip node_modules
        if (entry.name !== "node_modules") {
          results.push(...walkDirectory(fullPath, extensions));
        }
      } else if (entry.isFile()) {
        const ext = nodePath.extname(entry.name);
        if (extensions.includes(ext)) {
          results.push(fullPath);
        }
      }
    }
  } catch (e) {
    // Log error in case it is something the user can/should act on
    console.debug(`[tch] Failed to read directory ${dir}:`, e);
  }

  return results;
}

/**
 * Adds .js extension to relative import paths that don't have an extension.
 * Handles import/export from statements and dynamic imports.
 *
 * @param content - JavaScript file content
 * @param fileDir - Directory containing the file being processed (for resolving paths)
 * @returns Content with .js extensions added to relative imports
 */
function addJsExtensionToImports(content: string, fileDir?: string): string {
  // Pattern for 'from' statements with relative paths
  // Matches: from './foo', from "../bar", from './utils/helper'
  const fromPattern = /(from\s+['"])(\.\.?\/[^'"]*?)(['"])/g;

  // Pattern for side-effect-only imports with relative paths
  // Matches: import './foo', import "../bar" (no 'from' keyword, just importing for side effects)
  const bareImportPattern = /(import\s+['"])(\.\.?\/[^'"]*?)(['"])/g;

  // Pattern for dynamic imports with relative paths
  // Matches: import('./foo'), import("../bar")
  const dynamicPattern = /(import\s*\(\s*['"])(\.\.?\/[^'"]*?)(['"])/g;

  let result = content;

  // Process 'from' statements
  result = result.replace(fromPattern, (match, prefix, importPath, quote) => {
    return rewriteImportPath(match, prefix, importPath, quote, fileDir);
  });

  // Process side-effect-only imports
  result = result.replace(
    bareImportPattern,
    (match, prefix, importPath, quote) => {
      return rewriteImportPath(match, prefix, importPath, quote, fileDir);
    },
  );

  // Process dynamic imports
  result = result.replace(
    dynamicPattern,
    (match, prefix, importPath, quote) => {
      return rewriteImportPath(match, prefix, importPath, quote, fileDir);
    },
  );

  return result;
}

/**
 * Rewrites a single import path to add .js extension if needed.
 * Handles directory imports with index files correctly.
 *
 * @param match - The full matched string
 * @param prefix - The prefix (from ' or import(')
 * @param importPath - The import path
 * @param quote - The closing quote
 * @param fileDir - Directory containing the file being processed (for resolving paths)
 * @returns The rewritten import or original if no change needed
 */
function rewriteImportPath(
  match: string,
  prefix: string,
  importPath: string,
  quote: string,
  fileDir?: string,
): string {
  // Skip if already has a JavaScript extension
  if (/\.[cm]?js$/.test(importPath)) {
    return match;
  }

  // Skip if importing a JSON file
  if (/\.json$/.test(importPath)) {
    return match;
  }

  // If we have the file directory, check if the import target is a directory with index.js
  if (fileDir) {
    const resolvedPath = nodePath.resolve(fileDir, importPath);

    // Check if path.js exists (regular file import)
    if (existsSync(`${resolvedPath}.js`)) {
      return `${prefix}${importPath}.js${quote}`;
    }

    // Check if path/index.js exists (directory import)
    if (existsSync(nodePath.join(resolvedPath, "index.js"))) {
      // For directory imports, rewrite to include /index.js
      return `${prefix}${importPath}/index.js${quote}`;
    }

    // Check for .mjs variants
    if (existsSync(`${resolvedPath}.mjs`)) {
      return `${prefix}${importPath}.mjs${quote}`;
    }
    if (existsSync(nodePath.join(resolvedPath, "index.mjs"))) {
      return `${prefix}${importPath}/index.mjs${quote}`;
    }

    // Check for .cjs variants
    if (existsSync(`${resolvedPath}.cjs`)) {
      return `${prefix}${importPath}.cjs${quote}`;
    }
    if (existsSync(nodePath.join(resolvedPath, "index.cjs"))) {
      return `${prefix}${importPath}/index.cjs${quote}`;
    }
  }

  // Default: add .js extension (fallback when no fileDir or file not found)
  return `${prefix}${importPath}.js${quote}`;
}

/**
 * Rewrites relative import paths in JavaScript files to include .js extensions.
 * This is required for Node.js ESM which requires explicit extensions.
 *
 * Handles:
 * - import statements: import { foo } from './bar' -> import { foo } from './bar.js'
 * - dynamic imports: import('./bar') -> import('./bar.js')
 * - re-exports: export { foo } from './bar' -> export { foo } from './bar.js'
 *
 * Does NOT modify:
 * - Package imports (no leading . or ..)
 * - Imports that already have extensions
 * - Imports from node_modules
 *
 * @param outDir - Directory containing compiled JavaScript files
 */
export function rewriteImportExtensions(outDir: string): void {
  const files = walkDirectory(outDir, [".js", ".mjs"]);

  for (const filePath of files) {
    const content = readFileSync(filePath, "utf-8");
    const fileDir = nodePath.dirname(filePath);
    const rewritten = addJsExtensionToImports(content, fileDir);

    if (content !== rewritten) {
      writeFileSync(filePath, rewritten, "utf-8");
    }
  }
}

export const MAX_RETRIES = 150;
export const MAX_RETRY_TIME_MS = 1000;
export const RETRY_INITIAL_TIME_MS = 100;

export const MAX_RETRIES_PRODUCER = 150;
export const RETRY_FACTOR_PRODUCER = 0.2;
// Means all replicas need to acknowledge the message
export const ACKs = -1;

/**
 * Interface for logging functionality
 */
export interface Logger {
  logPrefix: string;
  log: (message: string) => void;
  error: (message: string) => void;
  warn: (message: string) => void;
}

export const logError = (logger: Logger, e: Error): void => {
  logger.error(e.message);
  const stack = e.stack;
  if (stack) {
    logger.error(stack);
  }
};
