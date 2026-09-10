import { useIdPrivacy } from "../lib/idPrivacy";

export function IdPrivacyToggle() {
  const { isEncrypted, toggleEncryption } = useIdPrivacy();

  return (
    <button
      type="button"
      className={`ixigo-header__shield-pill ${
        isEncrypted
          ? "ixigo-header__shield-pill--encrypted"
          : "ixigo-header__shield-pill--visible"
      }`}
      onClick={toggleEncryption}
      title={
        isEncrypted
          ? "Operational ID Privacy Shield is ACTIVE. Incident IDs and Node IDs are encrypted. Click to make all visible."
          : "IDs are currently visible in plain text. Click to encrypt all IDs."
      }
      aria-label={
        isEncrypted ? "IDs are encrypted. Click to show." : "IDs are visible. Click to encrypt."
      }
    >
      {isEncrypted ? (
        <svg
          className="ixigo-header__shield-icon"
          width="13"
          height="13"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.4"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
          <path d="M7 11V7a5 5 0 0 1 10 0v4" />
        </svg>
      ) : (
        <svg
          className="ixigo-header__shield-icon"
          width="13"
          height="13"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.4"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
          <circle cx="12" cy="12" r="3" />
        </svg>
      )}
      <span className="ixigo-header__shield-label">
        {isEncrypted ? "IDs Encrypted" : "IDs Visible"}
      </span>
    </button>
  );
}
