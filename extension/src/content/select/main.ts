interface SelectMainApi {
  setLabels(labels: { saved: string; open: string; disabled: string }): void;
  enable(): void;
  disable(): void;
  markSaved(value: unknown): void;
  clearSelection(): void;
  retryCheck(keys: string[]): void;
  collectEntriesJSON(): string;
  status(): { enabled: boolean; count: number };
}

// Self-contained functions serialized by chrome.scripting into the MAIN world.
export function selectMain(action: string, value: unknown = null): unknown {
  const api = (window as unknown as { __ssSelect?: SelectMainApi }).__ssSelect;
  if (!api) return action === 'status' ? { enabled: false, count: 0 } : null;
  if (action === 'enable') {
    if (value && typeof value === 'object')
      api.setLabels(value as Parameters<typeof api.setLabels>[0]);
    api.enable();
  } else if (action === 'disable') api.disable();
  else if (action === 'mark') api.markSaved(value);
  else if (action === 'clear') api.clearSelection();
  else if (action === 'retry') api.retryCheck(value as string[]);
  else if (action === 'collect') return { json: api.collectEntriesJSON(), href: location.href };
  return api.status();
}
