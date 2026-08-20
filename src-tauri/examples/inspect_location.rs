//! Reports what a node's database holds for incident location.
//!
//! Exists so a claim about a *running* node can be checked against the node
//! itself rather than against a test harness. It opens the database read-only,
//! writes nothing, and takes the path from `SECUREMESH_DATA_DIR`.
//!
//! ```text
//! SECUREMESH_DATA_DIR=%TEMP%\smA cargo run --example inspect_location
//! ```

fn main() {
    let dir = std::env::var("SECUREMESH_DATA_DIR").expect("set SECUREMESH_DATA_DIR");
    let path = std::path::Path::new(&dir).join("securemesh.sqlite");

    let conn = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open database read-only");

    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    println!("schema version : {version}");

    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(incidents)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    println!("location columns present: {}", {
        let wanted = ["accuracy_meters", "location_source", "location_captured_at"];
        wanted.iter().all(|c| columns.contains(&c.to_string()))
    });

    let mut statement = conn
        .prepare(
            "SELECT id, created_by, description, latitude, longitude,
                    accuracy_meters, location_source, location_captured_at
               FROM incidents ORDER BY created_at DESC",
        )
        .unwrap();

    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .unwrap();

    let mut count = 0;
    for row in rows {
        let (id, author, description, lat, lon, accuracy, source, captured) = row.unwrap();
        count += 1;
        println!("\n  {}  {description}", &id[..8]);
        println!("    author   : {}", &author[..16]);
        match (lat, lon) {
            (Some(lat), Some(lon)) => println!("    position : {lat:.6}, {lon:.6}"),
            _ => println!("    position : none recorded"),
        }
        println!(
            "    accuracy : {}",
            accuracy.map_or("not recorded".to_string(), |m| format!("±{m:.1} m"))
        );
        println!(
            "    source   : {}",
            source.as_deref().unwrap_or("not recorded")
        );
        println!(
            "    captured : {}",
            captured.as_deref().unwrap_or("not recorded")
        );
    }
    println!("\n{count} incident(s)");
}
