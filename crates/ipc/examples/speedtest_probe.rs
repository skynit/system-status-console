use localdesk_domain::{
    Iperf3Direction, SpeedTestDeepCommand, SpeedTestStageData,
};
use localdesk_ipc::{
    RequestEnvelope, request_speedtest_basic, request_speedtest_deep,
};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or("XDG_RUNTIME_DIR is not set")?;
    let socket = runtime_dir.join("localdesk/appd.sock");

    let mode = std::env::args().nth(1).unwrap_or_else(|| "basic".to_owned());
    match mode.as_str() {
        "basic" => {
            let end = request_speedtest_basic(&socket, RequestEnvelope::speedtest_basic(), |stage| {
                let label = match stage {
                    SpeedTestStageData::Latency { .. } => "latency",
                    SpeedTestStageData::Bandwidth { .. } => "bandwidth",
                    SpeedTestStageData::IpPurity { .. } => "ip_purity",
                };
                println!("STAGE {label}");
                Ok(())
            })
            .await?;
            println!("RESULT {}", serde_json::to_string_pretty(&end)?);
        }
        "wifi" => {
            let output = request_speedtest_deep(
                &socket,
                RequestEnvelope::speedtest_deep(SpeedTestDeepCommand::WifiScan),
            )
            .await?;
            println!("RESULT {}", serde_json::to_string_pretty(&output)?);
        }
        "linssid" => {
            let output = request_speedtest_deep(
                &socket,
                RequestEnvelope::speedtest_deep(SpeedTestDeepCommand::LinssidLaunch),
            )
            .await?;
            println!("RESULT {}", serde_json::to_string_pretty(&output)?);
        }
        "iperf3" => {
            let duration = std::env::args()
                .nth(2)
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or(3);
            let output = request_speedtest_deep(
                &socket,
                RequestEnvelope::speedtest_deep(SpeedTestDeepCommand::Iperf3Start {
                    server: "127.0.0.1".to_owned(),
                    port: 5201,
                    direction: Iperf3Direction::Download,
                    duration_secs: duration,
                    parallel: 1,
                }),
            )
            .await?;
            println!("RESULT {}", serde_json::to_string_pretty(&output)?);
        }
        "stop" => {
            let output = request_speedtest_deep(
                &socket,
                RequestEnvelope::speedtest_deep(SpeedTestDeepCommand::Iperf3Stop),
            )
            .await?;
            println!("RESULT {}", serde_json::to_string_pretty(&output)?);
        }
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    }
    Ok(())
}
