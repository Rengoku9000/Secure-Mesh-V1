import { useState, type FormEvent } from "react";
import { Panel } from "../../components/Panel";
import { askSecureMesh, CoreError } from "../../lib/ipc";
import type {
  GroundedAnswer,
  IntelligenceStatus,
  PassageSource,
} from "../../types/core";

/**
 * What each citation is, in words rather than an enum name.
 *
 * Shown on every source because the two kinds carry different weight: an
 * operational document is standing guidance that was provisioned onto this
 * device, while a live incident is one unverified field report. An answer that
 * synthesises both must let a reader see which half came from where.
 */
const SOURCE_LABEL: Record<PassageSource, string> = {
  OPERATIONAL_KNOWLEDGE: "Operational Knowledge",
  LIVE_INCIDENT: "Live Incident",
  IMPORTED_DOCUMENT: "Imported Document",
};

/** Class suffix per source type, so the label is distinguishable at a glance. */
const SOURCE_MODIFIER: Record<PassageSource, string> = {
  OPERATIONAL_KNOWLEDGE: "operational",
  LIVE_INCIDENT: "incident",
  IMPORTED_DOCUMENT: "imported",
};

interface AskSecureMeshProps {
  status: IntelligenceStatus | null;
}

/**
 * Question answering over this node's own records.
 *
 * The answer and its sources are presented separately and labelled, because the
 * distinction matters operationally: an answer the model built from retrieved
 * passages is different in kind from one it produced without citing anything,
 * and presenting both the same way would invite the second to be trusted like
 * the first.
 */
export function AskSecureMesh({ status }: AskSecureMeshProps) {
  const [question, setQuestion] = useState("");
  const [answer, setAnswer] = useState<GroundedAnswer | null>(null);
  const [asking, setAsking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const ready = status?.state === "READY";

  async function handleSubmit(event: FormEvent) {
    event.preventDefault();
    if (question.trim() === "") {
      return;
    }

    setAsking(true);
    setError(null);
    try {
      setAnswer(await askSecureMesh(question, 5));
    } catch (raw) {
      setError((raw as CoreError).message ?? "The question could not be answered.");
      setAnswer(null);
    } finally {
      setAsking(false);
    }
  }

  return (
    <Panel
      title="Ask SecureMesh"
      subtitle={ready ? "Answered from local knowledge only" : "Requires a local model"}
    >
      <form className="form-grid" onSubmit={handleSubmit}>
        <div className="field">
          <label className="field__label" htmlFor="ask-question">
            Question
          </label>
          <textarea
            id="ask-question"
            className="textarea"
            style={{ minHeight: 64 }}
            value={question}
            maxLength={1000}
            disabled={!ready || asking}
            onChange={(event) => setQuestion(event.target.value)}
            placeholder="What high severity incidents are active near Zone A?"
          />
        </div>

        <div className="form-actions">
          <button
            type="submit"
            className="button button--primary"
            disabled={!ready || asking || question.trim() === ""}
          >
            {asking ? "Thinking locally…" : "Ask"}
          </button>
        </div>
      </form>

      {!ready && (
        <p className="intelligence-detail">
          {status?.detail ?? "Local intelligence is not available on this node."}
        </p>
      )}

      {error && (
        <div className="alert" role="alert">
          <div className="alert__body">{error}</div>
        </div>
      )}

      {answer && (
        <div className="answer">
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

          <p className="answer__text">{answer.answer}</p>

          {answer.refused && (
            <p className="intelligence-detail">
              {answer.generationMs === 0
                ? "Nothing in this node's local knowledge was close enough to the question to answer it, so the model was never called. If the operational knowledge pack is not installed, installing it gives the node standing field guidance to answer from."
                : "The model produced an answer that its cited passages do not support, so it was discarded rather than shown. That is what an answer drawn from the model's own training looks like from here."}
            </p>
          )}

          {!answer.refused && !answer.grounded && (
            <p className="intelligence-detail">
              The model did not cite any retrieved passage, so this answer is its
              own interpretation rather than something the local knowledge states.
            </p>
          )}

          {answer.droppedCitations > 0 && (
            <p className="intelligence-detail">
              The model cited {answer.droppedCitations} source
              {answer.droppedCitations === 1 ? "" : "s"} that were never
              retrieved. They have been discarded — treat the rest of this answer
              with caution.
            </p>
          )}

          {answer.sources.length > 0 && (
            <>
              <h4 className="answer__heading">Sources</h4>
              <ul className="source-list">
                {answer.sources.map((source) => (
                  <li
                    key={source.marker}
                    className={`source ${source.cited ? "source--cited" : ""}`}
                  >
                    <div className="source__header">
                      <span className="source__marker mono">[{source.marker}]</span>
                      <span
                        className={`source__type source__type--${SOURCE_MODIFIER[source.source]}`}
                      >
                        {SOURCE_LABEL[source.source]}
                      </span>
                      <span className="source__title">{source.title}</span>
                      <span className="source__score mono">
                        {source.score.toFixed(2)}
                      </span>
                      {!source.cited && (
                        <span className="source__uncited">retrieved, not cited</span>
                      )}
                    </div>
                    <p className="source__excerpt">{source.excerpt}</p>
                  </li>
                ))}
              </ul>
            </>
          )}

          <p className="answer__timing mono">
            retrieval {answer.retrievalMs} ms · generation {answer.generationMs} ms
            {!answer.refused &&
              ` · support ${(answer.answerSupport * 100).toFixed(0)}%`}{" "}
            · {answer.modelId}
          </p>
        </div>
      )}
    </Panel>
  );
}
