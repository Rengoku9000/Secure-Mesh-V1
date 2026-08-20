import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { Panel } from "../../components/Panel";
import { SeverityBadge } from "../../components/SeverityBadge";
import { SyncStatusBadge } from "../../components/SyncStatusBadge";
import { IndexStateBadge } from "../../components/IndexStateBadge";
import {
  CoreError,
  getCurrentLocation,
  getMapBasemap,
  getMapGeojson,
} from "../../lib/ipc";
import { formatAccuracy, formatTimestamp } from "../../lib/format";
import type {
  Basemap,
  ComponentStatus,
  DeviceLocation,
  Incident,
  IncidentIndexState,
  IndexState,
  Peer,
  PublicIdentity,
} from "../../types/core";
import { projectBasemap, type ProjectedBasemap } from "./basemap.ts";
import { frameOnSelf, offscreenCount } from "./camera.ts";
import {
  accuracyRadiusPixels,
  DEVICE_SOURCE_LABEL,
  incidentMarkers,
  markerSignature,
  SOURCE_LABEL,
  peerMarkers,
  SEVERITY_RADIUS,
  SEVERITY_TOKEN,
} from "./markers.ts";
import {
  clampZoom,
  fitBounds,
  formatDistance,
  fromScreen,
  graticuleStep,
  niceDistance,
  metresPerPixel,
  project,
  toScreen,
  unproject,
  worldSize,
  type GeoPoint,
  type Viewport,
} from "./projection.ts";

interface TacticalMapProps {
  incidents: Incident[];
  peers: Peer[];
  identity: PublicIdentity | null;
  /** The map status row, so the panel can say why there is no basemap. */
  status: ComponentStatus | undefined;
  /** Index state per incident, so the popup can report AI indexing. */
  indexStates: IncidentIndexState[];
  /** The incident currently selected anywhere in the dashboard. */
  selected: Incident | null;
  /** Selecting on the map selects everywhere. One selection, one owner. */
  onSelect: (incident: Incident | null) => void;
  /** Opens the existing incident details view. Never a second details system. */
  onOpenDetails: (incident: Incident) => void;
}

/** Where the map looks when it has nothing to look at. */
const DEFAULT_ZOOM = 14;

/** How far one wheel notch moves the zoom. */
const WHEEL_ZOOM_STEP = 0.6;


/**
 * The offline tactical map.
 *
 * Draws incidents, and this node once the operator asks for a position, over an
 * optional locally provisioned basemap. Everything it shows comes from
 * SecureMesh state that already exists — the map holds no data of its own and
 * is never a source of truth.
 *
 * Nothing here reaches the network. There is no tile request, no style URL and
 * no geocoder, which is why the map works identically with the machine
 * disconnected.
 */
export function TacticalMap({
  incidents,
  peers,
  identity,
  indexStates,
  status,
  selected,
  onSelect,
  onOpenDetails,
}: TacticalMapProps) {
  const surfaceRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 640, height: 380 });
  const [centre, setCentre] = useState<GeoPoint>({ latitude: 0, longitude: 0 });
  const [zoom, setZoom] = useState(DEFAULT_ZOOM);
  const [framed, setFramed] = useState(false);

  const [basemap, setBasemap] = useState<Basemap | null>(null);
  const [geometry, setGeometry] = useState<ProjectedBasemap | null>(null);

  const [here, setHere] = useState<DeviceLocation | null>(null);
  const [locating, setLocating] = useState(false);
  const [locationError, setLocationError] = useState<string | null>(null);

  const drag = useRef<{ pointerId: number; x: number; y: number } | null>(null);

  // --- Surface size -------------------------------------------------------
  useLayoutEffect(() => {
    const element = surfaceRef.current;
    if (!element) return;

    const observer = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect;
      if (width > 0 && height > 0) {
        setSize({ width, height });
      }
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  // --- Basemap, read once -------------------------------------------------
  //
  // Not on the refresh cycle: geometry is large and does not change while the
  // application runs. Provisioning a basemap takes effect on the next launch.
  useEffect(() => {
    let cancelled = false;

    void getMapBasemap()
      .then(async (description) => {
        if (cancelled || !description) return;
        setBasemap(description);
        const geojson = await getMapGeojson();
        if (!cancelled) setGeometry(projectBasemap(geojson));
      })
      .catch(() => {
        // An unprovisioned or unreadable basemap is not a map failure: the
        // grid and every marker still render. The status row explains it.
        if (!cancelled) {
          setBasemap(null);
          setGeometry(null);
        }
      });

    return () => {
      cancelled = true;
    };
  }, []);

  // --- Markers ------------------------------------------------------------
  //
  // Keyed on a content signature rather than array identity: the dashboard
  // hands back a fresh array every two seconds, and rebuilding the marker set
  // thirty times a minute for unchanged data would make panning stutter.
  const signature = markerSignature(incidents);
  const markers = useMemo(
    () => incidentMarkers(incidents),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [signature],
  );

  // Always empty today: SecureMesh holds no authoritative peer position. Kept
  // as a real call so the layer is correct the day one exists.
  const peerPoints = useMemo(() => peerMarkers(peers), [peers]);

  const viewport: Viewport = useMemo(
    () => ({ centre, zoom, width: size.width, height: size.height }),
    [centre, zoom, size.width, size.height],
  );

  // --- Framing ------------------------------------------------------------
  const frameOn = useCallback(
    (points: GeoPoint[]) => {
      const fitted = fitBounds(points, size.width, size.height);
      if (!fitted) return false;
      setCentre(fitted.centre);
      setZoom(fitted.zoom);
      return true;
    },
    [size.width, size.height],
  );

  /**
   * The view the map opens on: every incident if there are any, otherwise the
   * middle of the provisioned basemap.
   *
   * Returns whether it found anything to look at. There is deliberately no
   * fallback coordinate — a map centred on a place no data supports would be
   * inventing a location.
   */
  const resetView = useCallback(() => {
    const points = markers.map((marker) => marker.position);
    if (points.length > 0 && frameOn(points)) {
      return true;
    }
    if (basemap) {
      setCentre({
        latitude: (basemap.bounds.minLatitude + basemap.bounds.maxLatitude) / 2,
        longitude: (basemap.bounds.minLongitude + basemap.bounds.maxLongitude) / 2,
      });
      setZoom(DEFAULT_ZOOM);
      return true;
    }
    return false;
  }, [markers, basemap, frameOn]);

  /**
   * Everything this node knows about, in one frame: the operator and every
   * located incident.
   *
   * The complete operational picture, and the action to reach for when the
   * question is "where is all of this relative to me?". Includes the node only
   * when a position has actually been taken.
   */
  const fitAll = useCallback(() => {
    const points = markers.map((marker) => marker.position);
    if (here) {
      points.push({ latitude: here.latitude, longitude: here.longitude });
    }
    return frameOn(points);
  }, [markers, here, frameOn]);

  // Frames once, when there is first something to frame. Not on every poll —
  // the map must not yank itself out from under someone who has panned away.
  useEffect(() => {
    if (framed || size.width <= 0) return;
    if (resetView()) setFramed(true);
  }, [framed, resetView, size.width]);

  // A selection made in the table brings the map to it. Tracked by id so this
  // fires once per incident: re-centring on every poll would fight an operator
  // who has panned away while leaving the same row selected.
  const centredOn = useRef<string | null>(null);
  useEffect(() => {
    if (!selected) {
      centredOn.current = null;
      return;
    }
    if (centredOn.current === selected.id) return;
    if (selected.latitude === null || selected.longitude === null) return;

    centredOn.current = selected.id;
    setCentre({ latitude: selected.latitude, longitude: selected.longitude });
    setFramed(true);
  }, [selected]);

  // --- Position -----------------------------------------------------------
  //
  // Taken only when the operator asks, through the core's existing provider.
  // No fix on mount, no watch, no history.
  async function locateMe() {
    setLocating(true);
    setLocationError(null);
    try {
      const fix = await getCurrentLocation();

      // Camera only. The fix is stored because it is new data, but nothing
      // here touches the incident list or the selection — moving the camera
      // must never be able to change what exists.
      setHere(fix);

      const camera = frameOnSelf(
        { latitude: fix.latitude, longitude: fix.longitude },
        markers.map((marker) => marker.position),
        size.width,
        size.height,
      );
      setCentre(camera.centre);
      setZoom(camera.zoom);
      setFramed(true);
      setFramed(true);
    } catch (raw) {
      const coreError = raw as CoreError;
      setHere(null);
      setLocationError(coreError.message ?? "Device location is unavailable.");
    } finally {
      setLocating(false);
    }
  }

  // --- Pan and zoom -------------------------------------------------------
  function onPointerDown(event: ReactPointerEvent<SVGSVGElement>) {
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = { pointerId: event.pointerId, x: event.clientX, y: event.clientY };
  }

  function onPointerMove(event: ReactPointerEvent<SVGSVGElement>) {
    const current = drag.current;
    if (!current || current.pointerId !== event.pointerId) return;

    const dx = event.clientX - current.x;
    const dy = event.clientY - current.y;
    drag.current = { ...current, x: event.clientX, y: event.clientY };

    // Panning moves the centre by the dragged distance, converted back through
    // the projection so the ground under the pointer stays under the pointer.
    setCentre((previous) =>
      fromScreen(
        {
          x: size.width / 2 - dx,
          y: size.height / 2 - dy,
        },
        { centre: previous, zoom, width: size.width, height: size.height },
      ),
    );
  }

  function endDrag(event: ReactPointerEvent<SVGSVGElement>) {
    if (drag.current?.pointerId === event.pointerId) {
      drag.current = null;
    }
  }

  // Wheel zoom is bound natively rather than through React's synthetic handler,
  // which is passive and cannot prevent the page from scrolling underneath.
  useEffect(() => {
    const element = surfaceRef.current;
    if (!element) return;

    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      setZoom((previous) =>
        clampZoom(previous - Math.sign(event.deltaY) * WHEEL_ZOOM_STEP),
      );
    };

    element.addEventListener("wheel", onWheel, { passive: false });
    return () => element.removeEventListener("wheel", onWheel);
  }, []);

  // --- Derived drawing values --------------------------------------------
  const world = worldSize(zoom);
  const centreWorld = project(centre);
  const offsetX = size.width / 2 - centreWorld.x * world;
  const offsetY = size.height / 2 - centreWorld.y * world;

  const graticule = useMemo(() => {
    const step = graticuleStep(zoom);
    const topLeft = fromScreen({ x: 0, y: 0 }, viewport);
    const bottomRight = fromScreen({ x: size.width, y: size.height }, viewport);

    const lines: { x1: number; y1: number; x2: number; y2: number; label: string }[] = [];

    const firstLon = Math.ceil(topLeft.longitude / step) * step;
    for (let lon = firstLon; lon <= bottomRight.longitude; lon += step) {
      const { x } = toScreen({ latitude: centre.latitude, longitude: lon }, viewport);
      if (x >= 0 && x <= size.width) {
        lines.push({ x1: x, y1: 0, x2: x, y2: size.height, label: lon.toFixed(4) });
      }
    }

    const firstLat = Math.floor(topLeft.latitude / step) * step;
    for (let lat = firstLat; lat >= bottomRight.latitude; lat -= step) {
      const { y } = toScreen({ latitude: lat, longitude: centre.longitude }, viewport);
      if (y >= 0 && y <= size.height) {
        lines.push({ x1: 0, y1: y, x2: size.width, y2: y, label: lat.toFixed(4) });
      }
    }

    return lines;
  }, [viewport, zoom, centre.latitude, centre.longitude, size.width, size.height]);

  const scaleBar = useMemo(() => {
    const perPixel = metresPerPixel(centre.latitude, zoom);
    const distance = niceDistance(perPixel * 120);
    return { width: distance / perPixel, label: formatDistance(distance) };
  }, [centre.latitude, zoom]);

  const provisioned = status?.state === "OPERATIONAL";

  // Counted so the operator can be told, never used to decide what to draw.
  // Every located incident is rendered; the viewport only decides what is on
  // screen right now.
  const outOfView = useMemo(
    () => offscreenCount(markers.map((marker) => marker.position), viewport),
    [markers, viewport],
  );

  // --- Selection ----------------------------------------------------------
  //
  // Derived from the dashboard's selection, never stored again here. An
  // incident with no coordinates has no marker, so it has no popup either —
  // the table is where an unlocated incident belongs.
  const selectedMarker = useMemo(
    () => markers.find((marker) => marker.incidentId === selected?.id) ?? null,
    [markers, selected?.id],
  );

  const selectedPoint = selectedMarker
    ? toScreen(selectedMarker.position, viewport)
    : { x: 0, y: 0 };

  const indexState: IndexState | undefined = selectedMarker
    ? indexStates.find((entry) => entry.incidentId === selectedMarker.incidentId)
        ?.state
    : undefined;

  return (
    <Panel
      title="Tactical map"
      subtitle={
        provisioned && basemap
          ? "Offline · local tactical map"
          : "Offline · coordinate grid, no basemap provisioned"
      }
      actions={
        <div className="map-actions">
          <button
            type="button"
            className="button button--secondary button--compact"
            onClick={() => void locateMe()}
            disabled={locating}
          >
            {locating ? "Locating…" : "My location"}
          </button>
          <button
            type="button"
            className="button button--secondary button--compact"
            onClick={() => frameOn(markers.map((marker) => marker.position))}
            disabled={markers.length === 0}
          >
            Fit incidents
          </button>
          <button
            type="button"
            className="button button--secondary button--compact"
            onClick={() => fitAll()}
            disabled={markers.length === 0 && !here}
          >
            Fit all
          </button>
          <button
            type="button"
            className="button button--ghost button--compact"
            onClick={() => resetView()}
          >
            Reset view
          </button>
        </div>
      }
      flush
    >
      <div className="map" ref={surfaceRef}>
        <svg
          className="map__surface"
          width={size.width}
          height={size.height}
          role="img"
          aria-label="Tactical map of incidents and this node"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
        >
          {/* Graticule. Always drawn, so the map states where it is looking
              even with no basemap behind it. */}
          <g className="map__graticule">
            {graticule.map((line, index) => (
              <line
                key={index}
                x1={line.x1}
                y1={line.y1}
                x2={line.x2}
                y2={line.y2}
              />
            ))}
          </g>

          {/* Basemap. Path data is in [0,1] world units, so panning and zooming
              is one transform rather than a reprojection of every path. */}
          {geometry && (
            <g
              className="map__basemap"
              transform={`translate(${offsetX} ${offsetY}) scale(${world})`}
            >
              {geometry.paths.map((path, index) => (
                <path
                  key={index}
                  d={path.d}
                  className={`map__feature map__feature--${path.kind}${
                    path.filled ? " map__feature--area" : ""
                  }`}
                  // Keeps line weight constant while the layer is scaled by
                  // the viewport transform, so a road does not become a
                  // ribbon when zoomed in.
                  vectorEffect="non-scaling-stroke"
                />
              ))}
            </g>
          )}

          {/* Place names. Screen space, not world space: text must stay
              legible at every zoom rather than growing with the map. */}
          {geometry && (
            <g className="map__labels">
              {geometry.labels.map((label) => {
                const point = toScreen(
                  unproject({ x: label.x, y: label.y }),
                  viewport,
                );
                if (
                  point.x < 0 ||
                  point.y < 0 ||
                  point.x > size.width ||
                  point.y > size.height
                ) {
                  return null;
                }
                return (
                  <text
                    key={`${label.text}-${label.x}`}
                    x={point.x}
                    y={point.y}
                    className={`map__label map__label--${label.place}`}
                  >
                    {label.text}
                  </text>
                );
              })}
            </g>
          )}

          {/* Accuracy areas, beneath the markers they describe. */}
          <g className="map__accuracy">
            {markers.map((marker) => {
              const radius = accuracyRadiusPixels(
                marker.accuracyMeters,
                marker.position.latitude,
                zoom,
              );
              if (radius === null) return null;
              const point = toScreen(marker.position, viewport);
              return (
                <circle
                  key={marker.incidentId}
                  cx={point.x}
                  cy={point.y}
                  r={radius}
                  style={{ stroke: SEVERITY_TOKEN[marker.severity] }}
                />
              );
            })}
            {here?.accuracyMeters != null &&
              (() => {
                const radius = accuracyRadiusPixels(
                  here.accuracyMeters,
                  here.latitude,
                  zoom,
                );
                if (radius === null) return null;
                const point = toScreen(
                  { latitude: here.latitude, longitude: here.longitude },
                  viewport,
                );
                return (
                  <circle
                    className="map__accuracy--node"
                    cx={point.x}
                    cy={point.y}
                    r={radius}
                  />
                );
              })()}
          </g>

          {/* Peers. Empty today; see peerMarkers. */}
          <g className="map__peers">
            {peerPoints.map((peer) => {
              const point = toScreen(peer.position, viewport);
              return (
                <circle
                  key={peer.nodeId}
                  className="map__peer"
                  cx={point.x}
                  cy={point.y}
                  r={7}
                />
              );
            })}
          </g>

          {/* Incidents. */}
          <g className="map__incidents">
            {markers.map((marker) => {
              const point = toScreen(marker.position, viewport);
              const isSelected = selected?.id === marker.incidentId;
              return (
                <circle
                  key={marker.incidentId}
                  className={`map__incident${isSelected ? " map__incident--selected" : ""}`}
                  cx={point.x}
                  cy={point.y}
                  r={SEVERITY_RADIUS[marker.severity]}
                  style={{ fill: SEVERITY_TOKEN[marker.severity] }}
                  role="button"
                  tabIndex={0}
                  aria-label={`Incident ${marker.severity}: ${marker.incident.description}`}
                  onPointerDown={(event) => event.stopPropagation()}
                  onClick={() => onSelect(marker.incident)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      onSelect(marker.incident);
                    }
                  }}
                />
              );
            })}
          </g>

          {/* This node, only once a position has actually been taken. */}
          {here && (
            <g className="map__node">
              {(() => {
                const point = toScreen(
                  { latitude: here.latitude, longitude: here.longitude },
                  viewport,
                );
                return (
                  <>
                    <circle cx={point.x} cy={point.y} r={7} />
                    <circle
                      className="map__node-core"
                      cx={point.x}
                      cy={point.y}
                      r={3}
                    />
                    {/* Identity beside the position it describes, rather than
                        only in a corner card. */}
                    <text
                      className="map__node-label"
                      x={point.x + 12}
                      y={point.y + 4}
                    >
                      {identity?.nodeName ?? "This node"}
                    </text>
                  </>
                );
              })()}
            </g>
          )}
        </svg>

        {/* Zoom controls */}
        <div className="map__zoom">
          <button
            type="button"
            aria-label="Zoom in"
            onClick={() => setZoom((value) => clampZoom(value + 1))}
          >
            +
          </button>
          <button
            type="button"
            aria-label="Zoom out"
            onClick={() => setZoom((value) => clampZoom(value - 1))}
          >
            −
          </button>
        </div>

        {/* Scale bar: the only honest way to judge distance on a Mercator map. */}
        <div className="map__scale">
          <span className="map__scale-bar" style={{ width: `${scaleBar.width}px` }} />
          <span className="map__scale-label mono">{scaleBar.label}</span>
        </div>

        <div className="map__legend">
          <span className="map__legend-item">
            <span className="map__swatch map__swatch--node" /> My node
          </span>
          <span className="map__legend-item map__legend-item--muted">
            <span className="map__swatch map__swatch--peer" /> Peer (no position held)
          </span>
          <span className="map__legend-item">
            <span className="map__swatch map__swatch--incident" /> Incident
          </span>
          <span className="map__legend-item">
            <span className="map__swatch map__swatch--accuracy" /> Accuracy area
          </span>
        </div>

        {!here && (
          <div className="map__notice">
            {locationError ?? "CURRENT LOCATION UNAVAILABLE"}
          </div>
        )}

        {/* Says what is off screen rather than leaving an operator to wonder
            whether an incident was dropped. Nothing is filtered — every located
            incident is drawn, and this only reports where the camera is. */}
        {outOfView > 0 && (
          <button
            type="button"
            className="map__offscreen"
            onClick={() => fitAll()}
          >
            {outOfView} incident{outOfView === 1 ? "" : "s"} outside current view
            <span className="map__offscreen-action">Fit all</span>
          </button>
        )}

        {/* Selected incident, anchored at its own marker.

            Reuses the dashboard selection rather than keeping a second one, so
            the table and the map can never disagree about what is selected.
            Positioned in screen space over the SVG so it stays a readable size
            at every zoom. */}
        {selectedMarker && (
          <div
            className="map__popup"
            style={{ left: selectedPoint.x, top: selectedPoint.y }}
            role="dialog"
            aria-label={`Incident ${selectedMarker.severity}`}
          >
            <div className="map__popup-head">
              <SeverityBadge severity={selectedMarker.severity} />
              <span className="map__popup-id mono">
                {selectedMarker.incidentId.slice(0, 8)}
              </span>
              <button
                type="button"
                className="map__popup-close"
                aria-label="Dismiss"
                onClick={() => onSelect(null)}
              >
                ×
              </button>
            </div>

            <p className="map__popup-text">{selectedMarker.incident.description}</p>

            <dl className="map__popup-facts">
              <div>
                <dt>Location</dt>
                <dd className="mono">
                  {selectedMarker.position.latitude.toFixed(6)},{" "}
                  {selectedMarker.position.longitude.toFixed(6)}
                </dd>
              </div>
              <div>
                <dt>Accuracy</dt>
                {/* "Unknown", never a number. A reading that carried no
                    accuracy has none to report. */}
                <dd className="mono">
                  {selectedMarker.accuracyMeters === null
                    ? "Unknown"
                    : formatAccuracy(selectedMarker.accuracyMeters)}
                </dd>
              </div>
              <div>
                <dt>Source</dt>
                <dd>{SOURCE_LABEL[selectedMarker.incident.locationSource]}</dd>
              </div>
              <div>
                <dt>Captured</dt>
                <dd>
                  {selectedMarker.incident.locationCapturedAt === null
                    ? "Unknown"
                    : formatTimestamp(selectedMarker.incident.locationCapturedAt)}
                </dd>
              </div>
            </dl>

            <div className="map__popup-badges">
              <SyncStatusBadge status={selectedMarker.incident.syncStatus} />
              {/* Only when a model is provisioned; a node without one shows
                  nothing rather than an empty or misleading state. */}
              {indexState && <IndexStateBadge state={indexState} />}
            </div>

            <button
              type="button"
              className="button button--secondary button--compact map__popup-action"
              onClick={() => onOpenDetails(selectedMarker.incident)}
            >
              View incident
            </button>
          </div>
        )}

        {/* This node, when a position has been taken. Provenance is stated in
            full: an operator reading a coordinate needs to know how good it is
            and where it came from. */}
        {here && (
          <div className="map__self">
            <div className="map__self-head">
              <span className="map__self-dot" />
              <span className="map__self-name mono">
                {identity?.nodeName ?? "This node"}
              </span>
              <span className="map__self-you">YOU</span>
            </div>
            <dl className="map__self-facts">
              <div>
                <dt>Location</dt>
                <dd className="mono">
                  {here.latitude.toFixed(6)}, {here.longitude.toFixed(6)}
                </dd>
              </div>
              <div>
                <dt>Accuracy</dt>
                <dd className="mono">
                  {here.accuracyMeters === null
                    ? "Unknown"
                    : formatAccuracy(here.accuracyMeters)}
                </dd>
              </div>
              <div>
                <dt>Source</dt>
                <dd>{DEVICE_SOURCE_LABEL[here.source]}</dd>
              </div>
            </dl>
          </div>
        )}
      </div>

      <p className="map__footnote">
        {status?.detail ??
          "Map data has not been installed on this node."}{" "}
        {identity ? `Node ${identity.nodeName}.` : ""} The map itself is fully
        offline: no tile server, no geocoding, and no request leaves this
        machine.
      </p>
    </Panel>
  );
}
