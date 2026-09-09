import { useState, useEffect, useRef, type FormEvent } from "react";
import { askSecureMesh, CoreError } from "../../lib/ipc";
import type {
  GroundedAnswer,
  IntelligenceStatus,
  PassageSource,
} from "../../types/core";

const SOURCE_LABEL: Record<PassageSource, string> = {
  OPERATIONAL_KNOWLEDGE: "Operational Knowledge",
  LIVE_INCIDENT: "Live Incident",
  IMPORTED_DOCUMENT: "Imported Document",
};

const SOURCE_MODIFIER: Record<PassageSource, string> = {
  OPERATIONAL_KNOWLEDGE: "operational",
  LIVE_INCIDENT: "incident",
  IMPORTED_DOCUMENT: "imported",
};

const SUGGESTED_PROMPTS = [
  "What critical incidents are currently active?",
  "Summarize active mesh nodes & connectivity",
  "Are there any unverified field hazard reports?",
  "Standard emergency protocol for casualty breach",
];

interface AskAiDrawerProps {
  isOpen: boolean;
  onClose: () => void;
  status: IntelligenceStatus | null;
  onNavigateToIntel?: () => void;
}

/**
 * Slide-over tactical AI assistant drawer.
 *
 * Accessible globally via header pill or Ctrl+K shortcut from any operational view
 * (Map, Incidents, Mesh, etc.) without losing viewport state or interrupting active tasks.
 */
export function AskAiDrawer({
  isOpen,
  onClose,
  status,
  onNavigateToIntel,
}: AskAiDrawerProps) {
  const [question, setQuestion] = useState("");
  const [answer, setAnswer] = useState<GroundedAnswer | null>(null);
  const [asking, setAsking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const ready = status?.state === "READY";

  // Focus input automatically when opened and listen for Escape key
  useEffect(() => {
    if (!isOpen) return;

    const timer = setTimeout(() => {
      textareaRef.current?.focus();
    }, 120);

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [isOpen, onClose]);

  async function executeQuery(queryText: string) {
    const q = queryText.trim();
    if (!q) return;

    setAsking(true);
    setError(null);
    setCopied(false);
    try {
      const res = await askSecureMesh(q, 5);
      setAnswer(res);
    } catch (raw) {
      setError((raw as CoreError).message ?? "The question could not be answered.");
      setAnswer(null);
    } finally {
      setAsking(false);
    }
  }

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    void executeQuery(question);
  }

  function handlePromptClick(prompt: string) {
    setQuestion(prompt);
    void executeQuery(prompt);
  }

  function handleCopyAnswer() {
    if (!answer?.answer) return;
    void navigator.clipboard.writeText(answer.answer);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  }

  if (!isOpen) return null;

  return (
    <div
      className="ai-drawer-backdrop"
      role="presentation"
      onClick={(e) => {
        if (e.target === e.currentTarget) {
          onClose();
        }
      }}
    >
      <div
        className="ai-drawer"
        role="dialog"
        aria-modal="true"
        aria-label="SecureMesh AI Tactical Assistant"
      >
        {/* Header */}
        <div className="ai-drawer__header">
          <div className="ai-drawer__brand">
            <div className="ai-drawer__icon" aria-hidden="true">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round">
                <path d="M12 2v4M12 18v4M4.93 4.93l2.83 2.83M16.24 16.24l2.83 2.83" />
                <circle cx="12" cy="12" r="3" fill="currentColor" opacity="0.4" />
              </svg>
            </div>
            <div>
              <h2 className="ai-drawer__title">SecureMesh AI</h2>
              <span
                className={`ai-drawer__status-pill ${
                  ready ? "" : "ai-drawer__status-pill--standby"
                }`}
              >
                <span
                  style={{
                    width: 5,
                    height: 5,
                    borderRadius: "50%",
                    backgroundColor: ready ? "var(--state-ok)" : "var(--state-warn)",
                  }}
                />
                {ready ? "Local Model Ready" : "Standby / Setup Needed"}
              </span>
            </div>
          </div>

          <button
            type="button"
            className="ai-drawer__close"
            onClick={onClose}
            aria-label="Close AI assistant (Esc)"
            title="Close (Esc)"
          >
            <span>Close</span>
            <kbd className="mono" style={{ fontSize: 10 }}>Esc</kbd>
          </button>
        </div>

        {/* Body Content */}
        <div className="ai-drawer__body">
          {/* Engine Standby Banner if not ready */}
          {!ready && (
            <div className="alert alert--warn" role="alert" style={{ borderRadius: "var(--radius-lg)" }}>
              <div className="alert__body">
                <p style={{ margin: 0, fontWeight: 600 }}>
                  {status?.detail ?? "Local intelligence model is not yet provisioned."}
                </p>
                {onNavigateToIntel && (
                  <button
                    type="button"
                    className="button button--secondary button--compact"
                    style={{ marginTop: 8 }}
                    onClick={() => {
                      onClose();
                      onNavigateToIntel();
                    }}
                  >
                    Open AI Intel Settings →
                  </button>
                )}
              </div>
            </div>
          )}

          {/* Prompt Input Box */}
          <form className="ai-drawer__input-box" onSubmit={handleSubmit}>
            <textarea
              ref={textareaRef}
              className="ai-drawer__textarea"
              value={question}
              onChange={(e) => setQuestion(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void executeQuery(question);
                }
              }}
              placeholder={
                ready
                  ? "Ask anything about incidents, mesh peers, or emergency protocols..."
                  : "Engine standby: configure local weights in AI Intel tab"
              }
              rows={2}
              maxLength={1000}
              disabled={!ready || asking}
            />

            <div className="ai-drawer__input-footer">
              <span className="ai-drawer__hint">
                Press <kbd className="mono" style={{ fontSize: 10 }}>Enter ↵</kbd> to query · Shift+Enter for newline
              </span>
              <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                {question && (
                  <button
                    type="button"
                    className="button button--ghost button--compact"
                    onClick={() => {
                      setQuestion("");
                      setAnswer(null);
                      setError(null);
                    }}
                    style={{ height: 30, fontSize: 11 }}
                  >
                    Clear
                  </button>
                )}
                <button
                  type="submit"
                  className="button button--primary button--compact"
                  disabled={!ready || asking || question.trim() === ""}
                  style={{ height: 30, gap: 5, padding: "4px 12px", fontSize: 12 }}
                >
                  {asking ? (
                    <>
                      <svg className="map__hud-spinner" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
                        <path d="M21 12a9 9 0 1 1-6.219-8.56" />
                      </svg>
                      <span>Thinking…</span>
                    </>
                  ) : (
                    <>
                      <span>Query</span>
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
                        <line x1="22" y1="2" x2="11" y2="13" />
                        <polygon points="22 2 15 22 11 13 2 9 22 2" />
                      </svg>
                    </>
                  )}
                </button>
              </div>
            </div>
          </form>

          {/* Quick Suggested Prompts (when no answer yet or idle) */}
          {!answer && (
            <div className="ai-drawer__prompts">
              <span className="ai-drawer__prompts-title">Suggested Quick Inquiries</span>
              <div className="ai-drawer__chips">
                {SUGGESTED_PROMPTS.map((prompt, idx) => (
                  <button
                    key={idx}
                    type="button"
                    className="ai-drawer__chip"
                    onClick={() => handlePromptClick(prompt)}
                    disabled={!ready || asking}
                  >
                    <span style={{ color: "var(--accent)" }}>✦</span>
                    <span>{prompt}</span>
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Error notice */}
          {error && (
            <div className="alert alert--error" role="alert" style={{ borderRadius: "var(--radius-lg)" }}>
              <div className="alert__body">{error}</div>
            </div>
          )}

          {/* Answer Card */}
          {answer && (
            <div className="answer" style={{ borderRadius: "var(--radius-xl)", margin: 0 }}>
              <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8, flexWrap: "wrap", marginBottom: 8 }}>
                <div
                  className={`answer__badge ${
                    answer.refused
                      ? "answer__badge--refused"
                      : answer.grounded
                        ? "answer__badge--grounded"
                        : "answer__badge--ungrounded"
                  }`}
                >
                  {answer.refused
                    ? "NO RELEVANT LOCAL KNOWLEDGE"
                    : answer.grounded
                      ? "ANSWERED FROM LOCAL KNOWLEDGE"
                      : "MODEL INTERPRETATION — NOT CITED"}
                </div>

                <button
                  type="button"
                  className="button button--ghost button--compact"
                  onClick={handleCopyAnswer}
                  title="Copy answer text"
                  style={{ height: 26, fontSize: 11, gap: 4 }}
                >
                  {copied ? (
                    <>
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
                        <polyline points="20 6 9 17 4 12" />
                      </svg>
                      <span>Copied</span>
                    </>
                  ) : (
                    <>
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                        <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
                        <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
                      </svg>
                      <span>Copy</span>
                    </>
                  )}
                </button>
              </div>

              <p className="answer__text" style={{ fontSize: 13.5, lineHeight: 1.55 }}>
                {answer.answer}
              </p>

              {answer.refused && (
                <p className="intelligence-detail" style={{ margin: "8px 0" }}>
                  {answer.generationMs === 0
                    ? "Nothing in this node's local records was close enough to answer. Ensure the operational knowledge pack or relevant incidents are saved."
                    : "The model response could not be verified by cited passages."}
                </p>
              )}

              {/* Citations List */}
              {answer.sources.length > 0 && (
                <div style={{ marginTop: 14 }}>
                  <h4 className="answer__heading" style={{ fontSize: 11, marginBottom: 6 }}>
                    Grounded Source Documents ({answer.sources.length})
                  </h4>
                  <ul className="source-list" style={{ gap: 6 }}>
                    {answer.sources.map((source) => (
                      <li
                        key={source.marker}
                        className={`source ${source.cited ? "source--cited" : ""}`}
                        style={{ borderRadius: "var(--radius-md)" }}
                      >
                        <div className="source__header" style={{ gap: 6, flexWrap: "wrap" }}>
                          <span className="source__marker mono">[{source.marker}]</span>
                          <span
                            className={`source__type source__type--${SOURCE_MODIFIER[source.source]}`}
                          >
                            {SOURCE_LABEL[source.source]}
                          </span>
                          <span className="source__title">{source.title}</span>
                          <span className="source__score mono">
                            {(source.score * 100).toFixed(0)}% match
                          </span>
                          {!source.cited && (
                            <span className="source__uncited">retrieved, not cited</span>
                          )}
                        </div>
                        <p className="source__excerpt" style={{ fontSize: 11.5, marginTop: 4 }}>
                          {source.excerpt}
                        </p>
                      </li>
                    ))}
                  </ul>
                </div>
              )}

              {/* Telemetry Footer */}
              <p className="answer__timing mono" style={{ fontSize: 10.5, marginTop: 12 }}>
                retrieval: {answer.retrievalMs}ms · generation: {answer.generationMs}ms
                {!answer.refused &&
                  ` · support: ${(answer.answerSupport * 100).toFixed(0)}%`}{" "}
                · {answer.modelId}
              </p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
