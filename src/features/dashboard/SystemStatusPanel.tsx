import { Panel } from "../../components/Panel";
import { StatusDot } from "../../components/StatusDot";
import type { ComponentStatus, SystemStatus } from "../../types/core";

interface SystemStatusPanelProps {
  status: SystemStatus | null;
}

/** Fixed display order, most fundamental subsystem first. */
const ROWS: { key: keyof SystemStatus; name: string }[] = [
  { key: "database", name: "Local database" },
  { key: "identity", name: "Node identity" },
  { key: "network", name: "Network" },
  { key: "ai", name: "Local AI" },
  { key: "location", name: "Location" },
  { key: "map", name: "Local map" },
  { key: "tee", name: "TEE" },
];

function StatusRow({ name, status }: { name: string; status: ComponentStatus }) {
  return (
    <li className="status-row">
      <span className="status-row__dot">
        <StatusDot state={status.state} />
      </span>
      <span>
        <span className="status-row__name">{name}</span>
        <div className="status-row__value">{status.label}</div>
        <div className="status-row__detail">{status.detail}</div>
      </span>
    </li>
  );
}

/**
 * Subsystem health.
 *
 * Every value shown here comes from the Rust core. The UI does not infer
 * status, which is what keeps the AI and TEE rows honest: they read as absent
 * because the core reports them absent, not because a constant is hardcoded
 * in the frontend.
 */
export function SystemStatusPanel({ status }: SystemStatusPanelProps) {
  if (!status) {
    return (
      <Panel title="System status">
        <div className="status-list" aria-busy="true">
          {ROWS.map((row) => (
            <div key={row.key} className="skeleton skeleton--stat" />
          ))}
        </div>
      </Panel>
    );
  }

  return (
    <Panel title="System status" subtitle="Reported by the local core">
      <ul className="status-list">
        {ROWS.map((row) => (
          <StatusRow key={row.key} name={row.name} status={status[row.key]} />
        ))}
      </ul>
    </Panel>
  );
}
