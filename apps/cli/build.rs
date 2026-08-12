use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=../../packages/protobuf");

    // Allow overriding the CLI version with MOOSE_CLI_VERSION environment variable
    // This is used during CI builds to inject the full version string including -ci- part
    // while keeping Cargo.toml with a simpler version for maturin compatibility
    let cli_version = std::env::var("MOOSE_CLI_VERSION")
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo:rustc-env=MOOSE_CLI_VERSION={cli_version}");
    println!("cargo:rerun-if-env-changed=MOOSE_CLI_VERSION");

    // Generate protobuf code
    std::fs::create_dir_all("src/proto/")?;
    protobuf_codegen::Codegen::new()
        .pure()
        .includes(["../../packages/protobuf"])
        .input("../../packages/protobuf/infrastructure_map.proto")
        .out_dir("src/proto/")
        .run_from_script();

    Ok(())
}
