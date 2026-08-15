import { IndexStateBadge } from "../../components/IndexStateBadge";
import { SeverityBadge } from "../../components/SeverityBadge";
import { SyncStatusBadge } from "../../components/SyncStatusBadge";
import {
  formatLocation,
  formatRelative,
  formatTimestamp,
  shortenId,
} from "../../lib/format";
import type { Incident, IncidentIndexState, IndexState } from "../../types/core";

interface IncidentTableProps {
  incidents: Incident[];
  loading: boolean;
  /**
   * Index state per incident ID. Empty when no model is provisioned, in which
   * case the AI column is omitted rather than showing a column of blanks.
   */
  indexStates: IncidentIndexState[];
}

/** The incident timeline, newest first. */
export function IncidentTable({
  incidents,
  loading,
  indexStates,
}: IncidentTableProps) {
  const stateFor = new Map<string, IndexState>(
    indexStates.map((entry) => [entry.incidentId, entry.state]),
  );
  const showIndexColumn = indexStates.length > 0;
  if (loading) {
    return (
      <div style={{ padding: "16px 20px", display: "grid", gap: 8 }} aria-busy="true">
        {[0, 1, 2].map((row) => (
          <div key={row} className="skeleton" style={{ height: 28 }} />
        ))}
      </div>
    );
  }

  if (incidents.length === 0) {
    return (
      <div className="empty-state">
        <p className="empty-state__title">No incidents recorded</p>
        <p className="empty-state__hint">
          Incidents created on this node are stored locally and remain available
          without a network connection.
        </p>
      </div>
    );
  }

  return (
    <div className="incident-table-wrapper">
      <table className="incident-table">
        <thead>
          <tr>
            <th scope="col">Incident ID</th>
            <th scope="col">Severity</th>
            <th scope="col">Description</th>
            <th scope="col">Location</th>
            <th scope="col">Recorded</th>
            <th scope="col">Sync</th>
            {showIndexColumn && <th scope="col">AI index</th>}
          </tr>
        </thead>
        <tbody>
          {incidents.map((incident) => (
            <tr key={incident.id}>
              <td className="incident-table__id" title={incident.id}>
                {shortenId(incident.id, 8, 4)}
              </td>
              <td>
                <SeverityBadge severity={incident.severity} />
              </td>
              <td className="incident-table__description">{incident.description}</td>
              <td className="incident-table__location">
                {formatLocation(incident.latitude, incident.longitude)}
              </td>
              <td
                className="incident-table__meta"
                title={formatTimestamp(incident.createdAt)}
              >
                {formatRelative(incident.createdAt)}
              </td>
              <td className="incident-table__meta">
                <SyncStatusBadge status={incident.syncStatus} />
              </td>
              {showIndexColumn && (
                <td className="incident-table__meta">
                  <IndexStateBadge
                    state={stateFor.get(incident.id) ?? "NOT_INDEXED"}
                  />
                </td>
              )}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
