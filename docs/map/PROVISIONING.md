# Provisioning offline map data

SecureMesh's tactical map is fully offline. It contacts no tile server, no
style server, no font or sprite host and no geocoder — there is no HTTP client
anywhere in the map code path. The consequence is that geography has to be put
on the machine deliberately, by you.

## What SecureMesh ships

Nothing. No basemap is bundled and none is ever downloaded, at startup or
otherwise. This is a deliberate constraint, not an omission:

- a build that fetched map data would make "no map request leaves this machine"
  untrue;
- map data carries licensing obligations that depend on the source, and this
  project must not make that choice on an operator's behalf.

Without a basemap the map still works. It draws a latitude/longitude graticule,
a scale bar, incident markers with accuracy circles, and this node's position
once you ask for it. What it lacks is coastlines, roads and boundaries.

## Installing a basemap

1. Obtain a GeoJSON file covering your area of operations.
2. Place it at `map/basemap.geojson`, beside the `ai/` directory.
3. Run `npm run map:provision` to verify it.
4. Restart SecureMesh. The basemap is read once at startup.

The map directory is located by walking up from the executable, the same way
the AI models are found, so a staged build in `dist-app/` and a development
build in `src-tauri/target/debug/` both resolve to the project root. Override
it with `SECUREMESH_MAP_ROOT` if you keep data elsewhere.

## What is accepted

| | |
|---|---|
| Format | GeoJSON, a `FeatureCollection` at the top level |
| Geometry | Point, LineString, Polygon, their Multi- forms, GeometryCollection |
| Coordinate system | WGS 84 decimal degrees (`[longitude, latitude]`) — GeoJSON's only valid CRS |
| Maximum size | 25 MB |

The file is validated before it is used. A file that is not JSON, is not a
`FeatureCollection`, or contains no drawable coordinates is refused with a
stated reason rather than rendering as an empty map — an empty map and a
provisioning mistake look identical, and only one of them is acceptable.

The 25 MB limit exists because the basemap crosses the IPC boundary in one
piece. If your region needs more than that, cut a smaller extract: a tactical
map is for an area of operations, not a continent.

## Producing a file

Any GIS tool that exports GeoJSON will do. Common routes:

- **OpenStreetMap extract** — clip a region and convert to GeoJSON with a tool
  such as `osmium` or `ogr2ogr`, offline, from a `.osm.pbf` you already hold.
  OSM data is ODbL: **attribution and share-alike obligations apply to you**,
  and SecureMesh does not discharge them on your behalf.
- **Natural Earth** — public domain, and small enough for country and coastline
  outlines at a regional scale. Good for orientation, too coarse for streets.
- **National mapping agency open data** — licensing varies; check before use.

Coordinates must be WGS 84. Reproject before importing; SecureMesh does not
transform coordinate systems and will place a projected file in the wrong part
of the world without complaining, because it has no way to know.

## The dataset shipped with this prototype

The demonstration node in this repository is provisioned with a real extract,
recorded here so it can be regenerated and verified independently.

| | |
|---|---|
| Source | OpenStreetMap, via the Overpass API |
| Extracted | 2026-08-20 |
| Region requested | 13.043161, 77.473086 → 13.224036, 77.657574 |
| Actual coverage | 13.003488, 77.448314 → 13.333222, 77.685387 |
| Area | ~20 km box, northern Bengaluru (Yelahanka, Hesaraghatta, Devanahalli approaches) |
| Features | 3 302 |
| Size | 1.25 MB (1 315 913 bytes) |
| SHA-256 | `b0a68dc5e78037d3e6a4af9f6596aec2f2f89fce09d7ce14dacb42dafac8ebe9` |
| Licence | ODbL 1.0 |
| Attribution | © OpenStreetMap contributors |

Feature breakdown: 1 641 secondary roads, 869 water bodies, 306 primary roads,
228 major roads, 167 railway ways, 88 named places, 3 waterways.

**The region was derived, not chosen.** `npm run map:provision -- <data-dir>`
reads the node's own incidents from SQLite, takes their bounding box and adds a
10 km margin. No coordinate for the demonstration area appears anywhere in the
application; change where the incidents are and the required region changes with
them.

### Attribution obligations

This data is © OpenStreetMap contributors and licensed under the
[Open Database Licence 1.0](https://opendatacommons.org/licenses/odbl/).

The attribution and licence are written **inside** `basemap.geojson` as
top-level members, so they cannot be separated from the data by copying the
file. Anyone distributing SecureMesh with this basemap carries ODbL's
attribution and share-alike obligations. That is a reason to treat the shipped
extract as a demonstration artefact and to make a deliberate licensing decision
before any real deployment.

## Regenerating the extract

```
node scripts/extract-osm-basemap.mjs <south> <west> <north> <east>
```

The script records its query, so the same box yields the same features and a
comparable checksum. It filters to what a tactical map needs — classified roads,
railways, water, administrative edges and named places — rather than everything
in the box. That keeps the file small enough to project in a single pass
**without simplifying geometry**, so nothing is moved to save bytes.

**This script is not part of the application.** It is run by hand, by an
operator, and the shipped binary has no HTTP client on the map path and cannot
fetch anything regardless.

## How features are drawn

The extraction assigns each feature a `kind`, and the renderer draws by `kind`
rather than by OSM tag, so tag vocabulary stays in provisioning:

| `kind` | Drawn as |
|---|---|
| `water` | Filled area, `--accent-soft` |
| `waterway` | Line, `--accent-soft` |
| `boundary` | Dashed line, `--border-strong`, reduced opacity |
| `rail` | Dashed line, `--text-muted` |
| `road-secondary` | Thin line, `--border-default` |
| `road-primary` | Medium line, `--border-strong` |
| `road-major` | Thick line, `--text-muted` |
| `place` | A **label**, never a mark on the map |

Order is back-to-front: water, boundaries, rail, then roads by importance.
Incident and node markers are drawn in layers above all of it and always win.

Every colour is an existing SecureMesh theme token, so light and dark both work
with no second theme and no style switching. A test asserts that no map rule
hard-codes a colour and that every colour token it uses is defined in all three
theme blocks.

A basemap from another source, without SecureMesh's `kind` classification,
renders as nothing rather than as unreadable scribble.

### Labels

At most 14 place names are shown, ranked city → town → suburb → village, with
duplicates removed. A 20 km box holds hundreds of named hamlets, and drawing
them all is less legible than drawing none. **Names come from the data or not at
all** — nothing is invented, and a place with no name in OSM gets no label.


## Checking what is installed

```
npm run map:provision
```

Reports the file name, size, feature count, SHA-256 and geographic coverage.
The checksum lets you confirm the file on a node is the one you intended to
deploy — useful when several nodes are provisioned from one source.

Coverage is the bounding box of the data. Incidents outside it still appear;
they simply have no basemap behind them.

## What this does not give you

- **No geocoding.** Coordinates are shown as numbers. There is no place-name
  search, forward or reverse, because both require a service.
- **No routing.** The map shows where things are, not how to get there.
- **No imagery.** Vector geometry only; satellite and aerial imagery are raster
  tile products and are not supported.
- **No tracking.** Position is a snapshot taken when an operator asks for one.
  SecureMesh records no movement history.

## A note on GPS

The map being offline says nothing about how a position was obtained. On
Windows, the platform location provider may use network-assisted positioning,
and a fix it returns is labelled `WIRELESS` rather than `GNSS` for exactly that
reason. A dedicated GNSS receiver would provide positioning independently of
Internet connectivity; that is future hardware work, not something the map
changes.
