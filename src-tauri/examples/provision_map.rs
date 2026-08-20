//! Verifies the offline map data an operator has installed.
//!
//! ```text
//! npm run map:provision
//! ```
//!
//! **This downloads nothing.** It checks what is already on the machine and
//! reports it. A provisioning step that fetched data would defeat the point of
//! an offline map, and would make "no request leaves this machine" a claim this
//! project could not make.
//!
//! Validation is `crate::map::load`, the same function the running node uses,
//! so this command and the application cannot disagree about whether a basemap
//! is usable.

use securemesh_lib::map;
use securemesh_lib::NodeRuntime;

/// Margin added around a node's own incidents when reporting what a basemap
/// needs to cover, so an incident is never at the very edge of its geography.
const COVERAGE_MARGIN_KM: f64 = 10.0;

/// The ground this node actually has records on.
///
/// Read from the database rather than assumed, so the region follows the
/// deployment instead of being decided in advance by this project. A node with
/// no located incidents yields nothing, and the check reports what is installed
/// without forming a coverage opinion.
fn required_coverage(data_dir: &str) -> Option<map::BoundingBox> {
    let runtime = NodeRuntime::initialize(data_dir).ok()?;
    let positions: Vec<(f64, f64)> = runtime
        .list_incidents(None)
        .ok()?
        .into_iter()
        .filter_map(|incident| match (incident.latitude, incident.longitude) {
            (Some(latitude), Some(longitude)) => Some((latitude, longitude)),
            _ => None,
        })
        .collect();

    map::required_coverage(&positions, COVERAGE_MARGIN_KM)
}

fn main() {
    println!("SecureMesh — offline map provisioning check\n");

    // Optional: a node data directory whose own incidents define the region
    // a basemap has to cover.
    let data_dir = std::env::args().nth(1);

    let Some(root) = map::map_root() else {
        let expected = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("map")))
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "map/".to_string());

        println!("LOCAL MAP");
        println!("Unavailable — map data not provisioned\n");
        println!("No `map` directory was found.");
        println!("Searched upward from the executable, expecting something like:");
        println!("  {expected}");
        println!("\nCreate it and place a GeoJSON basemap inside:");
        println!("  map/{}", map::BASEMAP_FILE);
        println!("\nSee docs/map/PROVISIONING.md.");
        std::process::exit(1);
    };

    println!("map directory : {}", root.display());
    println!(
        "expected file : {}\n",
        root.join(map::BASEMAP_FILE).display()
    );

    // Reported before anything else: this is what an operator needs in order
    // to extract the right region, and it is just as useful when no basemap
    // is installed yet as when one is.
    let needed = data_dir.as_deref().and_then(required_coverage);
    match needed {
        Some(box_) => println!(
            "node records  : {:.6}, {:.6}  to  {:.6}, {:.6}  (needs coverage)
",
            box_.min_latitude, box_.min_longitude, box_.max_latitude, box_.max_longitude
        ),
        None => println!(
            "node records  : no located incidents
"
        ),
    }
    match map::load() {
        Ok((basemap, _geojson)) => {
            println!("LOCAL MAP");
            println!("Ready\n");
            println!("  file      : {}", basemap.name);
            println!("  path      : {}", basemap.path);
            println!(
                "  size      : {:.2} MB ({} bytes)",
                basemap.bytes as f64 / (1024.0 * 1024.0),
                basemap.bytes
            );
            println!("  features  : {}", basemap.feature_count);
            println!("  sha256    : {}", basemap.sha256);
            println!(
                "  coverage  : {:.6}, {:.6}  to  {:.6}, {:.6}  (lat, lon)",
                basemap.bounds.min_latitude,
                basemap.bounds.min_longitude,
                basemap.bounds.max_latitude,
                basemap.bounds.max_longitude
            );
            println!(
                "\nOnly incidents inside that box will have a basemap behind them.\n\
                 Markers outside it still render, on the coordinate grid."
            );

            // Checked against real records rather than asserted. A basemap
            // for the wrong region still renders, so it looks correct until
            // someone notices the roads do not match the ground.
            if let Some(required) = needed {
                if basemap.bounds.contains(&required) {
                    println!(
                        "
coverage  : OK - every located incident has geography behind it"
                    );
                } else {
                    println!(
                        "
coverage  : INCOMPLETE - some incidents fall outside the basemap"
                    );
                    std::process::exit(1);
                }
            }
        }
        Err(reason) => {
            println!("LOCAL MAP");
            if reason.is_absence() {
                println!("Unavailable — map data not provisioned\n");
            } else {
                println!("Error — map data is present but unusable\n");
            }
            println!("{}", reason.detail());
            println!(
                "\nThe application still runs: incidents and this node are drawn\n\
                 on a coordinate grid, and every other subsystem is unaffected."
            );
            std::process::exit(1);
        }
    }
}
