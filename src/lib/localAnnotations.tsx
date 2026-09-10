import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";

/**
 * Operator annotations that live only on this device.
 *
 * Two things the operator wants to record are deliberately *not* changes to the
 * record itself:
 *
 * - A **local call sign** for this node. The node's `nodeName` comes from the
 *   keystore alongside its key material and is what peers see; renaming it
 *   would rewrite identity. A call sign sits beside that name for this
 *   operator's convenience and is never replicated.
 *
 * - A **dispute flag** on an incident. An incident is evidence: a report that
 *   turns out to be wrong is still a fact about what was reported and when, so
 *   it is marked rather than destroyed. The flag is this node's own assessment
 *   and does not alter, and cannot alter, the signed record it refers to.
 *
 * Both are held in `localStorage`, so neither crosses the IPC boundary and
 * neither reaches the mesh. Clearing site data loses them, which is the correct
 * trade for something that was never authoritative.
 */

const CALL_SIGN_KEY = "securemesh.node.call_sign";
const DISPUTES_KEY = "securemesh.incidents.disputes";

/** Why an operator believes a report is not to be trusted. */
export type DisputeReason = "INCORRECT" | "FALSE_REPORT" | "DUPLICATE" | "RESOLVED";

export const DISPUTE_LABELS: Record<DisputeReason, string> = {
  INCORRECT: "Details incorrect",
  FALSE_REPORT: "False report",
  DUPLICATE: "Duplicate of another report",
  RESOLVED: "No longer active",
};

export interface Dispute {
  reason: DisputeReason;
  /** Free-text context the operator added, if any. */
  note?: string;
  /** When this node flagged it, ISO-8601. */
  flaggedAt: string;
}

interface LocalAnnotationsValue {
  /** Operator's own call sign for this node, or null if none set. */
  callSign: string | null;
  setCallSign: (next: string | null) => void;
  /** The dispute recorded against an incident, if this node flagged one. */
  disputeFor: (incidentId: string) => Dispute | null;
  flagIncident: (incidentId: string, reason: DisputeReason, note?: string) => void;
  clearFlag: (incidentId: string) => void;
  /** How many incidents this node has flagged. */
  disputedCount: number;
}

const LocalAnnotationsContext = createContext<LocalAnnotationsValue | null>(null);

function readCallSign(): string | null {
  try {
    const stored = localStorage.getItem(CALL_SIGN_KEY);
    return stored && stored.trim() !== "" ? stored : null;
  } catch {
    // Private-mode or restricted storage: no call sign.
    return null;
  }
}

function readDisputes(): Record<string, Dispute> {
  try {
    const raw = localStorage.getItem(DISPUTES_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    // Anything that is not a plain object is treated as absent rather than
    // trusted — this value has been sitting in storage between sessions.
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return parsed as Record<string, Dispute>;
  } catch {
    return {};
  }
}

export function LocalAnnotationsProvider({ children }: { children: ReactNode }) {
  const [callSign, setCallSignState] = useState<string | null>(readCallSign);
  const [disputes, setDisputes] = useState<Record<string, Dispute>>(readDisputes);

  const setCallSign = useCallback((next: string | null) => {
    const trimmed = next?.trim() ?? "";
    const value = trimmed === "" ? null : trimmed;
    setCallSignState(value);
    try {
      if (value === null) {
        localStorage.removeItem(CALL_SIGN_KEY);
      } else {
        localStorage.setItem(CALL_SIGN_KEY, value);
      }
    } catch {
      // Storage unavailable: the call sign holds for this session only.
    }
  }, []);

  const persist = useCallback((next: Record<string, Dispute>) => {
    try {
      localStorage.setItem(DISPUTES_KEY, JSON.stringify(next));
    } catch {
      // Storage unavailable: flags hold for this session only.
    }
    return next;
  }, []);

  const flagIncident = useCallback(
    (incidentId: string, reason: DisputeReason, note?: string) => {
      setDisputes((prev) =>
        persist({
          ...prev,
          [incidentId]: {
            reason,
            note: note?.trim() ? note.trim() : undefined,
            flaggedAt: new Date().toISOString(),
          },
        }),
      );
    },
    [persist],
  );

  const clearFlag = useCallback(
    (incidentId: string) => {
      setDisputes((prev) => {
        if (!(incidentId in prev)) return prev;
        const next = { ...prev };
        delete next[incidentId];
        return persist(next);
      });
    },
    [persist],
  );

  const disputeFor = useCallback(
    (incidentId: string) => disputes[incidentId] ?? null,
    [disputes],
  );

  const value = useMemo(
    () => ({
      callSign,
      setCallSign,
      disputeFor,
      flagIncident,
      clearFlag,
      disputedCount: Object.keys(disputes).length,
    }),
    [callSign, setCallSign, disputeFor, flagIncident, clearFlag, disputes],
  );

  return (
    <LocalAnnotationsContext.Provider value={value}>
      {children}
    </LocalAnnotationsContext.Provider>
  );
}

/** Access to this device's operator annotations. */
export function useLocalAnnotations(): LocalAnnotationsValue {
  const ctx = useContext(LocalAnnotationsContext);
  if (!ctx) {
    // Rendered outside the provider: behave as though nothing is annotated.
    return {
      callSign: null,
      setCallSign: () => {},
      disputeFor: () => null,
      flagIncident: () => {},
      clearFlag: () => {},
      disputedCount: 0,
    };
  }
  return ctx;
}
