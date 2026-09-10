import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";

const STORAGE_KEY = "securemesh.id_encryption";

interface IdPrivacyContextValue {
  /** Whether IDs are encrypted by default globally */
  isEncrypted: boolean;
  /** Toggle global encryption state */
  toggleEncryption: () => void;
  /** Set global encryption state */
  setIsEncrypted: (encrypted: boolean) => void;
  /** Check if a specific ID has been individually revealed */
  isIdRevealed: (id: string) => boolean;
  /** Toggle reveal state for a specific ID */
  toggleRevealId: (id: string) => void;
}

const IdPrivacyContext = createContext<IdPrivacyContextValue | null>(null);

function readStoredSetting(): boolean {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored !== null) {
      return stored === "true";
    }
  } catch {
    // LocalStorage unavailable in restricted environment
  }
  // Default to true: "keep node ids and incident ids encrypted"
  return true;
}


/**
 * Generates masked security bullets to completely hide an ID in encrypted mode.
 */
export function encryptId(id: string, _lead = 4, _tail = 4, full = false): string {
  if (!id) return "••••••••";
  return full ? "••••••••••••••••" : "••••••••";
}

export function IdPrivacyProvider({ children }: { children: ReactNode }) {
  const [isEncrypted, setIsEncryptedState] = useState<boolean>(readStoredSetting);
  const [revealedIds, setRevealedIds] = useState<Set<string>>(() => new Set());

  // Keep localStorage updated with preference
  const setIsEncrypted = useCallback((encrypted: boolean) => {
    setIsEncryptedState(encrypted);
    try {
      localStorage.setItem(STORAGE_KEY, String(encrypted));
    } catch {
      // Storage error ignored
    }
  }, []);

  const toggleEncryption = useCallback(() => {
    setIsEncryptedState((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(STORAGE_KEY, String(next));
      } catch {
        // Storage error ignored
      }
      return next;
    });
  }, []);

  const isIdRevealed = useCallback(
    (id: string) => revealedIds.has(id),
    [revealedIds],
  );

  const toggleRevealId = useCallback((id: string) => {
    setRevealedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  const value = useMemo(
    () => ({
      isEncrypted,
      toggleEncryption,
      setIsEncrypted,
      isIdRevealed,
      toggleRevealId,
    }),
    [isEncrypted, toggleEncryption, setIsEncrypted, isIdRevealed, toggleRevealId],
  );

  return (
    <IdPrivacyContext.Provider value={value}>
      {children}
    </IdPrivacyContext.Provider>
  );
}

/**
 * Hook to access ID encryption & privacy shield controls.
 */
export function useIdPrivacy(): IdPrivacyContextValue {
  const ctx = useContext(IdPrivacyContext);
  if (!ctx) {
    // Fallback if rendered outside provider: default to encrypted
    return {
      isEncrypted: true,
      toggleEncryption: () => {},
      setIsEncrypted: () => {},
      isIdRevealed: () => false,
      toggleRevealId: () => {},
    };
  }
  return ctx;
}
