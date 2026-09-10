import { useCallback, useEffect, useState } from "react";
import { Panel } from "../../components/Panel";
import {
  CoreError,
  getKnowledgeSummary,
  getKnowledgeDocuments,
  installOperationalKnowledge,
  indexIntelligence,
} from "../../lib/ipc";
import type {
  IntelligenceStatus,
  KnowledgeBaseSummary,
  KnowledgeDocument,
} from "../../types/core";

interface KnowledgeBasePanelProps {
  status: IntelligenceStatus | null;
  /** Lets the dashboard re-read counts that indexing has since changed. */
  onChanged: () => void;
}

/**
 * What this node can answer from.
 *
 * Two kinds of knowledge, counted separately because they are not the same
 * thing: operational documents are stable field guidance provisioned onto the
 * device, and incidents are dynamic reports that arrive as events. Retrieval
 * searches both, and an answer may cite both.
 */
export function KnowledgeBasePanel({ status, onChanged }: KnowledgeBasePanelProps) {
  const [summary, setSummary] = useState<KnowledgeBaseSummary | null>(null);
  const [docs, setDocs] = useState<KnowledgeDocument[]>([]);
  const [installing, setInstalling] = useState(false);
  const [indexing, setIndexing] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [sum, documents] = await Promise.all([
        getKnowledgeSummary(),
        getKnowledgeDocuments().catch(() => []),
      ]);
      setSummary(sum);
      setDocs(documents);
    } catch {
      setSummary(null);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh, status?.chunksIndexed, status?.vectorsStored]);

  async function install() {
    setInstalling(true);
    setError(null);
    setMessage(null);
    try {
      const report = await installOperationalKnowledge();
      setMessage(
        report.documentsInstalled === 0
          ? `Already installed — ${report.documentsAlreadyPresent} documents present, nothing added.`
          : `Installed ${report.documentsInstalled} document${
              report.documentsInstalled === 1 ? "" : "s"
            } · ${report.chunksCreated} chunks. Embedding runs in the background.`,
      );
      await refresh();
      onChanged();
    } catch (raw) {
      const coreError = raw as CoreError;
      setError(coreError.message ?? "The knowledge pack could not be installed.");
    } finally {
      setInstalling(false);
    }
  }

  async function handleTriggerIndex() {
    setIndexing(true);
    setError(null);
    setMessage(null);
    try {
      const report = await indexIntelligence();
      setMessage(
        `Index reconciliation complete: ${report.chunksEmbedded} chunks and ${report.incidentsEmbedded} incidents newly embedded (${report.failures} failures).`,
      );
      await refresh();
      onChanged();
    } catch (raw) {
      const coreError = raw as CoreError;
      setError(coreError.message ?? "Failed to re-index knowledge.");
    } finally {
      setIndexing(false);
    }
  }

  if (!summary) {
    return (
      <Panel title="Knowledge Base & Protocols">
        <div className="key-value" aria-busy="true">
          <div className="skeleton skeleton--line" />
          <div className="skeleton skeleton--line" />
        </div>
      </Panel>
    );
  }

  const modelReady = status?.state === "READY";
  const indexingBehind = summary.liveIncidentsTotal > summary.liveIncidentsIndexed;

  return (
    <Panel
      title="Knowledge Base & Field Protocols"
      subtitle="Air-gapped documents & vector retrieval corpus"
    >
      <div className="intel-card">
        {/* Knowledge corpus metrics */}
        <div className="intel-metric-grid">
          <div className="intel-metric-box">
            <span className="intel-metric-box__val">
              {summary.operationalDocuments} / {summary.packDocumentsAvailable}
            </span>
            <span className="intel-metric-box__lbl">Manuals</span>
          </div>

          <div className="intel-metric-box">
            <span className="intel-metric-box__val">
              {summary.liveIncidentsIndexed} / {summary.liveIncidentsTotal}
            </span>
            <span className="intel-metric-box__lbl">
              {indexingBehind ? "Indexed (Syncing...)" : "Indexed Reports"}
            </span>
          </div>

          <div className="intel-metric-box">
            <span className="intel-metric-box__val">{summary.chunks}</span>
            <span className="intel-metric-box__lbl">Passage Chunks</span>
          </div>

          <div className="intel-metric-box">
            <span className="intel-metric-box__val">{summary.vectors}</span>
            <span className="intel-metric-box__lbl">Vectors</span>
          </div>
        </div>

        {/* Installed Documents Listing */}
        {docs.length > 0 && (
          <div>
            <h4
              style={{
                fontSize: "11px",
                fontWeight: 600,
                letterSpacing: "var(--tracking-wider)",
                textTransform: "uppercase",
                color: "var(--text-muted)",
                marginBottom: "8px",
              }}
            >
              Installed Operational Manuals ({docs.length})
            </h4>
            <div className="intel-doc-list">
              {docs.map((doc) => (
                <div key={doc.id} className="intel-doc-item">
                  <div className="intel-doc-info">
                    <span className="intel-doc-title" title={doc.title}>
                      {doc.title}
                    </span>
                    <span className="intel-doc-meta">
                      {doc.sourceType} · {doc.chunkCount} vector chunk{doc.chunkCount === 1 ? "" : "s"}
                    </span>
                  </div>
                  <span className="intel-doc-badge">EMBEDDED</span>
                </div>
              ))}
            </div>
          </div>
        )}

        {/* Action Controls */}
        <div style={{ display: "flex", flexWrap: "wrap", gap: "8px", marginTop: "4px" }}>
          <button
            type="button"
            className="button button--secondary button--compact"
            onClick={() => void install()}
            disabled={installing || !modelReady}
          >
            {installing
              ? "Installing…"
              : summary.packInstalled
                ? "Re-install Operational Protocols"
                : "Install Operational Protocols"}
          </button>

          <button
            type="button"
            className="button button--secondary button--compact"
            onClick={() => void handleTriggerIndex()}
            disabled={indexing || !modelReady}
            title="Force immediate embedding pass on any pending incident reports or chunks"
          >
            {indexing ? "Indexing…" : "Run Vector Indexer"}
          </button>
        </div>

        {message && <p className="intelligence-detail" style={{ color: "var(--accent)" }}>{message}</p>}
        {error && (
          <div className="alert" role="alert">
            <div className="alert__body">{error}</div>
          </div>
        )}

        {!modelReady && (
          <p className="intelligence-detail">
            Installing operational manuals requires the local embedding engine to be loaded so chunks can be indexed immediately.
          </p>
        )}

        <p className="intelligence-detail intelligence-detail--reassurance">
          Operational field documents are compiled directly into this binary. Adding or indexing them copies text strictly on this local device without external requests.
        </p>
      </div>
    </Panel>
  );
}

