use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let paths = fozmo::api::contracts::generate_contract_artifacts(repo_root)?;
    println!(
        "wrote {}, {}, and {}",
        paths.schema_path.display(),
        paths.types_path.display(),
        paths.endpoints_path.display()
    );
    Ok(())
}
