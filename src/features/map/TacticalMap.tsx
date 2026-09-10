import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { SeverityBadge } from "../../components/SeverityBadge";
import { SyncStatusBadge } from "../../components/SyncStatusBadge";
import { IndexStateBadge } from "../../components/IndexStateBadge";
import {
  CoreError,
  getCurrentLocation,
  getMapBasemap,
  getMapGeojson,
} from "../../lib/ipc";
import { formatAccuracy, formatAge, formatTimestamp } from "../../lib/format";
import type {
  Basemap,
  ComponentStatus,
  DeviceLocation,
  Incident,
  IncidentIndexState,
  IndexState,
  Peer,
  PeerLocationView,
  PublicIdentity,
} from "../../types/core";
import { projectBasemap, type ProjectedBasemap } from "./basemap.ts";
import { frameOnSelf, offscreenCount } from "./camera.ts";
import {
  accuracyRadiusPixels,
  DEVICE_SOURCE_LABEL,
  incidentMarkers,
  markerSignature,
  FRESHNESS_LABEL,
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
  /** Positions peers reported over the mesh. Authoritative, or absent. */
  peerLocations: PeerLocationView[];
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
  peerLocations,
  identity,
  indexStates,
  status: _status,
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
  // Peer selection is separate from incident selection: they are different
  // kinds of thing, and a peer is not an incident the dashboard can open.
  const [selectedPeer, setSelectedPeer] = useState<string | null>(null);
  const [locating, setLocating] = useState(false);
  const [locationError, setLocationError] = useState<string | null>(null);
  const [isFullscreen, setIsFullscreen] = useState(false);

  // Listen for Escape key to close popups or exit fullscreen mode smoothly
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        if (selected) {
          onSelect(null);
        } else if (selectedPeer) {
          setSelectedPeer(null);
        } else if (isFullscreen) {
          setIsFullscreen(false);
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [selected, selectedPeer, isFullscreen, onSelect]);

  const drag = useRef<{ pointerId: number; x: number; y: number } | null>(null);
  const dragOrigin = useRef<{ x: number; y: number } | null>(null);

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

  // Built only from positions peers reported over the authenticated mesh.
  // Nothing is inferred from an address or from an incident's coordinates.
  const peerPoints = useMemo(
    () => peerMarkers(peers, peerLocations),
    [peers, peerLocations],
  );

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
    // Peers whose position is current or ageing. Expired ones are excluded:
    // they say where a node was, and framing the view around them would move
    // the camera for something that is no longer true.
    for (const peer of peerPoints) {
      points.push(peer.position);
    }
    if (here) {
      points.push({ latitude: here.latitude, longitude: here.longitude });
    }
    return frameOn(points);
  }, [markers, peerPoints, here, frameOn]);

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
    if (selected.latitude === null || selected.longitude === null) return;

    centredOn.current = selected.id;
    setCentre({ latitude: selected.latitude, longitude: selected.longitude });
    setZoom((z) => Math.max(z, 14));
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
    dragOrigin.current = { x: event.clientX, y: event.clientY };
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
      if (dragOrigin.current) {
        const dist = Math.hypot(
          event.clientX - dragOrigin.current.x,
          event.clientY - dragOrigin.current.y,
        );
        // If the pointer moved less than 5px, it was a click on the background: dismiss selection
        if (dist < 5) {
          onSelect(null);
          setSelectedPeer(null);
        }
      }
      drag.current = null;
      dragOrigin.current = null;
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

  // Counted so the operator can be told, never used to decide what to draw.
  // Every located incident is rendered; the viewport only decides what is on
  // screen right now.
  const outOfView = useMemo(
    () => offscreenCount(markers.map((marker) => marker.position), viewport),
    [markers, viewport],
  );

  // --- Selection ----------------------------------------------------------
  //
  const peerCard = useMemo(
    () => peerPoints.find((peer) => peer.nodeId === selectedPeer) ?? null,
    [peerPoints, selectedPeer],
  );
  const peerPoint = peerCard ? toScreen(peerCard.position, viewport) : { x: 0, y: 0 };

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

  const handleOpenDetails = useCallback(
    (incident: Incident) => {
      setIsFullscreen(false);
      onOpenDetails(incident);
    },
    [onOpenDetails],
  );

  // Responsive, boundary-clamped positioning for incident popup
  const incidentPopupStyle = useMemo(() => {
    if (!selectedMarker) return undefined;
    const isCompact = size.width < 560 || size.height < 420;
    if (isCompact) {
      return {
        left: "12px",
        right: "12px",
        bottom: "12px",
        maxWidth: "420px",
        margin: "0 auto",
      };
    }
    const popupWidth = 310;
    const estimatedHeight = 260;
    const minLeft = 14;
    const maxLeft = Math.max(minLeft, size.width - popupWidth - 14);
    const clampedX = Math.max(minLeft, Math.min(maxLeft, selectedPoint.x - popupWidth / 2));
    const fitsAbove = selectedPoint.y - estimatedHeight - 16 >= 12;
    if (fitsAbove) {
      return {
        left: `${clampedX}px`,
        top: `${selectedPoint.y - 14}px`,
        transform: "translateY(-100%)",
        width: `${popupWidth}px`,
      };
    } else {
      const clampedY = Math.min(size.height - estimatedHeight - 14, selectedPoint.y + 16);
      return {
        left: `${clampedX}px`,
        top: `${Math.max(12, clampedY)}px`,
        width: `${popupWidth}px`,
      };
    }
  }, [selectedMarker, selectedPoint.x, selectedPoint.y, size.width, size.height]);

  // Responsive, boundary-clamped positioning for peer popup
  const peerPopupStyle = useMemo(() => {
    if (!peerCard) return undefined;
    const isCompact = size.width < 560 || size.height < 420;
    if (isCompact) {
      return {
        left: "12px",
        right: "12px",
        bottom: "12px",
        maxWidth: "420px",
        margin: "0 auto",
      };
    }
    const popupWidth = 310;
    const estimatedHeight = 240;
    const minLeft = 14;
    const maxLeft = Math.max(minLeft, size.width - popupWidth - 14);
    const clampedX = Math.max(minLeft, Math.min(maxLeft, peerPoint.x - popupWidth / 2));
    const fitsAbove = peerPoint.y - estimatedHeight - 16 >= 12;
    if (fitsAbove) {
      return {
        left: `${clampedX}px`,
        top: `${peerPoint.y - 14}px`,
        transform: "translateY(-100%)",
        width: `${popupWidth}px`,
      };
    } else {
      const clampedY = Math.min(size.height - estimatedHeight - 14, peerPoint.y + 16);
      return {
        left: `${clampedX}px`,
        top: `${Math.max(12, clampedY)}px`,
        width: `${popupWidth}px`,
      };
    }
  }, [peerCard, peerPoint.x, peerPoint.y, size.width, size.height]);

  return (
    <div className={`map${isFullscreen ? " map--fullscreen" : ""}`} ref={surfaceRef}>
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
          <defs>
            <filter id="map-pin-shadow" x="-40%" y="-40%" width="180%" height="180%">
              <feDropShadow dx="0" dy="1.5" stdDeviation="2.5" floodOpacity="0.35" />
            </filter>
            <filter id="map-pin-glow" x="-60%" y="-60%" width="220%" height="220%">
              <feDropShadow dx="0" dy="0" stdDeviation="4" floodColor="var(--accent)" floodOpacity="0.75" />
            </filter>
          </defs>

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
              const radius = accuracyRadiusPixels(
                peer.accuracyMeters,
                peer.position.latitude,
                zoom,
              );
              const stale = peer.freshness === "STALE";
              return (
                <g key={peer.nodeId}>
                  {radius !== null && (
                    <circle
                      className="map__accuracy--peer"
                      cx={point.x}
                      cy={point.y}
                      r={radius}
                    />
                  )}
                  <circle
                    className={`map__peer${stale ? " map__peer--stale" : ""}`}
                    cx={point.x}
                    cy={point.y}
                    r={7}
                    role="button"
                    tabIndex={0}
                    aria-label={`Peer ${peer.nodeName}`}
                    onPointerDown={(event) => event.stopPropagation()}
                    onClick={() => setSelectedPeer(peer.nodeId)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        setSelectedPeer(peer.nodeId);
                      }
                    }}
                  />
                  <text
                    className="map__peer-label"
                    x={point.x + 12}
                    y={point.y + 4}
                  >
                    {peer.nodeName}
                  </text>
                </g>
              );
            })}
          </g>

          {/* Incidents */}
          <g className="map__incidents">
            {markers.map((marker) => {
              const point = toScreen(marker.position, viewport);
              const isSelected = selected?.id === marker.incidentId;
              return (
                <g key={marker.incidentId} className="map__incident-item">
                  {/* Transparent expanded hit target for smooth, responsive hover */}
                  <circle
                    cx={point.x}
                    cy={point.y}
                    r={Math.max(14, SEVERITY_RADIUS[marker.severity] + 6)}
                    fill="transparent"
                    pointerEvents="all"
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
                  <circle
                    className={`map__incident${isSelected ? " map__incident--selected" : ""}`}
                    cx={point.x}
                    cy={point.y}
                    r={SEVERITY_RADIUS[marker.severity]}
                    style={{ fill: SEVERITY_TOKEN[marker.severity] }}
                    filter={isSelected ? "url(#map-pin-glow)" : "url(#map-pin-shadow)"}
                    pointerEvents="none"
                  />
                  {/* Inner white pip for crisp target precision */}
                  <circle
                    cx={point.x}
                    cy={point.y}
                    r={2}
                    fill="#ffffff"
                    pointerEvents="none"
                    opacity="0.9"
                  />
                </g>
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
                    {/* Animated radar ripple wave */}
                    <circle className="map__node-beacon" cx={point.x} cy={point.y} />
                    <circle className="map__node-outer" cx={point.x} cy={point.y} r={8} />
                    <circle
                      className="map__node-core"
                      cx={point.x}
                      cy={point.y}
                      r={3.5}
                    />
                    <text
                      className="map__node-label"
                      x={point.x + 14}
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

        {/* Floating tactical frosted glass HUD cluster */}
        <div className="map__hud">
          <button
            type="button"
            className="map__hud-btn"
            aria-label="Zoom in"
            title="Zoom in (+)"
            onClick={() => setZoom((value) => clampZoom(value + 1))}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <line x1="12" y1="5" x2="12" y2="19" />
              <line x1="5" y1="12" x2="19" y2="12" />
            </svg>
          </button>
          <button
            type="button"
            className="map__hud-btn"
            aria-label="Zoom out"
            title="Zoom out (−)"
            onClick={() => setZoom((value) => clampZoom(value - 1))}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <line x1="5" y1="12" x2="19" y2="12" />
            </svg>
          </button>
          <div className="map__hud-divider" />
          <button
            type="button"
            className={`map__hud-btn ${locating ? "map__hud-btn--locating" : ""} ${here ? "map__hud-btn--active" : ""}`}
            aria-label="My location"
            title={locating ? "Acquiring GPS fix…" : "My location"}
            onClick={() => void locateMe()}
            disabled={locating}
          >
            {locating ? (
              <svg className="map__hud-spinner" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M21 12a9 9 0 1 1-6.219-8.56" />
              </svg>
            ) : (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <circle cx="12" cy="12" r="7" />
                <line x1="12" y1="2" x2="12" y2="5" />
                <line x1="12" y1="19" x2="12" y2="22" />
                <line x1="2" y1="12" x2="5" y2="12" />
                <line x1="19" y1="12" x2="22" y2="12" />
                <circle cx="12" cy="12" r="2.5" fill="currentColor" />
              </svg>
            )}
          </button>
          <button
            type="button"
            className="map__hud-btn"
            aria-label="Fit all"
            title="Fit all markers"
            onClick={() => fitAll()}
            disabled={markers.length === 0 && !here}
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3" />
            </svg>
          </button>
          <div className="map__hud-divider" />
          <button
            type="button"
            className={`map__hud-btn ${isFullscreen ? "map__hud-btn--active" : ""}`}
            aria-label={isFullscreen ? "Exit fullscreen" : "Enter fullscreen"}
            title={isFullscreen ? "Exit Fullscreen (Esc)" : "Enter Fullscreen"}
            onClick={() => setIsFullscreen((prev) => !prev)}
          >
            {isFullscreen ? (
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <polyline points="4 14 10 14 10 20" />
                <polyline points="20 10 14 10 14 4" />
                <line x1="14" y1="10" x2="21" y2="3" />
                <line x1="3" y1="21" x2="10" y2="14" />
              </svg>
            ) : (
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <polyline points="15 3 21 3 21 9" />
                <polyline points="9 21 3 21 3 15" />
                <line x1="21" y1="3" x2="14" y2="10" />
                <line x1="3" y1="21" x2="10" y2="14" />
              </svg>
            )}
          </button>
        </div>

        {/* Top exit chip when in fullscreen */}
        {isFullscreen && (
          <button
            type="button"
            className="map__fullscreen-exit"
            onClick={() => setIsFullscreen(false)}
            title="Exit Fullscreen (Esc)"
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
            <span>Exit Fullscreen</span>
            <kbd className="mono">Esc</kbd>
          </button>
        )}

        {/* Scale bar with frosted glass background */}
        <div className="map__scale">
          <span className="map__scale-bar" style={{ width: `${scaleBar.width}px` }} />
          <span className="map__scale-label mono">{scaleBar.label}</span>
        </div>

        {/* Tactical Legend: frosted glass pill */}
        <div className="map__legend">
          <span className="map__legend-item">
            <span className="map__swatch map__swatch--node" /> My node
          </span>
          <span
            className={`map__legend-item${
              peerPoints.length === 0 ? " map__legend-item--muted" : ""
            }`}
          >
            <span className="map__swatch map__swatch--peer" />{" "}
            {peerPoints.length === 0
              ? peerLocations.length === 0
                ? "Peer (no fix)"
                : "Peer (stale)"
              : "Peer"}
          </span>
          <span className="map__legend-item">
            <span className="map__swatch map__swatch--incident" /> Incident
          </span>
        </div>

        {!here && (
          <div className="map__notice-pill">
            <span className="map__notice-icon">
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M4.9 19.1C1 15.2 1 8.8 4.9 4.9" />
                <path d="M7.8 16.2c-2.3-2.3-2.3-6.1 0-8.5" />
                <circle cx="12" cy="12" r="2" fill="currentColor" />
                <path d="M16.2 7.8c2.3 2.3 2.3 6.1 0 8.5" />
                <path d="M19.1 4.9C23 8.8 23 15.2 19.1 19.1" />
              </svg>
            </span>
            <span className="map__notice-text">
              {locationError ?? "Offline grid · GPS standby"}
            </span>
            <button
              type="button"
              className="map__notice-action"
              onClick={() => void locateMe()}
              disabled={locating}
            >
              {locating ? "Locating…" : "Locate"}
            </button>
          </div>
        )}

        {here && (
          <div className="map__notice-pill map__notice-pill--active">
            <span className="map__notice-icon">
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M21 10c0 7-9 13-9 13s-9-6-9-13a9 9 0 0 1 18 0z" />
                <circle cx="12" cy="10" r="3" />
              </svg>
            </span>
            <span className="map__notice-text">
              Fix: {here.latitude.toFixed(4)}, {here.longitude.toFixed(4)}
              {here.accuracyMeters != null ? ` (±${Math.round(here.accuracyMeters)}m)` : ""}
            </span>
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
            style={incidentPopupStyle}
            role="dialog"
            aria-label={`Incident ${selectedMarker.severity}`}
            onPointerDown={(event) => event.stopPropagation()}
          >
            <div className="map__popup-head">
              <SeverityBadge severity={selectedMarker.severity} />
              <span className="map__popup-id mono">
                {selectedMarker.incidentId.slice(0, 8)}
              </span>
              <button
                type="button"
                className="map__popup-close"
                aria-label="Close information"
                title="Close information (Esc)"
                onClick={() => onSelect(null)}
              >
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <line x1="18" y1="6" x2="6" y2="18" />
                  <line x1="6" y1="6" x2="18" y2="18" />
                </svg>
              </button>
            </div>

            <p
              className="map__popup-text"
              title="Click to view full incident details"
              onClick={() => handleOpenDetails(selectedMarker.incident)}
              style={{ cursor: "pointer" }}
            >
              {selectedMarker.incident.description}
            </p>

            <dl className="map__popup-facts">
              <div>
                <dt>Location</dt>
                <dd className="mono">
                  {selectedMarker.position.latitude.toFixed(5)},{" "}
                  {selectedMarker.position.longitude.toFixed(5)}
                </dd>
              </div>
              <div>
                <dt>Accuracy</dt>
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
              {indexState && <IndexStateBadge state={indexState} />}
            </div>

            <div className="map__popup-actions">
              <button
                type="button"
                className="button button--primary button--compact map__popup-btn-view"
                onClick={() => handleOpenDetails(selectedMarker.incident)}
                title="Redirect to Incidents tab and highlight this record"
              >
                <span>View in Incidents</span>
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <line x1="5" y1="12" x2="19" y2="12" />
                  <polyline points="12 5 19 12 12 19" />
                </svg>
              </button>
            </div>
          </div>
        )}

        {/* A peer, from the position it reported over the mesh. Provenance is
            stated in full: an operator needs to know how good the reading was,
            what produced it, and how long ago it arrived. */}
        {peerCard && (
          <div
            className="map__popup map__popup--peer"
            style={peerPopupStyle}
            role="dialog"
            aria-label={`Peer ${peerCard.nodeName}`}
            onPointerDown={(event) => event.stopPropagation()}
          >
            <div className="map__popup-head">
              <span className="map__popup-kind">SecureMesh node</span>
              <button
                type="button"
                className="map__popup-close"
                aria-label="Close information"
                title="Close information (Esc)"
                onClick={() => setSelectedPeer(null)}
              >
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <line x1="18" y1="6" x2="6" y2="18" />
                  <line x1="6" y1="6" x2="18" y2="18" />
                </svg>
              </button>
            </div>

            <p className="map__popup-text mono">{peerCard.nodeName}</p>

            <dl className="map__popup-facts">
              <div>
                <dt>Location</dt>
                <dd className="mono">
                  {peerCard.position.latitude.toFixed(5)},{" "}
                  {peerCard.position.longitude.toFixed(5)}
                </dd>
              </div>
              <div>
                <dt>Accuracy</dt>
                <dd className="mono">
                  {peerCard.accuracyMeters === null
                    ? "Unknown"
                    : formatAccuracy(peerCard.accuracyMeters)}
                </dd>
              </div>
              <div>
                <dt>Source</dt>
                <dd>{SOURCE_LABEL[peerCard.source]}</dd>
              </div>
              <div>
                <dt>Updated</dt>
                <dd>{formatAge(peerCard.ageSeconds)}</dd>
              </div>
              <div>
                <dt>Captured</dt>
                <dd>{formatTimestamp(peerCard.capturedAt)}</dd>
              </div>
              <div>
                <dt>Sequence</dt>
                <dd className="mono">{peerCard.sequence}</dd>
              </div>
            </dl>

            <div className="map__popup-badges">
              <span
                className={`map__freshness map__freshness--${peerCard.freshness.toLowerCase()}`}
              >
                {FRESHNESS_LABEL[peerCard.freshness]} location
              </span>
            </div>
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
  );
}
