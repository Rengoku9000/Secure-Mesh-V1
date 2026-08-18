//! Asks this machine for a position, and reports exactly what came back.
//!
//! Exists because "does this desktop have a usable location provider?" is a
//! question about the machine, not about SecureMesh, and it has to be answered
//! honestly before any claim is made about the feature working. It calls the
//! same provider the application uses, through the same trait.
//!
//! ```text
//! cargo run --example location_probe
//! ```

use securemesh_lib::location::{platform_provider, LocationPermission};

fn main() {
    let provider = platform_provider();

    println!("provider   : {}", provider.describe());
    println!("permission : {:?}  (before asking)", provider.permission());

    let requested = provider.request_permission();
    println!("permission : {requested:?}  (after asking)");

    if requested != LocationPermission::Granted {
        println!("\nNo position will be attempted: access is {requested:?}.");
        println!("This is a correct outcome, not a failure — the application");
        println!("reports it and still allows incidents without coordinates.");
        return;
    }

    println!("\ntaking one fix…");
    let started = std::time::Instant::now();
    match provider.current_location() {
        Ok(fix) => {
            println!("fix obtained in {} ms", started.elapsed().as_millis());
            println!("  latitude   : {:.6}", fix.latitude);
            println!("  longitude  : {:.6}", fix.longitude);
            println!(
                "  accuracy   : {}",
                match fix.accuracy_meters {
                    Some(metres) => format!("±{metres:.1} m"),
                    None => "not reported".to_string(),
                }
            );
            println!(
                "  altitude   : {}",
                match fix.altitude_meters {
                    Some(metres) => format!("{metres:.1} m"),
                    None => "not reported".to_string(),
                }
            );
            println!("  source     : {:?}", fix.source);
            println!("  captured   : {}", fix.captured_at);
            println!(
                "  offline-capable source: {}",
                if fix.source.works_offline() {
                    "yes — satellite"
                } else {
                    "NO — this position required the OS to reach the network"
                }
            );
        }
        Err(error) => {
            println!("no fix after {} ms", started.elapsed().as_millis());
            println!("reason: {}", error.message());
            println!("\nReported rather than substituted. Nothing is invented.");
        }
    }
}
