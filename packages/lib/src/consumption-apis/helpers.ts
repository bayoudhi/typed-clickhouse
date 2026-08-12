import { JWTPayload } from "jose";
import { sql } from "../sqlHelpers";
export { joinQueries } from "../sqlHelpers";
import { QueryClient } from "./query-client";
export { QueryClient, type RowPolicyOptions } from "./query-client";
export {
  MOOSE_RLS_ROLE,
  MOOSE_RLS_USER,
  MOOSE_RLS_SETTING_PREFIX,
  buildRowPolicyOptionsFromClaims,
  type RowPoliciesConfig,
} from "../rls-constants";

/**
 * Utilities provided by getHandlerUtils() for database access and SQL queries.
 * Works in both the library runtime and standalone contexts.
 */
export interface HandlerUtils {
  client: ResourceClient;
  sql: typeof sql;
  jwt?: JWTPayload;
}

/**
 * @deprecated Use HandlerUtils instead. ApiUtil is now a type alias to HandlerUtils
 * and will be removed in a future version.
 *
 * Migration: Replace `ApiUtil` with `HandlerUtils` in your type annotations.
 */
export type ApiUtil = HandlerUtils;

/** @deprecated Use HandlerUtils instead. */
export type ConsumptionUtil = HandlerUtils;

export class ResourceClient {
  query: QueryClient;

  constructor(queryClient: QueryClient) {
    this.query = queryClient;
  }
}

export const ApiHelpers = {
  column: (value: string) => ["Identifier", value] as [string, string],
  table: (value: string) => ["Identifier", value] as [string, string],
};

/** @deprecated Use ApiHelpers instead. */
export const ConsumptionHelpers = ApiHelpers;
