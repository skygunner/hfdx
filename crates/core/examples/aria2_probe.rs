use std::path::Path;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let receipt = hfdx_core::run_aria2_probe(Path::new("aria2c")).await?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(())
}
