// Capture event/stage/failure codes (P4 lane rule 10): the capture pipeline emits
// stable snake_case codes with scalar params, never UI prose, so the same events
// work on the desktop (Italian) and on the web (either locale). The strings live
// in shared/capture/codes.json, the contract shared with the SPA.
//
// This module is the desktop-side renderer: weborchestrator.ts turns a streamed
// CaptureEvent into the Italian narration the Websites panel already shows.

import CODES from '../../shared/capture/codes.json';

export type CaptureParam = string | number | boolean | null;
export type CaptureParams = Record<string, CaptureParam>;

type LocaleStrings = { it: string; en: string };
const EVENTS = CODES.events as Record<string, { kind: string } & LocaleStrings>;
const STAGES = CODES.stages as Record<string, LocaleStrings>;
const FAILURES = CODES.failures as Record<string, LocaleStrings>;

function interpolate(template: string, params?: CaptureParams): string {
  return template.replace(/\{(\w+)\}/g, (_, key: string) => {
    const v = params?.[key];
    return v === undefined || v === null ? '' : String(v);
  });
}

function pick(entry: LocaleStrings | undefined, locale: string): string | null {
  if (!entry) return null;
  return locale.startsWith('it') ? entry.it : entry.en;
}

// A capture event code + its params → a human string in `locale` (default it).
// Unknown codes fall back to the code itself, so a new service code never crashes
// an older renderer.
export function formatCaptureEvent(code: string, params?: CaptureParams, locale = 'it'): string {
  const tpl = pick(EVENTS[code], locale);
  return tpl ? interpolate(tpl, params) : code;
}

export function formatStage(code: string, locale = 'it'): string {
  return pick(STAGES[code], locale) || code;
}

export function formatFailure(code: string, locale = 'it'): string {
  return pick(FAILURES[code], locale) || code;
}
