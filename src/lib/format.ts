/**
 * Presentation helpers.
 *
 * Formatting lives here rather than inside components so the same value is
 * rendered identically everywhere it appears.
 */

/**
 * Formats an RFC 3339 timestamp from the core for display.
 *
 * The core stores UTC; operators read local time, so the conversion happens
 * at the last possible moment — here.
 */
export function formatTimestamp(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) {
    return "unknown";
  }
  return date.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/** A compact "3 min ago" style label for the incident timeline. */
export function formatRelative(iso: string, now: number = Date.now()): string {
  const timestamp = new Date(iso).getTime();
  if (Number.isNaN(timestamp)) {
    return "unknown";
  }

  const seconds = Math.round((now - timestamp) / 1000);
  if (seconds < 5) return "just now";
  if (seconds < 60) return `${seconds}s ago`;

  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;

  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;

  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d ago`;

  return formatTimestamp(iso);
}

/**
 * Shortens a long hex identifier for display while keeping it recognisable.
 * The full value is always available in the element's title attribute.
 */
export function shortenId(id: string, lead = 8, tail = 6): string {
  if (id.length <= lead + tail + 1) {
    return id;
  }
  return `${id.slice(0, lead)}…${id.slice(-tail)}`;
}

/** Renders a coordinate pair, or an explicit absence. */
export function formatLocation(
  latitude: number | null,
  longitude: number | null,
): string {
  if (latitude === null || longitude === null) {
    return "No location";
  }
  // Six places is roughly a tenth of a metre — finer than any source here
  // resolves, but it is what the coordinates were recorded as, and rounding a
  // record for display invites someone to read the rounding as the precision.
  return `${latitude.toFixed(6)}, ${longitude.toFixed(6)}`;
}

/**
 * Renders an accuracy radius, or says plainly that none was recorded.
 *
 * "unavailable" rather than a blank or a zero: a missing figure means nobody
 * measured, which is different from a measurement of nothing.
 */
export function formatAccuracy(meters: number | null): string {
  if (meters === null) {
    return "unavailable";
  }
  return meters < 10 ? `±${meters.toFixed(1)} m` : `±${Math.round(meters)} m`;
}

/**
 * How long ago something happened, in words.
 *
 * Coarse on purpose: "4 min ago" is what an operator needs from a peer's last
 * heartbeat, and a second-by-second countdown would imply a precision the
 * five-minute interval does not have.
 */
export function formatAge(seconds: number): string {
  if (seconds < 60) {
    return "just now";
  }
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes} min ago`;
  }
  const hours = Math.floor(minutes / 60);
  return hours < 24 ? `${hours} h ago` : `${Math.floor(hours / 24)} d ago`;
}
