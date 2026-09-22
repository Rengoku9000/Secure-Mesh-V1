import { useEffect, useState } from "react";
import { CoreError, getIncidentAnalysis, getIncidentInsight } from "../../lib/ipc";
import { SeverityBadge } from "../../components/SeverityBadge";
import { IncidentAnalysisView } from "./IncidentAnalysis";
import type {
  AnalysisOutcome,
  CategoryMethod,
  Hazard,
  Incident,
  IncidentInsight,
  IntelligenceStatus,
  PeopleSummary,
  Relation,
} from "../../types/core";

const HAZARD_LABEL: Record<Hazard, string> = {
  FIRE: "Fire",
  EXPLOSION: "Explosion",
  MEDICAL_EMERGENCY: "Medical emergency",
  ACCIDENT: "Accident",
  STRUCTURAL_DAMAGE: "Structural damage",
  FLOOD: "Flood",
  LANDSLIDE: "Landslide",
  TRAPPED_PERSONS: "Trapped persons",
  MISSING_PERSONS: "Missing persons",
  ROAD_BLOCKAGE: "Road blockage",
  POWER_FAILURE: "Power failure",
  INFRASTRUCTURE_FAILURE: "Infrastructure failure",
  COMMUNICATION_FAILURE: "Communication failure",
  EARTHQUAKE: "Earthquake",
  SEVERE_WEATHER: "Severe weather",
  EVACUATION: "Evacuation",
  RESOURCE_SHORTAGE: "Resource shortage",
  HAZARDOUS_MATERIAL: "Hazardous material",
};

const METHOD_LABEL: Record<CategoryMethod, string> = {
  LEXICAL: "from report wording",
  SEMANTIC: "by semantic match",
  NONE: "no recognisable cue",
};

const RELATION_LABEL: Record<Relation, string> = {
  DUPLICATE: "Likely duplicate",
  POSSIBLE_DUPLICATE: "Possible duplicate",
  RELATED: "Related",
};

function humanise(value: string): string {
  return value.charAt(0) + value.slice(1).toLowerCase().replace(/_/g, " ");
}

/** "5 trapped (approx.) · 2 injured", or null when no people are counted. */
function peopleLine(summary: PeopleSummary): string | null {
  const parts: string[] = [];
  const add = (value: number | null, label: string) => {
    if (value !== null) parts.push(`${value} ${label}`);
  };
  add(summary.deceased, "dead");
  add(summary.trapped, "trapped");
  add(summary.missing, "missing");
  add(summary.injured, "injured");
  add(summary.displaced, "displaced");
  add(summary.affected, "affected");
  if (parts.length === 0) {
    return summary.unquantified ? "People mentioned, number not stated" : null;
  }
  const line = parts.join(" · ");
  return summary.approximate ? `${line} (approximate)` : line;
}

interface IncidentInsightViewProps {
  incident: Incident;
  status: IntelligenceStatus | null;
  /** Opens another incident, for following a related report. */
  onOpenIncident?: (incidentId: string) => void;
}

/**
 * What this device derived from one report.
 *
 * Everything here is computed locally from the report text and records this
 * node already holds. It is never written over the report, never replaces the
 * operator's severity, and never leaves the device. The rule-based part works
 * with no model installed; the model analysis below it is optional.
 */
export function IncidentInsightView({
  incident,
  status,
  onOpenIncident,
}: IncidentInsightViewProps) {
  const [insight, setInsight] = useState<IncidentInsight | null>(null);
  const [analysis, setAnalysis] = useState<AnalysisOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setInsight(null);
    setError(null);

    getIncidentInsight(incident.id)
      .then((result) => {
        if (!cancelled) setInsight(result);
      })
      .catch((raw) => {
        if (!cancelled) {
          setError((raw as CoreError).message ?? "Insight could not be computed.");
        }
      });

    // The model analysis is optional; its absence is not an error.
    getIncidentAnalysis(incident.id)
      .then((result) => {
        if (!cancelled) setAnalysis(result);
      })
      .catch(() => {
        if (!cancelled) setAnalysis(null);
      });

    return () => {
      cancelled = true;
    };
  }, [incident.id]);

  const extraction = insight?.extraction;
  const activeHazards = extraction
    ? extraction.hazards.filter((h) => !h.negated && !h.resolved)
    : [];
  const uniqueActive = Array.from(new Set(activeHazards.map((h) => h.hazard)));
  const ruledOut = extraction
    ? Array.from(
        new Set(
          extraction.hazards
            .filter((h) => h.negated || h.resolved)
            .map((h) => `${HAZARD_LABEL[h.hazard].toLowerCase()} (${h.negated ? "stated absent" : "easing"})`),
        ),
      )
    : [];
  const people = extraction ? peopleLine(extraction.peopleSummary) : null;

  return (
    <section className="insight" aria-label="On-device insight">
      <div className="insight__header">
        <span className="field__label">On-device insight</span>
        <span className="insight__badge" title="Computed on this device from the report text. Never sent to peers.">
          Derived locally · not shared
        </span>
      </div>

      {error && (
        <div className="alert" role="alert">
          <div className="alert__body">{error}</div>
        </div>
      )}

      {!insight && !error && (
        <div aria-busy="true">
          <div className="skeleton skeleton--line" />
          <div className="skeleton skeleton--line" />
        </div>
      )}

      {insight && extraction && (
        <>
          <dl className="key-value">
            <div className="key-value__row">
              <dt className="key-value__key">Type</dt>
              <dd className="key-value__value">
                {humanise(insight.category)}{" "}
                <span className="insight__method">{METHOD_LABEL[insight.categoryMethod]}</span>
                {insight.modelAgrees === false && (
                  <span className="analysis__disagreement"> model analysis differs</span>
                )}
              </dd>
            </div>

            <div className="key-value__row">
              <dt className="key-value__key">Severity</dt>
              <dd className="key-value__value analysis__severity">
                <span>
                  derived: <SeverityBadge severity={extraction.severity.level} />
                </span>
                <span>
                  operator: <SeverityBadge severity={incident.severity} />
                </span>
                {insight.severityDiffersFromOperator && (
                  <span className="analysis__disagreement">assessments differ</span>
                )}
              </dd>
            </div>

            <div className="key-value__row">
              <dt className="key-value__key">Why</dt>
              <dd className="key-value__value analysis__summary">{extraction.severity.reason}</dd>
            </div>

            {uniqueActive.length > 0 && (
              <div className="key-value__row">
                <dt className="key-value__key">Hazards</dt>
                <dd className="key-value__value">
                  <span className="insight__chips">
                    {uniqueActive.map((hazard) => (
                      <span key={hazard} className="insight__chip">
                        {HAZARD_LABEL[hazard]}
                      </span>
                    ))}
                  </span>
                </dd>
              </div>
            )}

            {ruledOut.length > 0 && (
              <div className="key-value__row">
                <dt className="key-value__key">Ruled out</dt>
                <dd className="key-value__value text-muted">{ruledOut.join(", ")}</dd>
              </div>
            )}

            {people && (
              <div className="key-value__row">
                <dt className="key-value__key">People</dt>
                <dd className="key-value__value">{people}</dd>
              </div>
            )}

            {extraction.locations.length > 0 && (
              <div className="key-value__row">
                <dt className="key-value__key">Places</dt>
                <dd className="key-value__value">{extraction.locations.join(", ")}</dd>
              </div>
            )}

            {extraction.routes.length > 0 && (
              <div className="key-value__row">
                <dt className="key-value__key">Routes</dt>
                <dd className="key-value__value">
                  <span className="insight__chips">
                    {extraction.routes.map((route) => (
                      <span
                        key={route.text}
                        className={`insight__chip ${route.blocked ? "insight__chip--blocked" : ""}`}
                      >
                        {route.text}
                        {route.blocked ? " · blocked" : ""}
                      </span>
                    ))}
                  </span>
                </dd>
              </div>
            )}

            {extraction.structures.length > 0 && (
              <div className="key-value__row">
                <dt className="key-value__key">Structures</dt>
                <dd className="key-value__value">{extraction.structures.join(", ")}</dd>
              </div>
            )}

            {extraction.organizations.length > 0 && (
              <div className="key-value__row">
                <dt className="key-value__key">Responders</dt>
                <dd className="key-value__value">{extraction.organizations.join(", ")}</dd>
              </div>
            )}

            {extraction.times.length > 0 && (
              <div className="key-value__row">
                <dt className="key-value__key">Times stated</dt>
                <dd className="key-value__value">{extraction.times.join(", ")}</dd>
              </div>
            )}

            {extraction.quantities.length > 0 && (
              <div className="key-value__row">
                <dt className="key-value__key">Quantities</dt>
                <dd className="key-value__value">
                  {extraction.quantities
                    .map((q) => `${q.approximate ? "~" : ""}${q.value} ${q.unit}`)
                    .join(", ")}
                </dd>
              </div>
            )}
          </dl>

          {extraction.severity.factors.length > 0 && (
            <details className="insight__factors">
              <summary>How the severity was derived (score {extraction.severity.score})</summary>
              <ul>
                {extraction.severity.factors.map((factor, index) => (
                  <li
                    key={`${factor.label}-${index}`}
                    className={factor.weight < 0 ? "insight__factor--negative" : undefined}
                  >
                    <span className="mono">
                      {factor.weight > 0 ? `+${factor.weight}` : factor.weight}
                    </span>{" "}
                    {factor.label}
                  </li>
                ))}
              </ul>
            </details>
          )}

          <div className="insight__related">
            <span className="field__label">
              Related reports
              <span className="insight__method">
                {" "}
                {insight.semanticAvailable ? "semantic match" : "word match (no vector yet)"}
              </span>
            </span>
            {insight.related.length === 0 ? (
              <span className="text-muted">No related reports on this node.</span>
            ) : (
              <ul className="insight__related-list">
                {insight.related.map((related) => (
                  <li key={related.incidentId}>
                    <button
                      type="button"
                      className="insight__related-item"
                      onClick={() => onOpenIncident?.(related.incidentId)}
                      disabled={!onOpenIncident}
                    >
                      <span className={`insight__relation insight__relation--${related.relation.toLowerCase()}`}>
                        {RELATION_LABEL[related.relation]}
                      </span>
                      <span className="insight__related-text">{related.excerpt}</span>
                      <span className="insight__related-meta">
                        {Math.round(related.similarity * 100)}% similar
                        {related.distanceKm !== null && ` · ${related.distanceKm} km away`}
                        {` · ${related.hoursApart} h apart`}
                        {related.method === "LEXICAL" && " · word match"}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </>
      )}

      <div className="insight__model">
        <span className="field__label">Model analysis (optional)</span>
        <IncidentAnalysisView
          incident={incident}
          analysis={analysis}
          status={status}
          onAnalysed={setAnalysis}
        />
      </div>

      <p className="insight__note">
        The report above is unchanged. These fields are computed on this device and are never
        sent to peers.
      </p>
    </section>
  );
}
