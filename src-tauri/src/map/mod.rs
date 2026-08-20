//! Offline geographic data for the tactical map.
//!
//! # What this is, and what it deliberately is not
//!
//! This module answers one question — *what geography does this node hold
//! locally?* — and hands the answer to the UI, which draws it. It is a
//! **visualization input**, never a source of truth. Incidents remain
//! authoritative in SQLite, position remains the `LocationProvider`'s
//! responsibility, and nothing here writes to either.
//!
//! # Nothing here reaches the network
//!
//! There is no tile server, no style URL, no glyph or sprite URL, no geocoder
//! and no API key — because there is no HTTP client in this module at all. A
//! basemap is a file an operator put on the machine, read from disk, parsed,
//! and handed to the renderer. That is the whole data path, and it is why the
//! offline claim is structural rather than a promise.
//!
//! The renderer is SVG drawn by SecureMesh's own code. That choice was forced
//! by the application's Content-Security-Policy, which is
//! `default-src 'self'` with no `worker-src`: a WebGL map library that spawns
//! workers from `blob:` URLs cannot run under it, and broadening the policy to
//! admit one would trade a hard security boundary for a prettier basemap.
//!
//! # Provisioning is explicit
//!
//! No map data ships with SecureMesh and none is ever downloaded. Until an
//! operator provisions a basemap this module reports **not provisioned**, and
//! the map draws a coordinate grid with real markers on it rather than
//! pretending to have geography it does not have.
//!
//! Layout mirrors the AI models deliberately, so "an asset an operator
//! installs" has one shape in this project:
//!
//! ```text
//!   <root>/
//!     ai/models/…           provisioned by docs/ai/PROVISIONING.md
//!     map/basemap.geojson   provisioned by docs/map/PROVISIONING.md
//! ```

use crate::error::{CoreError, CoreResult};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// File an operator provisions, relative to the map root.
pub const BASEMAP_FILE: &str = "basemap.geojson";

/// Largest basemap accepted, in bytes.
///
/// A basemap crosses the IPC boundary in one piece and is projected in the
/// renderer, so an unbounded file would stall the UI rather than degrade it.
/// Refused with a stated reason rather than truncated: half a coastline drawn
/// as if it were the whole one would be a map that lies.
pub const MAX_BASEMAP_BYTES: u64 = 25 * 1024 * 1024;

/// Why no basemap is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BasemapUnavailable {
    /// No `map/` directory anywhere above the executable.
    RootMissing,
    /// The directory exists but holds no basemap.
    NotProvisioned(String),
    /// Present but larger than [`MAX_BASEMAP_BYTES`].
    TooLarge { path: String, bytes: u64 },
    /// Present but not usable as geography.
    Malformed { path: String, reason: String },
    /// The file could not be read at all.
    Unreadable { path: String, reason: String },
}

impl BasemapUnavailable {
    /// A short, operator-facing explanation.
    ///
    /// Says what is wrong *and* where, because "not provisioned" is only
    /// actionable if the operator knows which path was searched.
    pub fn detail(&self) -> String {
        match self {
            BasemapUnavailable::RootMissing => {
                "No map directory was found. See docs/map/PROVISIONING.md.".to_string()
            }
            BasemapUnavailable::NotProvisioned(path) => {
                format!("No basemap at {path}. See docs/map/PROVISIONING.md.")
            }
            BasemapUnavailable::TooLarge { path, bytes } => format!(
                "The basemap at {path} is {} MB; the limit is {} MB.",
                bytes / (1024 * 1024),
                MAX_BASEMAP_BYTES / (1024 * 1024)
            ),
            BasemapUnavailable::Malformed { path, reason } => {
                format!("The basemap at {path} is not usable: {reason}")
            }
            BasemapUnavailable::Unreadable { path, reason } => {
                format!("The basemap at {path} could not be read: {reason}")
            }
        }
    }

    /// Whether this is simply "nothing installed yet", as opposed to a fault.
    ///
    /// The distinction drives the status row: an unprovisioned node is in a
    /// normal state an operator can resolve, while a malformed file is a
    /// problem to report.
    pub fn is_absence(&self) -> bool {
        matches!(
            self,
            BasemapUnavailable::RootMissing | BasemapUnavailable::NotProvisioned(_)
        )
    }
}

impl From<BasemapUnavailable> for CoreError {
    fn from(value: BasemapUnavailable) -> Self {
        CoreError::internal(value.detail())
    }
}

/// The geographic extent a basemap covers, in WGS 84 degrees.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundingBox {
    pub min_latitude: f64,
    pub min_longitude: f64,
    pub max_latitude: f64,
    pub max_longitude: f64,
}

impl BoundingBox {
    /// Grows to include a coordinate, ignoring anything not on Earth.
    ///
    /// Silently skipping an out-of-range point rather than failing: one bad
    /// vertex in a large file should not deny an operator the rest of the map,
    /// and a coordinate outside WGS 84 cannot be drawn anywhere meaningful.
    fn extend(&mut self, longitude: f64, latitude: f64) {
        if !longitude.is_finite() || !latitude.is_finite() {
            return;
        }
        if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
            return;
        }
        self.min_latitude = self.min_latitude.min(latitude);
        self.max_latitude = self.max_latitude.max(latitude);
        self.min_longitude = self.min_longitude.min(longitude);
        self.max_longitude = self.max_longitude.max(longitude);
    }

    fn empty() -> Self {
        Self {
            min_latitude: f64::INFINITY,
            min_longitude: f64::INFINITY,
            max_latitude: f64::NEG_INFINITY,
            max_longitude: f64::NEG_INFINITY,
        }
    }

    fn is_empty(&self) -> bool {
        !self.min_latitude.is_finite() || !self.max_latitude.is_finite()
    }

    /// Whether this box fully contains another.
    ///
    /// Used to check a provisioned basemap actually covers the ground the
    /// node has incidents on. A basemap for the wrong region is worse than
    /// none: it renders, so it looks correct.
    pub fn contains(&self, other: &BoundingBox) -> bool {
        self.min_latitude <= other.min_latitude
            && self.max_latitude >= other.max_latitude
            && self.min_longitude <= other.min_longitude
            && self.max_longitude >= other.max_longitude
    }

    /// Grows the box by a margin in kilometres.
    ///
    /// Longitude degrees shrink with latitude, so the eastward margin is
    /// divided by cos(latitude). Using a fixed degree margin would give a
    /// box far narrower than intended away from the equator.
    pub fn expanded_by_km(&self, margin_km: f64) -> BoundingBox {
        const KM_PER_DEGREE_LATITUDE: f64 = 110.574;
        const KM_PER_DEGREE_LONGITUDE: f64 = 111.320;

        let latitude_margin = margin_km / KM_PER_DEGREE_LATITUDE;
        let mid_latitude = (self.min_latitude + self.max_latitude) / 2.0;
        let scale = mid_latitude.to_radians().cos().max(0.01);
        let longitude_margin = margin_km / (KM_PER_DEGREE_LONGITUDE * scale);

        BoundingBox {
            min_latitude: (self.min_latitude - latitude_margin).max(-90.0),
            max_latitude: (self.max_latitude + latitude_margin).min(90.0),
            min_longitude: (self.min_longitude - longitude_margin).max(-180.0),
            max_longitude: (self.max_longitude + longitude_margin).min(180.0),
        }
    }
}

/// The ground a node's own records sit on, plus a margin.
///
/// Derived from what is actually in the database rather than from a constant,
/// so the region a basemap must cover follows the deployment instead of being
/// decided in advance by this project.
///
/// Returns `None` when no incident has coordinates: there is no region to
/// derive, and inventing one would be picking a place at random.
pub fn required_coverage(positions: &[(f64, f64)], margin_km: f64) -> Option<BoundingBox> {
    let mut bounds = BoundingBox::empty();
    for (latitude, longitude) in positions {
        bounds.extend(*longitude, *latitude);
    }

    if bounds.is_empty() {
        return None;
    }
    Some(bounds.expanded_by_km(margin_km))
}

/// A provisioned basemap, described without its contents.
///
/// Deliberately separate from the GeoJSON itself: the dashboard polls status
/// every couple of seconds and must never pull megabytes of geometry across
/// IPC to do it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Basemap {
    /// File name, shown to the operator so they can identify what is installed.
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub feature_count: usize,
    /// Extent of the data, so the UI can frame it without scanning geometry.
    pub bounds: BoundingBox,
    /// SHA-256 of the file, so an operator can confirm what is installed
    /// matches what they intended to install.
    pub sha256: String,
}

/// Locates the directory holding `map/`.
///
/// Mirrors `crate::project_root`: checks the executable's own directory first,
/// which is where a staged or installed build keeps provisioned assets, then
/// walks up, which covers running from `target/debug` during development.
pub fn map_root() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("SECUREMESH_MAP_ROOT") {
        return Some(PathBuf::from(explicit));
    }

    let executable = std::env::current_exe().ok()?;
    let mut directory = executable.parent()?.to_path_buf();

    for _ in 0..6 {
        if directory.join("map").is_dir() {
            return Some(directory.join("map"));
        }
        if !directory.pop() {
            break;
        }
    }
    None
}

/// Reads and validates the provisioned basemap.
///
/// Validation is not decoration. The renderer draws whatever this returns, so a
/// file that is not a `FeatureCollection`, or holds no drawable geometry, has
/// to be refused here rather than becoming an empty map that looks like a
/// working one.
pub fn load() -> Result<(Basemap, String), BasemapUnavailable> {
    let root = map_root().ok_or(BasemapUnavailable::RootMissing)?;
    load_from(&root.join(BASEMAP_FILE))
}

/// Reads and validates a basemap at an explicit path.
pub fn load_from(path: &Path) -> Result<(Basemap, String), BasemapUnavailable> {
    let display = path.display().to_string();

    let metadata =
        std::fs::metadata(path).map_err(|_| BasemapUnavailable::NotProvisioned(display.clone()))?;
    if metadata.len() > MAX_BASEMAP_BYTES {
        return Err(BasemapUnavailable::TooLarge {
            path: display,
            bytes: metadata.len(),
        });
    }

    let text = std::fs::read_to_string(path).map_err(|error| BasemapUnavailable::Unreadable {
        path: display.clone(),
        reason: error.to_string(),
    })?;

    let parsed: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| BasemapUnavailable::Malformed {
            path: display.clone(),
            reason: format!("not valid JSON: {error}"),
        })?;

    if parsed.get("type").and_then(|value| value.as_str()) != Some("FeatureCollection") {
        return Err(BasemapUnavailable::Malformed {
            path: display,
            reason: "the top level must be a GeoJSON FeatureCollection".to_string(),
        });
    }

    let features = parsed
        .get("features")
        .and_then(|value| value.as_array())
        .ok_or_else(|| BasemapUnavailable::Malformed {
            path: display.clone(),
            reason: "the FeatureCollection has no `features` array".to_string(),
        })?;

    let mut bounds = BoundingBox::empty();
    for feature in features {
        if let Some(geometry) = feature.get("geometry") {
            extend_bounds(&mut bounds, geometry, 0);
        }
    }

    if bounds.is_empty() {
        return Err(BasemapUnavailable::Malformed {
            path: display,
            reason: "no drawable coordinates were found".to_string(),
        });
    }

    let basemap = Basemap {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| BASEMAP_FILE.to_string()),
        path: display,
        bytes: metadata.len(),
        feature_count: features.len(),
        bounds,
        sha256: hex::encode(Sha256::digest(text.as_bytes())),
    };

    Ok((basemap, text))
}

/// Walks a GeoJSON geometry, extending the bounding box with every position.
///
/// Depth-bounded rather than trusting the file: GeoJSON nests coordinate arrays
/// by geometry type, and a hand-edited or hostile file could nest far deeper
/// than any real geometry does. Recursion that follows the data without a limit
/// would overflow the stack on input this function exists to validate.
fn extend_bounds(bounds: &mut BoundingBox, geometry: &serde_json::Value, depth: usize) {
    const MAX_DEPTH: usize = 8;
    if depth > MAX_DEPTH {
        return;
    }

    // A GeometryCollection holds geometries rather than coordinates.
    if let Some(geometries) = geometry
        .get("geometries")
        .and_then(|value| value.as_array())
    {
        for nested in geometries {
            extend_bounds(bounds, nested, depth + 1);
        }
        return;
    }

    if let Some(coordinates) = geometry.get("coordinates") {
        extend_positions(bounds, coordinates, depth);
    }
}

/// Descends a coordinate array to its `[longitude, latitude]` leaves.
fn extend_positions(bounds: &mut BoundingBox, node: &serde_json::Value, depth: usize) {
    const MAX_DEPTH: usize = 8;
    if depth > MAX_DEPTH {
        return;
    }

    let Some(array) = node.as_array() else {
        return;
    };

    // A position is `[lon, lat]` or `[lon, lat, altitude]`; anything whose
    // first element is a number is a leaf.
    if array.first().and_then(|value| value.as_f64()).is_some() && array.len() >= 2 {
        if let (Some(longitude), Some(latitude)) = (array[0].as_f64(), array[1].as_f64()) {
            bounds.extend(longitude, latitude);
        }
        return;
    }

    for child in array {
        extend_positions(bounds, child, depth + 1);
    }
}

/// Reads the basemap description without its geometry.
///
/// Used by the status row, which is polled. Reading the file to hash it is the
/// cost of being able to state what is installed; at the 25 MB ceiling that is
/// still cheap next to a network round trip, and there is no network here.
pub fn describe() -> Result<Basemap, BasemapUnavailable> {
    load().map(|(basemap, _geojson)| basemap)
}

/// The basemap geometry, for the renderer.
///
/// Returned as the original text rather than a re-serialised structure, so what
/// the renderer draws is exactly what the operator provisioned and the
/// checksum in [`Basemap`] describes.
pub fn geojson() -> CoreResult<String> {
    let (_basemap, text) = load()?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write(directory: &TempDir, contents: &str) -> PathBuf {
        let path = directory.path().join(BASEMAP_FILE);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        path
    }

    const VALID: &str = r#"{
        "type": "FeatureCollection",
        "features": [
            {
                "type": "Feature",
                "properties": { "name": "track" },
                "geometry": {
                    "type": "LineString",
                    "coordinates": [[77.55, 13.12], [77.57, 13.14]]
                }
            },
            {
                "type": "Feature",
                "properties": {},
                "geometry": { "type": "Point", "coordinates": [77.56, 13.13] }
            }
        ]
    }"#;

    #[test]
    fn a_valid_basemap_reports_its_extent_and_contents() {
        let directory = TempDir::new().unwrap();
        let path = write(&directory, VALID);

        let (basemap, text) = load_from(&path).unwrap();

        assert_eq!(basemap.feature_count, 2);
        assert_eq!(basemap.name, BASEMAP_FILE);
        assert!(basemap.bytes > 0);
        assert_eq!(basemap.sha256.len(), 64);
        // The geometry is returned verbatim, so it matches the checksum.
        assert_eq!(text.len() as u64, basemap.bytes);

        assert!((basemap.bounds.min_longitude - 77.55).abs() < 1e-9);
        assert!((basemap.bounds.max_longitude - 77.57).abs() < 1e-9);
        assert!((basemap.bounds.min_latitude - 13.12).abs() < 1e-9);
        assert!((basemap.bounds.max_latitude - 13.14).abs() < 1e-9);
    }

    #[test]
    fn a_missing_basemap_reads_as_absence_not_as_a_fault() {
        let directory = TempDir::new().unwrap();
        let error = load_from(&directory.path().join(BASEMAP_FILE)).unwrap_err();

        assert!(error.is_absence());
        assert!(error.detail().contains("PROVISIONING"));
    }

    #[test]
    fn a_file_that_is_not_json_is_refused() {
        let directory = TempDir::new().unwrap();
        let path = write(&directory, "this is not a map");

        let error = load_from(&path).unwrap_err();
        assert!(!error.is_absence());
        assert!(error.detail().contains("not valid JSON"));
    }

    #[test]
    fn json_that_is_not_a_feature_collection_is_refused() {
        let directory = TempDir::new().unwrap();
        let path = write(&directory, r#"{"type":"Point","coordinates":[0,0]}"#);

        let error = load_from(&path).unwrap_err();
        assert!(error.detail().contains("FeatureCollection"));
    }

    #[test]
    fn a_feature_collection_with_no_coordinates_is_refused() {
        // An empty map would render as a working map with nothing on it, which
        // is indistinguishable from a provisioning mistake.
        let directory = TempDir::new().unwrap();
        let path = write(&directory, r#"{"type":"FeatureCollection","features":[]}"#);

        let error = load_from(&path).unwrap_err();
        assert!(error.detail().contains("no drawable coordinates"));
    }

    #[test]
    fn every_geometry_type_contributes_to_the_extent() {
        let directory = TempDir::new().unwrap();
        let path = write(
            &directory,
            r#"{
                "type": "FeatureCollection",
                "features": [
                    { "type": "Feature", "geometry": {
                        "type": "Polygon",
                        "coordinates": [[[10.0, 20.0], [11.0, 21.0], [10.0, 20.0]]] } },
                    { "type": "Feature", "geometry": {
                        "type": "MultiPolygon",
                        "coordinates": [[[[-5.0, -6.0], [-4.0, -5.0], [-5.0, -6.0]]]] } },
                    { "type": "Feature", "geometry": {
                        "type": "GeometryCollection",
                        "geometries": [
                            { "type": "Point", "coordinates": [30.0, 40.0] }
                        ] } }
                ]
            }"#,
        );

        let (basemap, _) = load_from(&path).unwrap();
        assert_eq!(basemap.feature_count, 3);
        assert!((basemap.bounds.min_longitude - -5.0).abs() < 1e-9);
        assert!((basemap.bounds.min_latitude - -6.0).abs() < 1e-9);
        assert!((basemap.bounds.max_longitude - 30.0).abs() < 1e-9);
        assert!((basemap.bounds.max_latitude - 40.0).abs() < 1e-9);
    }

    #[test]
    fn coordinates_off_the_earth_are_ignored_rather_than_widening_the_extent() {
        let directory = TempDir::new().unwrap();
        let path = write(
            &directory,
            r#"{
                "type": "FeatureCollection",
                "features": [
                    { "type": "Feature", "geometry": {
                        "type": "MultiPoint",
                        "coordinates": [[77.56, 13.13], [999.0, 999.0]] } }
                ]
            }"#,
        );

        let (basemap, _) = load_from(&path).unwrap();
        assert!((basemap.bounds.max_longitude - 77.56).abs() < 1e-9);
        assert!((basemap.bounds.max_latitude - 13.13).abs() < 1e-9);
    }

    #[test]
    fn deeply_nested_coordinates_do_not_overflow_the_stack() {
        // Validation runs on operator-supplied files, so it has to survive one
        // that was hand-edited or crafted. Bounded depth, not trust.
        let directory = TempDir::new().unwrap();
        let mut nested = String::new();
        for _ in 0..2_000 {
            nested.push('[');
        }
        nested.push_str("77.56, 13.13");
        for _ in 0..2_000 {
            nested.push(']');
        }
        let path = write(
            &directory,
            &format!(
                r#"{{"type":"FeatureCollection","features":[
                    {{"type":"Feature","geometry":{{"type":"Polygon","coordinates":{nested}}}}}]}}"#
            ),
        );

        // Either refused as undrawable or accepted with nothing found — both
        // are fine. Not crashing is the property under test.
        let _ = load_from(&path);
    }

    #[test]
    fn the_same_file_always_hashes_the_same_way() {
        let directory = TempDir::new().unwrap();
        let path = write(&directory, VALID);

        let (first, _) = load_from(&path).unwrap();
        let (second, _) = load_from(&path).unwrap();
        assert_eq!(first.sha256, second.sha256);
    }

    #[test]
    fn this_module_holds_no_network_client() {
        // Structural, not behavioural: a runtime check could only show that
        // this machine happened not to make a request.
        //
        // Only the implementation half is scanned. Both the module
        // documentation and this test necessarily name the things they rule
        // out, and a scan including them would fail on a correct file.
        let source = include_str!("mod.rs");
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("the file has an implementation half");
        let code: String = implementation
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        for forbidden in [
            "http://",
            "https://",
            "reqwest",
            "ureq",
            "TcpStream",
            "mapbox",
            "googleapis",
            "openstreetmap",
            "api_key",
            "tile",
        ] {
            assert!(
                !code.contains(forbidden),
                "the map module references {forbidden}"
            );
        }
    }
}
