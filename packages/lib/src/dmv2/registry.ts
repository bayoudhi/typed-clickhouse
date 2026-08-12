/**
 * @module registry
 * Public registry functions for accessing the library Data Model v2 (dmv2) resources.
 *
 * This module provides functions to retrieve registered resources like tables,
 * SQL resources, and more. These functions are part of the public API and can be used by
 * user applications to inspect and interact with registered library resources.
 */

import { OlapTable } from "./sdk/olapTable";
import { SqlResource } from "./sdk/sqlResource";
import { WebApp } from "./sdk/webApp";
import { MaterializedView } from "./sdk/materializedView";
import { View } from "./sdk/view";
import { SelectRowPolicy } from "./sdk/selectRowPolicy";
import { getInternalRegistry } from "./internal";

/**
 * Get all registered OLAP tables.
 * @returns A Map of table name to OlapTable instance
 */
export function getTables(): Map<string, OlapTable<any>> {
  return getInternalRegistry().tables;
}

/**
 * Get a registered OLAP table by name.
 * @param name - The name of the table
 * @returns The OlapTable instance or undefined if not found
 */
export function getTable(name: string): OlapTable<any> | undefined {
  return getInternalRegistry().tables.get(name);
}

/**
 * Get all registered SQL resources.
 * @returns A Map of resource name to SqlResource instance
 */
export function getSqlResources(): Map<string, SqlResource> {
  return getInternalRegistry().sqlResources;
}

/**
 * Get a registered SQL resource by name.
 * @param name - The name of the SQL resource
 * @returns The SqlResource instance or undefined if not found
 */
export function getSqlResource(name: string): SqlResource | undefined {
  return getInternalRegistry().sqlResources.get(name);
}

/**
 * Get all registered web apps.
 * @returns A Map of web app name to WebApp instance
 */
export function getWebApps(): Map<string, WebApp> {
  return getInternalRegistry().webApps;
}

/**
 * Get a registered web app by name.
 * @param name - The name of the web app
 * @returns The WebApp instance or undefined if not found
 */
export function getWebApp(name: string): WebApp | undefined {
  return getInternalRegistry().webApps.get(name);
}

/**
 * Get all registered materialized views.
 * @returns A Map of MV name to MaterializedView instance
 */
export function getMaterializedViews(): Map<string, MaterializedView<any>> {
  return getInternalRegistry().materializedViews;
}

/**
 * Get a registered materialized view by name.
 * @param name - The name of the materialized view
 * @returns The MaterializedView instance or undefined if not found
 */
export function getMaterializedView(
  name: string,
): MaterializedView<any> | undefined {
  return getInternalRegistry().materializedViews.get(name);
}

/**
 * Get all registered views.
 * @returns A Map of view name to View instance
 */
export function getViews(): Map<string, View> {
  return getInternalRegistry().views;
}

/**
 * Get a registered view by name.
 * @param name - The name of the view
 * @returns The View instance or undefined if not found
 */
export function getView(name: string): View | undefined {
  return getInternalRegistry().views.get(name);
}

/**
 * Get all registered row policies.
 * @returns A Map of policy name to SelectRowPolicy instance
 */
export function getSelectRowPolicies(): Map<string, SelectRowPolicy> {
  return getInternalRegistry().selectRowPolicies;
}

/**
 * Get a registered row policy by name.
 * @param name - The name of the row policy
 * @returns The SelectRowPolicy instance or undefined if not found
 */
export function getSelectRowPolicy(name: string): SelectRowPolicy | undefined {
  return getInternalRegistry().selectRowPolicies.get(name);
}
