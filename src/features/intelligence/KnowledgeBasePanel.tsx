import { useCallback, useEffect, useState } from "react";
import { Panel } from "../../components/Panel";
import {
  CoreError,
  getKnowledgeSummary,
  installOperationalKnowledge,
} from "../../lib/ipc";
import type { IntelligenceStatus, KnowledgeBaseSummary } from "../../types/core";

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
 *
 * Everything here is local. The install button copies documents that are
 * already inside the binary into the index; it opens no connection and reads no
 * file, so it works identically with the network unplugged.
 */
export function KnowledgeBasePanel({ status, onChanged }: KnowledgeBasePanelProps) {
  const [summary, setSummary] = useState<KnowledgeBaseSummary | null>(null);
  const [installing, setInstalling] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setSummary(await getKnowledgeSummary());
    } catch {
      // A node with no model still returns a summary, so a failure here is a
      // genuine fault rather than an expected absence. The panel degrades to
      // its loading shape rather than taking the dashboard down with it.
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

  if (!summary) {
    return (
      <Panel title="Knowledge base">
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
    <Panel title="Knowledge base" subtitle="Local knowledge · nothing is fetched">
      <dl className="key-value">
        <div className="key-value__row">
          <dt className="key-value__key">Operational documents</dt>
          <dd className="key-value__value">
            {summary.operationalDocuments} of {summary.packDocumentsAvailable}
          </dd>
        </div>

        {summary.importedDocuments > 0 && (
          <div className="key-value__row">
            <dt className="key-value__key">Imported documents</dt>
            <dd className="key-value__value">{summary.importedDocuments}</dd>
          </div>
        )}

        <div className="key-value__row">
          <dt className="key-value__key">Live incidents</dt>
          <dd className="key-value__value">
            {summary.liveIncidentsIndexed}
            {/* Stated rather than hidden: an incident with no vector is not
                retrievable yet, and silently counting it as searchable would
                misdescribe what a question can actually find. */}
            {indexingBehind && ` indexed · ${summary.liveIncidentsTotal} held`}
          </dd>
        </div>

        <div className="key-value__row">
          <dt className="key-value__key">Chunks</dt>
          <dd className="key-value__value">{summary.chunks}</dd>
        </div>

        <div className="key-value__row">
          <dt className="key-value__key">Vectors</dt>
          <dd className="key-value__value">{summary.vectors}</dd>
        </div>
      </dl>

      <div className="knowledge-actions">
        <button
          type="button"
          className="button button--secondary button--compact"
          onClick={() => void install()}
          disabled={installing || !modelReady}
        >
          {installing
            ? "Installing…"
            : summary.packInstalled
              ? "Reinstall operational knowledge"
              : "Install operational knowledge"}
        </button>
      </div>

      {message && <p className="intelligence-detail">{message}</p>}
      {error && (
        <div className="alert" role="alert">
          <div className="alert__body">{error}</div>
        </div>
      )}

      {!modelReady && (
        <p className="intelligence-detail">
          Installing needs the local embedding model, because a document that
          cannot be embedded is not something this node could retrieve.
        </p>
      )}

      <p className="intelligence-detail intelligence-detail--reassurance">
        The operational documents are compiled into this application. Installing
        them copies text that is already on this device into the local index —
        no download, no API, no network of any kind.
      </p>
    </Panel>
  );
}
