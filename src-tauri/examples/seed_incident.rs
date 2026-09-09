//! Development helper: create an incident in a node's data directory.
//!
//! Used to script two-node demos and to verify replication between running
//! application instances without driving the UI by hand.
//!
//! ```text
//! cargo run --example seed_incident -- <data-dir> <description> [severity] [--locate]
//! ```
//!
//! With `--locate` the incident carries a **real position taken from this
//! machine**, through the same `LocationProvider` the application uses.
//! Nothing is invented: if the provider cannot produce a fix, the incident is
//! recorded without coordinates rather than with a guessed one.
//!
//! ```text
//! (see usage above)
//! ```
//!
//! This is an example target, so it is **not** part of the shipped application
//! binary. Run it only against a node that is not currently running: a
//! SecureMesh node expects to be the sole writer of its own event log, and two
//! processes appending under one identity could race on sequence numbers.

use securemesh_lib::domain::{LocationSource, NewIncident};
use securemesh_lib::location::platform_provider;
use securemesh_lib::NodeRuntime;

fn main() {
    let mut args = std::env::args().skip(1);

    let (Some(data_dir), Some(description)) = (args.next(), args.next()) else {
        eprintln!(
            "usage: seed_incident <data-dir> <description> [severity] [--locate] [--at=<lat>,<lon>] [--accuracy=<metres>]"
        );
        std::process::exit(2);
    };
    let remaining: Vec<String> = args.collect();
    let locate = remaining.iter().any(|argument| argument == "--locate");

    // `--at <lat>,<lon>` records an operator-supplied position instead of
    // reading the sensor. Useful for staging a demonstration with incidents at
    // distinct places on one machine, where every live fix would be identical.
    // Validation still happens in the core; nothing here bypasses it.
    let explicit = remaining
        .iter()
        .find_map(|argument| argument.strip_prefix("--at="))
        .and_then(|value| {
            let (latitude, longitude) = value.split_once(',')?;
            Some((
                latitude.trim().parse::<f64>().ok()?,
                longitude.trim().parse::<f64>().ok()?,
            ))
        });
    // `--accuracy=<metres>` attaches an uncertainty radius to a hand-placed
    // position. The source stays UNKNOWN: a supplied radius says how coarse a
    // coordinate is, and nothing about what produced it. Only a real reading
    // through --locate can claim a source.
    let explicit_accuracy = remaining
        .iter()
        .find_map(|argument| argument.strip_prefix("--accuracy="))
        .and_then(|value| value.trim().parse::<f64>().ok());

    let severity = remaining
        .iter()
        .find(|argument| !argument.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "MEDIUM".to_string());

    // A real reading or none at all. The provider is asked once, exactly as
    // the UI asks it, and a failure is reported rather than substituted.
    let fix = if locate {
        let provider = platform_provider();
        provider.request_permission();
        match provider.current_location() {
            Ok(reading) => {
                println!(
                    "fix: {:.6}, {:.6}  accuracy {}  source {:?}",
                    reading.latitude,
                    reading.longitude,
                    reading
                        .accuracy_meters
                        .map(|m| format!("{m:.1} m"))
                        .unwrap_or_else(|| "unknown".to_string()),
                    reading.source
                );
                Some(reading)
            }
            Err(error) => {
                eprintln!(
                    "no position available ({}); recording without coordinates",
                    error.message()
                );
                None
            }
        }
    } else {
        None
    };

    // No transport: this writes to the log and exits. The running node picks
    // the event up from its own database and replicates it.
    let runtime = match NodeRuntime::initialize(&data_dir) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("could not open the node at {data_dir}: {error}");
            std::process::exit(1);
        }
    };

    match runtime.create_incident(NewIncident {
        description,
        severity,
        latitude: explicit
            .map(|(latitude, _)| latitude)
            .or_else(|| fix.as_ref().map(|reading| reading.latitude)),
        longitude: explicit
            .map(|(_, longitude)| longitude)
            .or_else(|| fix.as_ref().map(|reading| reading.longitude)),
        accuracy_meters: fix
            .as_ref()
            .and_then(|reading| reading.accuracy_meters)
            .or(explicit_accuracy),
        location_source: fix
            .as_ref()
            .map(|reading| LocationSource::from(reading.source))
            .or(explicit.map(|_| LocationSource::Unknown)),
        location_captured_at: fix.as_ref().map(|reading| reading.captured_at),
    }) {
        Ok(incident) => println!(
            "created {} on node {} ({})",
            incident.id,
            runtime.node_name(),
            incident.severity
        ),
        Err(error) => {
            eprintln!("could not create the incident: {error}");
            std::process::exit(1);
        }
    }
}
