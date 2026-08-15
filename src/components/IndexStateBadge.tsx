import type { IndexState } from "../types/core";

const DOT_CLASS: Record<IndexState, string> = {
  NOT_INDEXED: "",
  INDEXING: "",
  INDEXED: "sync-badge__dot--synced",
  INDEX_FAILED: "sync-badge__dot--failed",
};

const LABEL: Record<IndexState, string> = {
  NOT_INDEXED: "NOT INDEXED",
  INDEXING: "INDEXING",
  INDEXED: "INDEXED",
  INDEX_FAILED: "FAILED",
};

const TITLE: Record<IndexState, string> = {
  NOT_INDEXED: "Stored, but not yet searchable by local AI. Indexing is automatic.",
  INDEXING: "Building a local embedding for this incident.",
  INDEXED: "Searchable by local AI on this node.",
  INDEX_FAILED:
    "Could not be embedded — usually no local model. The incident is safe and will be retried.",
};

/**
 * Whether an incident is searchable by this node's local AI.
 *
 * Deliberately a separate badge from {@link SyncStatusBadge}. Replication and
 * indexing are independent: a vector is derived local state that each node
 * builds for itself, so an incident can be SYNCED to every peer and still not
 * be INDEXED here. Showing one badge for both would tell an operator something
 * untrue.
 */
export function IndexStateBadge({ state }: { state: IndexState }) {
  return (
    <span className="sync-badge" title={TITLE[state]}>
      <span className={`sync-badge__dot ${DOT_CLASS[state]}`} aria-hidden="true" />
      {LABEL[state]}
    </span>
  );
}
