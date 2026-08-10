import type { ReactNode } from "react";

interface PanelProps {
  title: string;
  subtitle?: string;
  /** Rendered at the right of the header, typically an action button. */
  actions?: ReactNode;
  /** Removes body padding, for content that manages its own (e.g. tables). */
  flush?: boolean;
  children: ReactNode;
}

/** A titled section of the dashboard. */
export function Panel({ title, subtitle, actions, flush, children }: PanelProps) {
  return (
    <section className="panel" aria-label={title}>
      <header className="panel__header">
        <div className="panel__heading">
          <h2 className="panel__title">{title}</h2>
          {subtitle && <span className="panel__subtitle">{subtitle}</span>}
        </div>
        {actions}
      </header>
      <div className={flush ? "panel__body panel__body--flush" : "panel__body"}>
        {children}
      </div>
    </section>
  );
}
