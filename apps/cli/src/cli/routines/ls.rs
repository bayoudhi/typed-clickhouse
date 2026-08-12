//! Module for listing available resources in this tool.
//!
//! This module provides functionality to list the ClickHouse resources (tables
//! and raw SQL resources) defined by the project.

use super::{RoutineFailure, RoutineSuccess};
use crate::framework::core::infrastructure_map::InfrastructureMap;
use crate::{
    cli::display::{show_table, Message},
    project::Project,
};
use itertools::Itertools;
use serde::Serialize;
use serde_json::Error;

#[derive(Debug, Serialize)]
pub struct TableInfo {
    pub name: String,
    pub schema_fields: Vec<String>,
}

impl ResourceInfo for Vec<TableInfo> {
    fn show(&self) {
        show_table(
            "Tables".to_string(),
            vec!["name".to_string(), "schema_fields".to_string()],
            self.iter()
                .map(|t| vec![t.name.clone(), t.schema_fields.iter().join(", ")])
                .collect(),
        )
    }
    fn to_json_string(&self) -> Result<String, Error> {
        serde_json::to_string_pretty(&self)
    }
}

// Note: From trait removed because Table::id() now requires default_database parameter.
// TableInfo is constructed directly where needed with the appropriate default_database.

#[derive(Debug, Serialize)]
pub struct SqlResourceInfo {
    pub name: String,
}

impl ResourceInfo for Vec<SqlResourceInfo> {
    fn show(&self) {
        show_table(
            "SQL Resources".to_string(),
            vec!["name".to_string()],
            self.iter()
                .map(|resource| vec![resource.name.clone()])
                .collect(),
        )
    }
    fn to_json_string(&self) -> Result<String, Error> {
        serde_json::to_string_pretty(&self)
    }
}

#[derive(Debug, Serialize)]
pub struct ResourceListing {
    pub tables: Vec<TableInfo>,
    pub sql_resources: Vec<SqlResourceInfo>,
}

impl ResourceInfo for ResourceListing {
    fn show(&self) {
        self.tables.show();
        self.sql_resources.show();
    }

    fn to_json_string(&self) -> Result<String, Error> {
        serde_json::to_string_pretty(&self)
    }
}

pub async fn ls(
    project: &Project,
    _type: Option<&str>,
    name: Option<&str>,
    json: bool,
) -> Result<RoutineSuccess, RoutineFailure> {
    // Don't resolve credentials for ls command - only inspects structure
    let infra_map = InfrastructureMap::load_from_user_code(project, false)
        .await
        .map_err(|e| {
            RoutineFailure::new(
                Message {
                    action: "Load".to_string(),
                    details: "Infrastructure".to_string(),
                },
                e,
            )
        })?;

    let default_database = infra_map.default_database.clone();

    let resources = ResourceListing {
        tables: infra_map
            .tables
            .into_values()
            .filter(|table| name.is_none_or(|name| table.name.contains(name)))
            .map(|t| TableInfo {
                name: t.id(&default_database),
                schema_fields: t.columns.iter().map(|col| col.name.clone()).collect(),
            })
            .collect(),
        sql_resources: infra_map
            .sql_resources
            .into_values()
            .filter(|resource| name.is_none_or(|name| resource.name.contains(name)))
            .map(|resource| SqlResourceInfo {
                name: resource.name,
            })
            .collect(),
    };
    let listing: &dyn ResourceInfo = match _type {
        None => &resources,
        Some("tables") => &resources.tables,
        Some("sql_resource") => &resources.sql_resources,
        _ => {
            return Err(RoutineFailure::error(Message::new(
                "Unknown".to_string(),
                "type".to_string(),
            )))
        }
    };
    if json {
        println!("{}", listing.to_json_string().unwrap());
    } else {
        listing.show();
    }

    Ok(RoutineSuccess::success(Message {
        action: "".to_string(),
        details: "".to_string(),
    }))
}

trait ResourceInfo {
    fn show(&self);
    fn to_json_string(&self) -> Result<String, serde_json::error::Error>;
}
