use std::path::Path;

pub mod auth;
pub mod ci_detection;
pub mod constants;
pub mod decode_object;
pub mod dotenv;
pub mod git;
pub mod identifiers;
pub mod json;
pub mod keyring;
pub mod machine_id;
pub mod nodejs_version;
pub mod package_managers;
pub mod process_output;
pub mod retry;
pub mod secrets;
pub mod system;

/// Converts an absolute file path to a relative path from the project root.
/// This is used to normalize metadata source file paths so they don't
/// contain developer-specific absolute paths.
pub fn normalize_path_string(absolute_path: &str, project_root: &Path) -> String {
    let path = Path::new(absolute_path);

    // Try to strip the project root prefix
    if let Ok(relative) = path.strip_prefix(project_root) {
        return relative.display().to_string();
    }

    // Fallback: Look for "app" directory and make relative from there
    // This matches the behavior in MCP's make_relative_path
    for ancestor in path.ancestors() {
        if ancestor.file_name().is_some_and(|name| name == "app") {
            if let Ok(relative) = path.strip_prefix(ancestor.parent().unwrap_or(ancestor)) {
                return relative.display().to_string();
            }
        }
    }

    // Last resort: return as-is
    absolute_path.to_string()
}

pub const fn _true() -> bool {
    true
}
