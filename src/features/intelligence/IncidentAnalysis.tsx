import { useState } from "react";
import { analyseIncident, CoreError } from "../../lib/ipc";
import { formatTimestamp } from "../../lib/format";
import { SeverityBadge } from "../../components/SeverityBadge";
import type { Incident, IncidentAnalysis, IntelligenceStatus } from "../../types/core";

interface IncidentAnalysisProps {
  incident: Incident;
  analysis: IncidentAnalysis | null;
  status: IntelligenceStatus | null;
  onAnalysed: (analysis: IncidentAnalysis) => void;
}

/**
 * Locally derived intelligence for one incident.
 *
 * Analysis is an explicit action, never automatic: it takes seconds on CPU, and
 * putting it on the incident-creation path would make capture wait for a model.
 *
 * The model's severity is shown *beside* the operator's rather than replacing
 * it. When they disagree that is worth seeing, and silently overwriting a
 * human's CRITICAL with a model's LOW would be the worst possible failure here.
 */
export function IncidentAnalysisView({
  incident,
  analysis,
  status,
  onAnalysed,
}: IncidentAnalysisProps) {
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const ready = status?.state === "READY";

  async function handleAnalyse() {
    setRunning(true);
    setError(null);
    try {
      onAnalysed(await analyseIncident(incident.id));
    } catch (raw) {
      setError((raw as CoreError).message ?? "The analysis could not be completed.");
    } finally {
      setRunning(false);
    }
  }

  return (
    <div className="analysis">
      <div className="analysis__actions">
        <button
          type="button"
          className="button button--secondary button--compact"
          disabled={!ready || running}
          onClick={handleAnalyse}
        >
          {running ? "Analysing locally…" : analysis ? "Re-analyse locally" : "Analyse locally"}
        </button>
        {!ready && (
          <span className="text-muted">Local model unavailable</span>
        )}
      </div>

      {error && (
        <div className="alert" role="alert">
          <div className="alert__body">{error}</div>
        </div>
      )}

      {analysis && (
        <dl className="key-value analysis__fields">
          <div className="key-value__row">
            <dt className="key-value__key">Category</dt>
            <dd className="key-value__value">{analysis.category}</dd>
          </div>

          <div className="key-value__row">
            <dt className="key-value__key">Severity</dt>
            <dd className="key-value__value analysis__severity">
              <span>
                model: <SeverityBadge severity={analysis.severity} />
              </span>
              <span>
                operator: <SeverityBadge severity={incident.severity} />
              </span>
              {analysis.severity !== incident.severity && (
                <span className="analysis__disagreement">
                  assessments differ
                </span>
              )}
            </dd>
          </div>

          <div className="key-value__row">
            <dt className="key-value__key">Summary</dt>
            <dd className="key-value__value analysis__summary">{analysis.summary}</dd>
          </div>

          {analysis.asset && (
            <div className="key-value__row">
              <dt className="key-value__key">Asset</dt>
              <dd className="key-value__value">{analysis.asset}</dd>
            </div>
          )}

          {analysis.cause && (
            <div className="key-value__row">
              <dt className="key-value__key">Cause</dt>
              <dd className="key-value__value">{analysis.cause}</dd>
            </div>
          )}

          <div className="key-value__row">
            <dt className="key-value__key">Access</dt>
            <dd className="key-value__value">{analysis.accessStatus}</dd>
          </div>

          {analysis.entities.length > 0 && (
            <div className="key-value__row">
              <dt className="key-value__key">Entities</dt>
              <dd className="key-value__value">{analysis.entities.join(", ")}</dd>
            </div>
          )}

          <div className="key-value__row">
            <dt className="key-value__key">Confidence</dt>
            <dd className="key-value__value">
              {analysis.confidence === null
                ? "not stated"
                : `${(analysis.confidence * 100).toFixed(0)}% (model's own estimate)`}
            </dd>
          </div>

          <div className="key-value__row">
            <dt className="key-value__key">Produced by</dt>
            <dd className="key-value__value">
              {analysis.modelId} · {analysis.latencyMs} ms ·{" "}
              {formatTimestamp(analysis.generatedAt)}
            </dd>
          </div>
        </dl>
      )}
    </div>
  );
}
