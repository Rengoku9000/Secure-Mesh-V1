# Local map data

Put a GeoJSON basemap here, named `basemap.geojson`, to give the tactical map
real geography.

SecureMesh ships **no map data** and downloads none, ever. Until a file is
placed here the map draws a coordinate grid with real incident and node markers
on it, and the dashboard reports `Local map: Not provisioned` rather than
claiming to be ready.

Verify what is installed with:

```
npm run map:provision
```

See `docs/map/PROVISIONING.md` for how to produce a file, what the size limit
is, and what licensing obligations come with common data sources.
