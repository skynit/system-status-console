use localdesk_network::NetworkMonitor;
use std::{thread, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut monitor = NetworkMonitor::default();
    let first = monitor.collect()?;
    thread::sleep(Duration::from_secs(1));
    let second = monitor.collect()?;

    println!(
        "system={:?}/{} per_app={:?}/{} coverage={:?}",
        second.system_traffic.status,
        second.system_traffic.reason,
        second.per_application.status,
        second.per_application.reason,
        second.coverage
    );
    println!(
        "totals={:?} aggregate_rate={:?}",
        second.totals, second.aggregate_rate
    );
    for interface in second.interfaces {
        println!(
            "interface={:?} counters={:?} rate={:?}",
            interface.interface, interface.interface.counters, interface.rate
        );
    }
    println!("first_sample_events={:?}", first.events);
    Ok(())
}
