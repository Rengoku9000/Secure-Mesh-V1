import type { ComponentState } from "../types/core";

const CLASS_BY_STATE: Record<ComponentState, string> = {
  OPERATIONAL: "status-dot--operational",
  DEGRADED: "status-dot--degraded",
  INACTIVE: "status-dot--inactive",
};

/**
 * A subsystem state indicator.
 *
 * The dot is filled when a subsystem is running and hollow when it is not, so
 * state is legible without relying on colour alone. It is marked
 * `aria-hidden` because the adjacent text already names the state.
 */
export function StatusDot({ state }: { state: ComponentState }) {
  return <span className={`status-dot ${CLASS_BY_STATE[state]}`} aria-hidden="true" />;
}
