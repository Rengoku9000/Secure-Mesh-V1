import type { ReactNode } from "react";
import { QwenIcon } from "./QwenIcon";

export type NavTab = "map" | "incidents" | "mesh" | "intel" | "node";

interface BottomNavProps {
  activeTab: NavTab;
  onChange: (tab: NavTab) => void;
  pendingSyncCount?: number;
  unapprovedPeersCount?: number;
}

interface NavItem {
  id: NavTab;
  label: string;
  badge?: number;
  icon: (isActive: boolean) => ReactNode;
}

/**
 * District UI floating bottom navigation dock featuring:
 * - Frosted glassmorphism container with specular border
 * - Spring-physics animated sliding capsule indicator
 * - Dual-state crisp vector iconography with active micro-accents
 * - Tactile micro-interactions and knockout pill notification badges
 */
export function BottomNav({
  activeTab,
  onChange,
  pendingSyncCount = 0,
  unapprovedPeersCount = 0,
}: BottomNavProps) {
  const items: NavItem[] = [
    {
      id: "map",
      label: "Map",
      icon: (isActive: boolean) => (
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth={isActive ? "2.2" : "1.85"}
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
          className="district-icon"
        >
          <polygon
            points="3 6 9 3 15 6 21 3 21 18 15 21 9 18 3 21"
            fill={isActive ? "var(--accent-soft)" : "none"}
          />
          <line x1="9" y1="3" x2="9" y2="18" />
          <line x1="15" y1="6" x2="15" y2="21" />
          {isActive && (
            <circle cx="12" cy="11.5" r="1.8" fill="var(--accent)" stroke="none" />
          )}
        </svg>
      ),
    },
    {
      id: "incidents",
      label: "Incidents",
      badge: pendingSyncCount > 0 ? pendingSyncCount : undefined,
      icon: (isActive: boolean) => (
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth={isActive ? "2.2" : "1.85"}
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
          className="district-icon"
        >
          <path
            d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"
            fill={isActive ? "var(--accent-soft)" : "none"}
          />
          <polyline points="14 2 14 8 20 8" />
          <line x1="16" y1="13" x2="8" y2="13" />
          <line x1="16" y1="17" x2="8" y2="17" />
          <circle
            cx="10"
            cy="9.5"
            r="1.2"
            fill={isActive ? "var(--accent)" : "currentColor"}
            stroke="none"
          />
        </svg>
      ),
    },
    {
      id: "mesh",
      label: "Mesh",
      badge: unapprovedPeersCount > 0 ? unapprovedPeersCount : undefined,
      icon: (isActive: boolean) => (
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth={isActive ? "2.2" : "1.85"}
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
          className="district-icon"
        >
          <circle
            cx="12"
            cy="12"
            r="3"
            fill={isActive ? "var(--accent)" : "none"}
            stroke={isActive ? "none" : "currentColor"}
          />
          <path d="M16.24 7.76a6 6 0 0 1 0 8.49m-8.48-.01a6 6 0 0 1 0-8.49" />
          <path d="M19.07 4.93a10 10 0 0 1 0 14.14M4.93 4.93a10 10 0 0 0 0 14.14" />
        </svg>
      ),
    },
    {
      id: "intel",
      label: "AI Intel",
      icon: (isActive: boolean) => (
        <QwenIcon
          size={20}
          className="district-icon"
          style={{ opacity: isActive ? 1 : 0.72 }}
        />
      ),
    },
    {
      id: "node",
      label: "My Node",
      icon: (isActive: boolean) => (
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth={isActive ? "2.2" : "1.85"}
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
          className="district-icon"
        >
          <path
            d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2"
            fill={isActive ? "var(--accent-soft)" : "none"}
          />
          <circle
            cx="12"
            cy="7"
            r="4"
            fill={isActive ? "var(--accent)" : "none"}
            stroke={isActive ? "none" : "currentColor"}
          />
        </svg>
      ),
    },
  ];

  const activeIndex = items.findIndex((item) => item.id === activeTab);
  const safeIndex = activeIndex >= 0 ? activeIndex : 0;

  return (
    <nav
      className="district-bottom-dock ixigo-bottom-dock"
      aria-label="Tactical Navigation"
      role="navigation"
    >
      <div
        className="district-bottom-dock__bar ixigo-bottom-dock__bar"
        style={{ "--active-index": safeIndex } as React.CSSProperties}
        role="tablist"
      >
        {/* Animated sliding capsule indicator */}
        <div
          className="district-bottom-dock__indicator"
          aria-hidden="true"
        />

        {items.map((item) => {
          const isActive = activeTab === item.id;
          return (
            <button
              key={item.id}
              type="button"
              className={`district-bottom-dock__tab ixigo-bottom-dock__tab ${
                isActive ? "district-bottom-dock__tab--active ixigo-bottom-dock__tab--active" : ""
              }`}
              onClick={() => onChange(item.id)}
              aria-selected={isActive}
              role="tab"
              id={`tab-${item.id}`}
              aria-controls={`panel-${item.id}`}
            >
              <div className="district-bottom-dock__icon-box ixigo-bottom-dock__icon-box">
                {item.icon(isActive)}
                {item.badge !== undefined && item.badge > 0 && (
                  <span
                    className="district-bottom-dock__badge ixigo-bottom-dock__badge"
                    aria-label={`${item.badge} notifications`}
                  >
                    {item.badge > 99 ? "99+" : item.badge}
                  </span>
                )}
              </div>
              <span className="district-bottom-dock__label ixigo-bottom-dock__label">
                {item.label}
              </span>
              {isActive && (
                <span className="district-bottom-dock__dot" aria-hidden="true" />
              )}
            </button>
          );
        })}
      </div>
    </nav>
  );
}
