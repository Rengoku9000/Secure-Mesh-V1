import type { Severity } from "../types/core";

const CLASS_BY_SEVERITY: Record<Severity, string> = {
  LOW: "badge--low",
  MEDIUM: "badge--medium",
  HIGH: "badge--high",
  CRITICAL: "badge--critical",
};

/** Incident urgency, as a tinted chip. The label carries the meaning; the
 *  colour only reinforces it. */
export function SeverityBadge({ severity }: { severity: Severity }) {
  return (
    <span className={`badge ${CLASS_BY_SEVERITY[severity]}`}>{severity}</span>
  );
}
