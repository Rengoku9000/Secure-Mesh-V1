//! Development helper: create an incident in a node's data directory.
//!
//! Used to script two-node demos and to verify replication between running
//! application instances without driving the UI by hand.
//!
//! ```text
//! cargo run --example seed_incident -- <data-dir> <description> [severity]
//! ```
//!
//! This is an example target, so it is **not** part of the shipped application
//! binary. Run it only against a node that is not currently running: a
//! SecureMesh node expects to be the sole writer of its own event log, and two
//! processes appending under one identity could race on sequence numbers.

use securemesh_lib::domain::NewIncident;
use securemesh_lib::NodeRuntime;

fn main() {
    let mut args = std::env::args().skip(1);

    let (Some(data_dir), Some(description)) = (args.next(), args.next()) else {
        eprintln!("usage: seed_incident <data-dir> <description> [severity]");
        std::process::exit(2);
    };
    let severity = args.next().unwrap_or_else(|| "MEDIUM".to_string());

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
        latitude: None,
        longitude: None,
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
