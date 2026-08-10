import { useEffect, useRef, useState, type FormEvent } from "react";
import { Alert } from "../../components/Alert";
import { CoreError, createIncident } from "../../lib/ipc";
import { SEVERITIES, type Incident, type Severity } from "../../types/core";

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

  const descriptionRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    descriptionRef.current?.focus();
  }, []);

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
      const incident = await createIncident({
        description,
        severity,
        latitude: lat,
        longitude: lon,
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
          <button type="button" className="button button--ghost" onClick={onClose}>
            Close
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
              Location is optional. Leave both fields blank if unknown.
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
