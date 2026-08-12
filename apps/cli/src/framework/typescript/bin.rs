use crate::utilities::constants::{CLI_VERSION, TSCONFIG_JSON};
use std::sync::OnceLock;
use std::{env, path::Path, process::Stdio};
use tracing::info;

use crate::project::Project;
use tokio::process::{Child, Command};

const RUNNER_COMMAND: &str = "tch-runner";

static VERSION_CHECK_RESULT: OnceLock<Result<(), String>> = OnceLock::new();

fn bin_path(project_path: &Path) -> String {
    let path = env::var("PATH").unwrap_or_else(|_| "/usr/local/bin".to_string());
    format!(
        "{}/node_modules/.bin:{}",
        project_path.to_str().unwrap(),
        path
    )
}

/// Runs `tch-runner print-version` and compares the installed library version
/// against the CLI version. Skips the check for dev builds.
fn check_library_version(project_path: &Path) -> Result<(), String> {
    if CLI_VERSION == "0.0.1" || CLI_VERSION.contains("dev") {
        return Ok(());
    }

    let output = std::process::Command::new(RUNNER_COMMAND)
        .arg("print-version")
        .env("PATH", bin_path(project_path))
        .env("NODE_NO_WARNINGS", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run tch-runner print-version: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("unknown command") {
            return Err(format!(
                "Version mismatch: installed @typed-clickhouse/core does not support version checking \
                 and is older than the CLI ({CLI_VERSION}). \
                 Please install @typed-clickhouse/core@{CLI_VERSION}."
            ));
        }
        return Err(format!(
            "Failed to check @typed-clickhouse/core version: {stderr}"
        ));
    }

    let lib_version = String::from_utf8_lossy(&output.stdout);
    let lib_version = lib_version.trim();
    if lib_version != CLI_VERSION {
        return Err(format!(
            "Version mismatch: installed @typed-clickhouse/core is {lib_version}, \
             but the CLI is {CLI_VERSION}. \
             Please install @typed-clickhouse/core@{CLI_VERSION}."
        ));
    }

    info!(
        "@typed-clickhouse/core version check passed: {}",
        lib_version
    );
    Ok(())
}

pub fn run(
    binary_command: &str,
    project_path: &Path,
    args: &[&str],
    project: &Project,
) -> Result<Child, std::io::Error> {
    let check = VERSION_CHECK_RESULT.get_or_init(|| check_library_version(project_path));
    if let Err(msg) = check {
        return Err(std::io::Error::other(msg.clone()));
    }

    let mut command = Command::new(RUNNER_COMMAND);

    command.arg(binary_command);

    command
        .env("TS_NODE_PROJECT", project_path.join(TSCONFIG_JSON))
        .env("PATH", bin_path(project_path))
        .env("TS_NODE_COMPILER_HOST", "true")
        .env("NODE_NO_WARNINGS", "1")
        .env("TS_NODE_EMIT", "true")
        .env("TCH_SOURCE_DIR", &project.source_dir);

    // Set IS_LOADING_INFRA_MAP=true only when loading infrastructure map
    // This allows runtimeEnv.get() to return markers for later resolution
    // For runtime execution (functions/workflows), it will return actual env var values
    if binary_command == "dmv2-serializer" {
        command.env("IS_LOADING_INFRA_MAP", "true");
    }

    for arg in args {
        command.arg(arg);
    }

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}
