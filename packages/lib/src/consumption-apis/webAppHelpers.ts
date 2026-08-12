import http from "http";
import type { HandlerUtils } from "./helpers";

/**
 * @deprecated Use `getHandlerUtils()` from '@typed-clickhouse/core' instead.
 *
 * This synchronous function extracts HandlerUtils from a request object that was
 * injected by the library runtime middleware. It returns undefined if not running
 * in a the library-managed context.
 *
 * Migration: Replace with the async version:
 * ```typescript
 * // Old (sync, deprecated):
 * import { getHandlerUtilsFromRequest } from '@typed-clickhouse/core';
 * const tch = getHandlerUtilsFromRequest(req);
 *
 * // New (async, recommended):
 * import { getHandlerUtils } from '@typed-clickhouse/core';
 * const tch = await getHandlerUtils();
 * ```
 *
 * @param req - The HTTP request object containing injected tch utilities
 * @returns HandlerUtils if available on the request, undefined otherwise
 */
export function getHandlerUtilsFromRequest(
  req: http.IncomingMessage | any,
): HandlerUtils | undefined {
  console.warn(
    "[DEPRECATED] getHandlerUtilsFromRequest() is deprecated. " +
      "Import getHandlerUtils from '@typed-clickhouse/core' and call it without parameters: " +
      "const { client, sql } = await getHandlerUtils();",
  );
  return (req as any).tch;
}

/**
 * @deprecated Use `getHandlerUtils()` from '@typed-clickhouse/core' instead.
 *
 * This is a legacy alias for getHandlerUtilsFromRequest. The main getHandlerUtils
 * export from '@typed-clickhouse/core' is now async and does not require a request parameter.
 *
 * BREAKING CHANGE WARNING: The new getHandlerUtils() returns Promise<HandlerUtils>,
 * not HandlerUtils | undefined. You must await the result:
 * ```typescript
 * const tch = await getHandlerUtils(); // New async API
 * ```
 */
export const getLegacyHandlerUtils = getHandlerUtilsFromRequest;

/**
 * @deprecated No longer needed. Use getHandlerUtils() directly instead.
 * the library now handles utility injection automatically when injectHandlerUtils is true.
 */
export function expressMiddleware() {
  console.warn(
    "[DEPRECATED] expressMiddleware() is deprecated. " +
      "Use getHandlerUtils() directly or rely on injectHandlerUtils config.",
  );
  return (req: any, res: any, next: any) => {
    // Maintain backwards compat: copy req.raw.tch to req.tch if present
    if (!req.tch && req.raw && (req.raw as any).tch) {
      req.tch = (req.raw as any).tch;
    }
    next();
  };
}

/**
 * @deprecated Use HandlerUtils from helpers.ts instead.
 */
export interface ExpressRequestWithHandlerUtils {
  tch?: HandlerUtils;
}
