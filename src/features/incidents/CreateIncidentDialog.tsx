import { useEffect, useRef, useState, type FormEvent } from "react";
import { Alert } from "../../components/Alert";
import {
  CoreError,
  createIncident,
  getCurrentLocation,
  getLocationPermission,
  requestLocationPermission,
} from "../../lib/ipc";
import {
  SEVERITIES,
  type DeviceLocation,
  type Incident,
  type LocationPermission,
  type LocationSource,
  type Severity,
} from "../../types/core";

/**
 * How the position was obtained, in words an operator can act on.
 *
 * Only a satellite fix is both precise and independent of a network. Saying
 * "GPS" for an IP-derived guess would invite someone to drive to a coordinate
 * that is accurate to a city.
 */
const SOURCE_LABEL: Record<LocationSource, string> = {
  SATELLITE: "Satellite (GNSS)",
  WIRELESS: "Wi-Fi / cellular estimate",
  IP_ADDRESS: "IP address estimate",
  UNKNOWN: "Source not reported",
};

/** Formats accuracy, or says plainly that the platform did not report it. */
function formatAccuracy(meters: number | null): string {
  if (meters === null) {
    return "unavailable";
  }
  return meters < 10 ? `±${meters.toFixed(1)} m` : `±${Math.round(meters)} m`;
}

/** How coordinates are written into the form, and therefore how a captured
 *  position is compared against what is about to be submitted. */
const COORDINATE_PLACES = 6;

/**
 * Whether the fields still hold the position that was captured.
 *
 * The coordinate fields stay editable after a capture. If the operator corrects
 * them, the accuracy and source no longer describe what is in the form, and
 * submitting them anyway would attach a satellite-grade claim to hand-typed
 * numbers. So provenance travels only while the numbers are untouched.
 */
function fieldsStillHoldTheFix(
  fix: DeviceLocation,
  latitude: string,
  longitude: string,
): boolean {
  return (
    latitude.trim() === fix.latitude.toFixed(COORDINATE_PLACES) &&
    longitude.trim() === fix.longitude.toFixed(COORDINATE_PLACES)
  );
}

interface CreateIncidentDialogProps {
  onClose: () => void;
  onCreated: (incident: Incident) => void;
}

/** Parses an optional coordinate field. Returns `undefined` when the text is
 *  present but not a number, so the caller can distinguish "blank" from
 *  "invalid". */
function parseCoordinate(raw: string): number | null | undefined {
  const trimmed = raw.trim();
  if (trimmed === "") {
    return null;
  }
  const value = Number(trimmed);
  return Number.isFinite(value) ? value : undefined;
}

/**
 * Incident capture form.
 *
 * Client-side checks here exist only to give immediate feedback. The Rust core
 * re-validates everything; it is the enforcement point, and a bypassed form
 * cannot write an invalid record.
 */
export function CreateIncidentDialog({ onClose, onCreated }: CreateIncidentDialogProps) {
  const [description, setDescription] = useState("");
  const [severity, setSeverity] = useState<Severity>("MEDIUM");
  const [latitude, setLatitude] = useState("");
  const [longitude, setLongitude] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  // --- Device location ---
  //
  // Nothing is captured when the form opens. A position is read only when the
  // operator asks for one, and what they see before submitting is what gets
  // stored: a snapshot, not a live feed.
  const [permission, setPermission] = useState<LocationPermission>("NOT_REQUESTED");
  const [location, setLocation] = useState<DeviceLocation | null>(null);
  const [locating, setLocating] = useState(false);
  const [locationError, setLocationError] = useState<string | null>(null);

  const descriptionRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    descriptionRef.current?.focus();
  }, []);

  // Reading the permission state never prompts, so it is safe on mount. It is
  // only used to decide which button to draw.
  useEffect(() => {
    let cancelled = false;
    void getLocationPermission()
      .then((state) => {
        if (!cancelled) setPermission(state);
      })
      .catch(() => {
        if (!cancelled) setPermission("UNAVAILABLE");
      });
    return () => {
      cancelled = true;
    };
  }, []);

  /**
   * Captures one position and fills the coordinate fields.
   *
   * The fields stay editable afterwards: the capture is a convenience, and the
   * operator remains the authority on where the incident actually was.
   */
  async function captureLocation() {
    setLocating(true);
    setLocationError(null);
    try {
      if (permission !== "GRANTED") {
        const granted = await requestLocationPermission();
        setPermission(granted);
      }
      const fix = await getCurrentLocation();
      setLocation(fix);
      setLatitude(fix.latitude.toFixed(6));
      setLongitude(fix.longitude.toFixed(6));
      // A successful fix proves access, whatever the earlier state said.
      setPermission("GRANTED");
    } catch (raw) {
      const coreError = raw as CoreError;
      setLocation(null);
      setLocationError(
        coreError.message ?? "Device location is unavailable on this machine.",
      );
      void getLocationPermission()
        .then(setPermission)
        .catch(() => setPermission("UNAVAILABLE"));
    } finally {
      setLocating(false);
    }
  }

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  async function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setError(null);

    if (description.trim() === "") {
      setError("Enter a description of what happened.");
      return;
    }

    const lat = parseCoordinate(latitude);
    const lon = parseCoordinate(longitude);
    if (lat === undefined || lon === undefined) {
      setError("Coordinates must be numbers, or left blank.");
      return;
    }
    if ((lat === null) !== (lon === null)) {
      setError("Enter both latitude and longitude, or neither.");
      return;
    }

    setSubmitting(true);
    try {
      // Provenance describes a measurement. It is sent only when the fields
      // still hold that measurement, and never for coordinates typed by hand —
      // the core refuses metadata without coordinates, and this refuses
      // metadata that would misdescribe them.
      const measured =
        location !== null &&
        lat !== null &&
        lon !== null &&
        fieldsStillHoldTheFix(location, latitude, longitude);

      const incident = await createIncident({
        description,
        severity,
        latitude: lat,
        longitude: lon,
        accuracyMeters: measured ? location.accuracyMeters : null,
        locationSource: measured ? location.source : null,
        locationCapturedAt: measured ? location.capturedAt : null,
      });
      onCreated(incident);
      onClose();
    } catch (raw) {
      const coreError = raw as CoreError;
      setError(coreError.message ?? "The incident could not be saved.");
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div
      className="dialog-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        // Only a click on the backdrop itself dismisses, not a drag that ends
        // outside the dialog after starting inside it.
        if (event.target === event.currentTarget) {
          onClose();
        }
      }}
    >
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="create-incident-title"
      >
        <header className="dialog__header">
          <h2 className="dialog__title" id="create-incident-title">
            Record incident
          </h2>
          <button
            type="button"
            className="dialog__close-btn"
            onClick={onClose}
            aria-label="Close dialog"
            title="Close (Esc)"
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </header>

        <div className="dialog__body">
          <form className="form-grid" onSubmit={handleSubmit}>
            {error && <Alert title="Could not save incident" message={error} />}

            <div className="field">
              <label className="field__label" htmlFor="incident-description">
                Description
              </label>
              <textarea
                id="incident-description"
                ref={descriptionRef}
                className="textarea"
                value={description}
                maxLength={2000}
                onChange={(event) => setDescription(event.target.value)}
                placeholder="What was observed, where, and what is needed"
              />
              <span className="field__hint">
                {description.trim().length} / 2000 characters
              </span>
            </div>

            <div className="field">
              <label className="field__label" htmlFor="incident-severity">
                Severity
              </label>
              <select
                id="incident-severity"
                className="select"
                value={severity}
                onChange={(event) => setSeverity(event.target.value as Severity)}
              >
                {SEVERITIES.map((option) => (
                  <option key={option} value={option}>
                    {option}
                  </option>
                ))}
              </select>
            </div>

            {/* Location is captured on request only — never when the form
                opens — and what is shown here is exactly what will be stored. */}
            <div className="location-capture">
              <div className="location-capture__header">
                <span className="field__label">Location</span>
                {permission === "UNAVAILABLE" && (
                  <span className="location-capture__badge">no provider</span>
                )}
              </div>

              {locationError !== null ? (
                <div className="location-capture__state location-capture__state--error">
                  <span className="location-capture__dot location-capture__dot--error" />
                  <div>
                    <div className="location-capture__title">Location error</div>
                    <div className="location-capture__detail">{locationError}</div>
                    <div className="location-capture__detail">
                      You can still create this incident, or type coordinates by
                      hand.
                    </div>
                  </div>
                </div>
              ) : location === null ? (
                <div className="location-capture__state">
                  <span className="location-capture__dot" />
                  <span className="location-capture__detail">
                    No location selected
                  </span>
                </div>
              ) : (
                <div className="location-capture__state">
                  <span className="location-capture__dot location-capture__dot--ok" />
                  <dl className="location-capture__fix">
                    <div>
                      <dt>Latitude</dt>
                      <dd className="mono">{location.latitude.toFixed(6)}</dd>
                    </div>
                    <div>
                      <dt>Longitude</dt>
                      <dd className="mono">{location.longitude.toFixed(6)}</dd>
                    </div>
                    <div>
                      <dt>Accuracy</dt>
                      <dd className="mono">
                        {formatAccuracy(location.accuracyMeters)}
                      </dd>
                    </div>
                    <div>
                      <dt>Source</dt>
                      <dd>{SOURCE_LABEL[location.source]}</dd>
                    </div>
                    <div>
                      <dt>Captured</dt>
                      <dd className="mono">
                        {new Date(location.capturedAt).toLocaleTimeString()}
                      </dd>
                    </div>
                  </dl>
                </div>
              )}

              <button
                type="button"
                className="button button--secondary button--compact"
                onClick={() => void captureLocation()}
                disabled={locating || permission === "UNAVAILABLE"}
              >
                {locating
                  ? "Locating…"
                  : location === null
                    ? "Use current location"
                    : "Refresh location"}
              </button>

              <span className="field__hint">
                {permission === "UNAVAILABLE"
                  ? "This machine has no location provider. Coordinates can be entered by hand."
                  : "Captured once, when you ask. The incident keeps the position it was reported at."}
              </span>
            </div>

            <div className="field-row">
              <div className="field">
                <label className="field__label" htmlFor="incident-latitude">
                  Latitude
                </label>
                <input
                  id="incident-latitude"
                  className="input"
                  inputMode="decimal"
                  value={latitude}
                  onChange={(event) => setLatitude(event.target.value)}
                  placeholder="-90 to 90"
                />
              </div>
              <div className="field">
                <label className="field__label" htmlFor="incident-longitude">
                  Longitude
                </label>
                <input
                  id="incident-longitude"
                  className="input"
                  inputMode="decimal"
                  value={longitude}
                  onChange={(event) => setLongitude(event.target.value)}
                  placeholder="-180 to 180"
                />
              </div>
            </div>
            <span className="field__hint">
              {location !== null && !fieldsStillHoldTheFix(location, latitude, longitude)
                ? "Coordinates edited by hand. Accuracy and source will not be recorded, because they no longer describe these numbers."
                : "Location is optional. Leave both fields blank if unknown."}
            </span>

            <div className="form-actions">
              <button
                type="button"
                className="button button--secondary"
                onClick={onClose}
                disabled={submitting}
              >
                Cancel
              </button>
              <button type="submit" className="button button--primary" disabled={submitting}>
                {submitting ? "Saving…" : "Record incident"}
              </button>
            </div>
          </form>
        </div>
      </div>
    </div>
  );
}
