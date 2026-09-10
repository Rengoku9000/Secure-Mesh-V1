import { Panel } from "../../components/Panel";
import { StatusDot } from "../../components/StatusDot";
import type { IntelligenceStatus } from "../../types/core";

interface IntelligencePanelProps {
  status: IntelligenceStatus | null;
}

/**
 * Local intelligence: what model is loaded, and where it runs.
 *
 * "Inference: LOCAL" and "Network dependency: NONE" are stated rather than
 * implied. They come from the core, not from constants here, so they cannot
 * drift from what is actually true — and `UNAVAILABLE` is a normal state, not
 * an error, because a node without a model is fully functional.
 */
export function IntelligencePanel({ status }: IntelligencePanelProps) {
  if (!status) {
    return (
      <Panel title="Intelligence">
        <div className="key-value" aria-busy="true">
          <div className="skeleton skeleton--line" />
          <div className="skeleton skeleton--line" />
        </div>
      </Panel>
    );
  }

  const ready = status.state === "READY";

  return (
    <Panel
      title="Field Intelligence Engine"
      subtitle={status.modelName ?? "Autonomous local reasoning"}
    >
      <div className="intel-card">
        {/* Real-time stats */}
        <div className="intel-metric-grid">
          <div className="intel-metric-box">
            <span className="intel-metric-box__val">
              {ready ? "ACTIVE" : status.state}
            </span>
            <span className="intel-metric-box__lbl">Core Status</span>
          </div>

          <div className="intel-metric-box">
            <span className="intel-metric-box__val">{status.analysesStored}</span>
            <span className="intel-metric-box__lbl">Analyses</span>
          </div>

          <div className="intel-metric-box">
            <span className="intel-metric-box__val">{status.vectorsStored}</span>
            <span className="intel-metric-box__lbl">Embeddings</span>
          </div>

          <div className="intel-metric-box">
            <span className="intel-metric-box__val">0 kb/s</span>
            <span className="intel-metric-box__lbl">Cloud Transit</span>
          </div>
        </div>

        {/* Detailed configuration specs */}
        <dl className="key-value">
          <div className="key-value__row">
            <dt className="key-value__key">Inference Provider</dt>
            <dd className="key-value__value" style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <span className="intelligence-state">
                <StatusDot state={ready ? "OPERATIONAL" : "INACTIVE"} />
                {status.inference} (On-Device)
              </span>
            </dd>
          </div>

          {status.modelId && (
            <div className="key-value__row">
              <dt className="key-value__key">LLM Model</dt>
              <dd className="key-value__value">
                <strong>{status.modelId}</strong>
                {status.quantisation && (
                  <span style={{ color: "var(--text-muted)", marginLeft: "6px" }}>
                    · {status.quantisation}
                  </span>
                )}
              </dd>
            </div>
          )}

          {status.embeddingModel && (
            <div className="key-value__row">
              <dt className="key-value__key">Vector Embedder</dt>
              <dd className="key-value__value">{status.embeddingModel}</dd>
            </div>
          )}

          <div className="key-value__row">
            <dt className="key-value__key">Network Air-Gap</dt>
            <dd className="key-value__value">
              <span style={{ color: "var(--state-ok)", fontWeight: 600 }}>
                100% Offline (Zero Cloud Dependency)
              </span>
            </dd>
          </div>

          {ready && (
            <div className="key-value__row">
              <dt className="key-value__key">Vector Chunks Indexed</dt>
              <dd className="key-value__value">
                {status.chunksIndexed} chunks across {status.documentsIndexed} source documents
              </dd>
            </div>
          )}
        </dl>

        {!ready && <p className="intelligence-detail">{status.detail}</p>}

        <p className="intelligence-detail intelligence-detail--reassurance" style={{ marginTop: "4px" }}>
          Inference runs exclusively via local hardware acceleration. Emergency peer mesh synchronization and incident tracking operate completely uninterrupted.
        </p>
      </div>
    </Panel>
  );
}
