// Label lookups for the Jobs view (P4-09): kind, stage and error codes become
// i18n strings from `src/i18n/messages/jobs.ts` when one is authored, and a
// prettified version of the raw code otherwise — "unknown kinds … get a
// generic label" (the card's acceptance). General on purpose (EXECUTION.md
// L14): P2-08's Activity center reuses these for the same codes.
import { translate } from '../../i18n';

// "archive.drain" → "Archive Drain"; "lease_expired" → "Lease Expired". Never
// worse than the raw code, so an unlabeled kind or error is still readable.
function fallbackLabel(raw: string): string {
  const spaced = raw.replace(/[._-]+/g, ' ').trim();
  if (!spaced) return raw;
  return spaced.replace(/\p{L}+/gu, (word) => word[0].toUpperCase() + word.slice(1));
}

// `key` is the fully-qualified i18n key (e.g. "jobs.kind.archive.drain");
// `translate` returns it verbatim when no language has it (src/i18n/index.tsx).
function lookupOr(lang: string, key: string, raw: string): string {
  const value = translate(lang, key);
  return value === key ? fallbackLabel(raw) : value;
}

export function jobKindLabel(lang: string, kind: string): string {
  return lookupOr(lang, `jobs.kind.${kind}`, kind);
}

export function jobStageLabel(lang: string, stage: string): string {
  return lookupOr(lang, `jobs.stage.${stage}`, stage);
}

export function jobErrorLabel(lang: string, code: string): string {
  return lookupOr(lang, `jobs.error.${code}`, code);
}

export function jobStateLabel(lang: string, state: string): string {
  return lookupOr(lang, `jobs.state.${state}`, state);
}
