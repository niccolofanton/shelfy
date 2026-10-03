import { isRecord, type CaptureSource, type InterceptMessage } from '../../shared/protocol';

/** One worker-authorized refresh at a time; page scope messages alone never
 * authorize ingest. The worker sanitizes the returned item again. */
export class RefreshCollector {
  private current: {
    requestId: string;
    nativeId: string;
    items: unknown[];
    wake: (() => void) | null;
  } | null = null;
  prepare(requestId: string, nativeId: string): boolean {
    if (this.current) return false;
    this.current = { requestId, nativeId, items: [], wake: null };
    return true;
  }
  consume(message: InterceptMessage, capture: CaptureSource): boolean {
    if (capture !== 'refresh') return false;
    const current = this.current;
    if (current && message.platform === 'instagram') {
      current.items = message.items
        .filter(
          (item) =>
            isRecord(item) &&
            typeof item.id === 'string' &&
            item.id.split('_')[0] === current.nativeId,
        )
        .slice(0, 1);
      if (current.items.length) current.wake?.();
    }
    return true;
  }
  async take(requestId: string, discard: boolean): Promise<unknown[]> {
    const current = this.current;
    if (!current || current.requestId !== requestId) return [];
    if (!discard && !current.items.length) {
      await new Promise<void>((resolve) => {
        const timer = setTimeout(resolve, 1500);
        current.wake = () => {
          clearTimeout(timer);
          resolve();
        };
      });
    }
    if (this.current === current) this.current = null;
    return discard ? [] : current.items;
  }
}
