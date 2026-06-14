// Disclaimer acceptance — tracked, persisted gate state.
//
// We persist the user's acknowledgement of the legal disclaimer (see
// DISCLAIMER.md) in localStorage, mirroring the app's existing settings
// pattern (see useDownloadPrefs / PostModal video pref). Bumping
// DISCLAIMER_VERSION re-prompts everyone on next launch — do this only when the
// legal terms materially change.

export const DISCLAIMER_VERSION = '2026-06-07';
const STORAGE_KEY = 'app:disclaimerAcceptance';

// The persisted acceptance record.
export interface DisclaimerAcceptance {
  version: string;
  acceptedAt: string; // ISO 8601
  dontShowAgain: boolean;
}

// Returns the stored acceptance record, or null if absent/invalid.
// Shape: { version: string, acceptedAt: string (ISO), dontShowAgain: boolean }
export function getDisclaimerAcceptance(): DisclaimerAcceptance | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const rec = JSON.parse(raw) as unknown;
    if (
      rec &&
      typeof rec === 'object' &&
      typeof (rec as { version?: unknown }).version === 'string'
    ) {
      return rec as DisclaimerAcceptance;
    }
    return null;
  } catch {
    return null;
  }
}

// True only when the CURRENT disclaimer version has been accepted at least once.
export function isDisclaimerAccepted(): boolean {
  const rec = getDisclaimerAcceptance();
  return !!rec && rec.version === DISCLAIMER_VERSION;
}

// Whether the blocking gate should be shown at startup. It shows when the
// current version was never accepted, OR when it was accepted without ticking
// "don't show again" (so the notice keeps appearing as a reminder until the
// user opts out).
export function shouldShowDisclaimerGate(): boolean {
  const rec = getDisclaimerAcceptance();
  if (!rec || rec.version !== DISCLAIMER_VERSION) return true;
  return rec.dontShowAgain !== true;
}

// Persist acceptance of the current version with a timestamp and the
// "don't show again" preference; returns the record.
export function acceptDisclaimer(dontShowAgain = false): DisclaimerAcceptance {
  const rec: DisclaimerAcceptance = {
    version: DISCLAIMER_VERSION,
    acceptedAt: new Date().toISOString(),
    dontShowAgain: !!dontShowAgain,
  };
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(rec));
  } catch {
    /* storage unavailable — gate will simply re-prompt next launch */
  }
  return rec;
}
