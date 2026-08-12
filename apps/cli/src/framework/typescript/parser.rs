use std::fs;

use serde_json::Value;
use tracing::debug;

use crate::project::Project;
use crate::utilities::constants::CLI_PROJECT_INTERNAL_DIR;

#[derive(Debug, thiserror::Error)]
#[error("Failed to parse the typescript file")]
#[non_exhaustive]
pub enum TypescriptParsingError {
    #[error("Failure setting up the file structure")]
    FileSystemError(#[from] std::io::Error),
    TypescriptCompilerError(Option<Box<dyn std::error::Error + Send + Sync>>),
    #[error("Typescript Parser - Unsupported data type in {field_name}: {type_name}")]
    UnsupportedDataTypeError {
        type_name: String,
        field_name: String,
    },

    #[error("Invalid output from compiler plugin. Possible incompatible versions between @typed-clickhouse/core and the CLI")]
    DeserializationError(#[from] serde_json::Error),

    OtherError {
        message: String,
    },

    #[error("TypeScript compilation failed: {0}")]
    CompilationError(String),
}

/// Default output directory for compiled TypeScript code.
pub const DEFAULT_OUT_DIR: &str = ".tch/compiled";

/// Reads the outDir from .tch/.compile-config.json (written by tch-tspc).
/// Falls back to default if the file doesn't exist.
///
/// This avoids duplicating the outDir resolution logic between Rust and TypeScript.
/// tch-tspc is the source of truth for where it compiles to.
fn read_compile_config_out_dir(project: &Project) -> Option<String> {
    let config_path = project
        .project_location
        .join(CLI_PROJECT_INTERNAL_DIR)
        .join(".compile-config.json");

    fs::read_to_string(&config_path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .and_then(|json| json.get("outDir")?.as_str().map(String::from))
}

/// Gets the path to the compiled index.js for a TypeScript project.
/// Reads from .tch/.compile-config.json (written by tch-tspc) to know where output is.
pub fn get_compiled_index_path(project: &Project) -> std::path::PathBuf {
    let out_dir =
        read_compile_config_out_dir(project).unwrap_or_else(|| DEFAULT_OUT_DIR.to_string());
    project
        .project_location
        .join(&out_dir)
        .join(&project.source_dir)
        .join("index.js")
}

/// Ensures TypeScript is compiled before loading infrastructure.
///
/// Runs tch-tspc to compile TypeScript. Respects user's tsconfig.json outDir
/// if specified, otherwise uses .tch/compiled.
///
/// This should be called before loading TypeScript infrastructure to ensure
/// compiled artifacts exist for the dmv2-serializer.
pub fn ensure_typescript_compiled(project: &Project) -> Result<(), TypescriptParsingError> {
    use crate::utilities::constants::IS_DEV_MODE;
    use std::sync::atomic::Ordering;

    // In dev mode, tspc --watch handles compilation. We don't need to run tspc ourselves
    // because spawn_and_await_initial_compile() already started it and the watcher keeps
    // it running for incremental compilation.
    if IS_DEV_MODE.load(Ordering::Relaxed) {
        debug!(
            "Skipping ensure_typescript_compiled: tspc --watch is handling compilation in dev mode"
        );
        return Ok(());
    }

    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin".to_string());
    let bin_path = format!(
        "{}/node_modules/.bin:{}",
        project.project_location.display(),
        path
    );

    // Don't pass outDir - let tch-tspc read from tsconfig or use default
    // Call tch-tspc directly (bin from @typed-clickhouse/core) instead of npx
    // since npx would try to find a standalone package named "tch-tspc"
    let output = std::process::Command::new("tch-tspc")
        .current_dir(&project.project_location)
        .env("TCH_SOURCE_DIR", &project.source_dir)
        .env("PATH", bin_path)
        .output()
        .map_err(|e| {
            TypescriptParsingError::CompilationError(format!("Failed to run tch-tspc: {}", e))
        })?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Check if output was generated despite errors (noEmitOnError: false)
        let compiled_index = get_compiled_index_path(project);
        if compiled_index.exists() {
            // Compilation succeeded with warnings
            debug!("TypeScript compiled with warnings: {}", stderr);
            Ok(())
        } else {
            Err(TypescriptParsingError::CompilationError(stderr.to_string()))
        }
    }
}
