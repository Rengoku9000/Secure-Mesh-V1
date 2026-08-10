import type { SyncStatus } from "../types/core";

const DOT_CLASS: Record<SyncStatus, string> = {
  PENDING: "",
  SYNCING: "",
  SYNCED: "sync-badge__dot--synced",
  FAILED: "sync-badge__dot--failed",
};

/**
 * Whether a record has reached a peer.
 *
 * In Phase 1 every record reads PENDING, which is accurate rather than
 * decorative: there is no transport yet, so nothing has been propagated.
 */
export function SyncStatusBadge({ status }: { status: SyncStatus }) {
  return (
    <span className="sync-badge">
      <span className={`sync-badge__dot ${DOT_CLASS[status]}`} aria-hidden="true" />
      {status}
    </span>
  );
}
