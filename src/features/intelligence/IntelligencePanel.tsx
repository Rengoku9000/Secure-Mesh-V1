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
    <Panel title="Intelligence" subtitle={status.modelName ?? "No model provisioned"}>
      <dl className="key-value">
        <div className="key-value__row">
          <dt className="key-value__key">Status</dt>
          <dd className="key-value__value">
            <span className="intelligence-state">
              <StatusDot state={ready ? "OPERATIONAL" : "INACTIVE"} />
              {status.state}
            </span>
          </dd>
        </div>

        {status.modelId && (
          <div className="key-value__row">
            <dt className="key-value__key">Model</dt>
            <dd className="key-value__value">
              {status.modelId}
              {status.quantisation && ` · ${status.quantisation}`}
            </dd>
          </div>
        )}

        {status.embeddingModel && (
          <div className="key-value__row">
            <dt className="key-value__key">Embeddings</dt>
            <dd className="key-value__value">{status.embeddingModel}</dd>
          </div>
        )}

        <div className="key-value__row">
          <dt className="key-value__key">Inference</dt>
          <dd className="key-value__value">{status.inference}</dd>
        </div>

        <div className="key-value__row">
          <dt className="key-value__key">Network dependency</dt>
          <dd className="key-value__value">{status.networkDependency}</dd>
        </div>

        {ready && (
          <div className="key-value__row">
            <dt className="key-value__key">Local index</dt>
            <dd className="key-value__value">
              {status.analysesStored} analyses · {status.chunksIndexed} chunks ·{" "}
              {status.vectorsStored} vectors
            </dd>
          </div>
        )}
      </dl>

      {!ready && <p className="intelligence-detail">{status.detail}</p>}

      {!ready && (
        <p className="intelligence-detail intelligence-detail--reassurance">
          Incident capture, peer-to-peer synchronisation and the dashboard are
          unaffected.
        </p>
      )}
    </Panel>
  );
}
