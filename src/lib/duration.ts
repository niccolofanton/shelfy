// Compact human-readable durations for the AI analysis activity UI (sidebar strip
// + AI Tags queue). Returns null for missing/invalid input so callers can hide
// the field entirely instead of rendering a placeholder.
export function formatDuration(ms: number | null | undefined): string | null {
  if (ms == null || !isFinite(ms) || ms < 0) return null;
  const totalSec = Math.round(ms / 1000);
  if (totalSec < 60) return `${totalSec}s`;
  const min = Math.floor(totalSec / 60);
  const sec = totalSec % 60;
  if (min < 60) return sec ? `${min}m ${sec}s` : `${min}m`;
  const hours = Math.floor(min / 60);
  const remMin = min % 60;
  return remMin ? `${hours}h ${remMin}m` : `${hours}h`;
}

// ETA reading with a leading "≈" so it reads as an estimate, not a countdown.
export function formatEta(ms: number | null | undefined): string | null {
  const d = formatDuration(ms);
  return d ? `≈ ${d}` : null;
}
