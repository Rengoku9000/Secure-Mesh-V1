import type { ReactNode } from "react";

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
  icon: ReactNode;
}

/**
 * ixigo-inspired elevated bottom navigation dock with active capsule indicator
 * and tactile tap areas.
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
      icon: (
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <polygon points="3 6 9 3 15 6 21 3 21 18 15 21 9 18 3 21" />
          <line x1="9" y1="3" x2="9" y2="18" />
          <line x1="15" y1="6" x2="15" y2="21" />
        </svg>
      ),
    },
    {
      id: "incidents",
      label: "Incidents",
      badge: pendingSyncCount > 0 ? pendingSyncCount : undefined,
      icon: (
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <polyline points="14 2 14 8 20 8" />
          <line x1="16" y1="13" x2="8" y2="13" />
          <line x1="16" y1="17" x2="8" y2="17" />
          <polyline points="10 9 9 9 8 9" />
        </svg>
      ),
    },
    {
      id: "mesh",
      label: "Mesh",
      badge: unapprovedPeersCount > 0 ? unapprovedPeersCount : undefined,
      icon: (
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <circle cx="12" cy="12" r="3" />
          <path d="M16.24 7.76a6 6 0 0 1 0 8.49m-8.48-.01a6 6 0 0 1 0-8.49m11.31-2.82a10 10 0 0 1 0 14.14m-14.14 0a10 10 0 0 1 0-14.14" />
        </svg>
      ),
    },
    {
      id: "intel",
      label: "AI Intel",
      icon: (
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="M12 2v4M12 18v4M4.93 4.93l2.83 2.83M16.24 16.24l2.83 2.83M2 12h4M18 12h4M4.93 19.07l2.83-2.83M16.24 7.76l2.83-2.83" />
        </svg>
      ),
    },
    {
      id: "node",
      label: "My Node",
      icon: (
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
          <circle cx="12" cy="7" r="4" />
        </svg>
      ),
    },
  ];

  return (
    <nav className="ixigo-bottom-dock" aria-label="Tactical Navigation">
      <div className="ixigo-bottom-dock__bar">
        {items.map((item) => {
          const isActive = activeTab === item.id;
          return (
            <button
              key={item.id}
              type="button"
              className={`ixigo-bottom-dock__tab ${isActive ? "ixigo-bottom-dock__tab--active" : ""}`}
              onClick={() => onChange(item.id)}
              aria-selected={isActive}
              role="tab"
            >
              <div className="ixigo-bottom-dock__icon-box">
                {item.icon}
                {item.badge !== undefined && item.badge > 0 && (
                  <span className="ixigo-bottom-dock__badge" aria-label={`${item.badge} notifications`}>
                    {item.badge > 99 ? "99+" : item.badge}
                  </span>
                )}
              </div>
              <span className="ixigo-bottom-dock__label">{item.label}</span>
            </button>
          );
        })}
      </div>
    </nav>
  );
}
