use localdesk_ipc::{RequestEnvelope, request_telemetry_snapshot};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or("XDG_RUNTIME_DIR is not set")?;
    let socket = runtime_dir.join("localdesk/appd.sock");
    let snapshot =
        request_telemetry_snapshot(&socket, RequestEnvelope::telemetry_snapshot()).await?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}
