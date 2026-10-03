import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { useShelfy } from '../api/ShelfyProvider';
import {
  isSyncPlatform,
  SYNC_PLATFORMS,
  type SyncConnection,
  type SyncPlatform,
  type SyncProgress,
  type SyncRun,
  type SyncTarget,
} from '../api/sync';
import SyncHelp from '../components/sync/SyncHelp';

export const isInteractiveSync = (run: Pick<SyncRun, 'trigger'>) =>
  !['passive', 'refresh'].includes(run.trigger);
export function isMobileSyncDevice(): boolean {
  return (
    /Android|iPhone|iPad|iPod/i.test(navigator.userAgent) ||
    (navigator.maxTouchPoints > 0 && window.matchMedia('(pointer: coarse)').matches)
  );
}
interface Plan {
  total: number;
  ids: string[];
}
function useSyncState() {
  const client = useShelfy();
  const api = client.capabilities.sync ? client.sync : undefined;
  const [runs, setRuns] = useState<SyncRun[]>([]);
  const [connection, setConnection] = useState<SyncConnection>({
    extension: { state: 'missing' },
    syncing: {},
  });
  const [pending, setPending] = useState<Partial<Record<SyncPlatform, boolean>>>({});
  const [busy, setBusy] = useState<Set<SyncPlatform>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [help, setHelp] = useState<string | null>(null);
  const plans = useRef<Partial<Record<SyncPlatform, Plan>>>({});
  const actionLocks = useRef(new Set<SyncPlatform>());
  const epoch = useRef(0);
  const connectionGeneration = useRef(0);
  const refreshRef = useRef<() => void>(() => {});

  useEffect(() => {
    const current = ++epoch.current;
    const live = () => epoch.current === current;
    const abort = new AbortController();
    let request = 0;
    const received = new Map<string, SyncRun>();
    const receivedAt = new Map<string, number>();
    let progressSerial = 0;
    setConnection({ extension: { state: 'missing' }, syncing: {} });
    setRuns([]);
    setPending({});
    setBusy(new Set());
    setError(null);
    setHelp(null);
    plans.current = {};
    actionLocks.current.clear();
    if (!api)
      return () => {
        epoch.current = current + 1;
      };
    const refresh = () => {
      const mine = ++request;
      const serialAtStart = progressSerial;
      const latestFor = async (platform: SyncPlatform) => {
        let cursor: string | undefined;
        const seen = new Set<string>();
        do {
          const page = await api.list({ platform, limit: 100, cursor, signal: abort.signal });
          const latest = page.items.find(isInteractiveSync);
          if (latest) return latest;
          cursor = page.nextCursor ?? undefined;
          if (cursor && seen.has(cursor)) return undefined;
          if (cursor) seen.add(cursor);
        } while (cursor && live() && mine === request);
        return undefined;
      };
      void Promise.all([
        Promise.all(SYNC_PLATFORMS.map(latestFor)),
        api.list({ state: 'running', limit: 100, signal: abort.signal }),
      ])
        .then(([recent, active]) => {
          if (!live() || mine !== request) return;
          const all = new Map(
            [...recent.filter((run): run is SyncRun => !!run), ...active.items]
              .filter((run) => isSyncPlatform(run.platform))
              .map((run) => [run.id, run]),
          );
          const activeIds = new Set(active.items.map((run) => run.id));
          for (const [id, event] of received) {
            if (
              event.state === 'running' &&
              (receivedAt.get(id) ?? 0) <= serialAtStart &&
              !activeIds.has(id)
            ) {
              received.delete(id);
              receivedAt.delete(id);
              continue;
            }
            const stored = all.get(id);
            all.set(id, {
              ...event,
              ...stored,
              state:
                stored?.state !== undefined && stored.state !== 'running'
                  ? stored.state
                  : event.state,
              scanned: Math.max(stored?.scanned ?? 0, event.scanned),
              inserted: Math.max(stored?.inserted ?? 0, event.inserted),
              known: Math.max(stored?.known ?? 0, event.known),
            });
          }
          setRuns([...all.values()].sort((a, b) => b.startedAt - a.startedAt));
        })
        .catch(() => {
          if (live() && mine === request) setError('load_failed');
        });
    };
    refreshRef.current = refresh;
    const check = () => {
      const generation = connectionGeneration.current;
      void api
        .connection()
        .then((status) => {
          if (!live() || generation !== connectionGeneration.current) return;
          setConnection(status);
          setPending((previous) => {
            const next = { ...previous };
            for (const platform of SYNC_PLATFORMS)
              if (!actionLocks.current.has(platform) && !status.syncing[platform])
                delete next[platform];
            return next;
          });
        })
        .catch(() => {
          if (live()) setError('unreachable');
        });
    };
    const progress = (event: SyncProgress) => {
      if (!live()) return;
      const next: SyncRun = {
        ...event,
        id: event.runId,
        startedAt: Date.now(),
        finishedAt: event.state === 'running' ? null : Date.now(),
        errorCode: null,
        collectionId: null,
      };
      const plan = plans.current[event.platform];
      if (plan && isInteractiveSync(next) && !plan.ids.includes(event.runId))
        plan.ids.push(event.runId);
      received.set(event.runId, next);
      receivedAt.set(event.runId, ++progressSerial);
      if (received.size > 200) {
        const oldest = received.keys().next().value!;
        received.delete(oldest);
        receivedAt.delete(oldest);
      }
      setRuns((previous) => {
        const old = previous.find((run) => run.id === event.runId);
        if (old && old.state !== 'running' && event.state === 'running') return previous;
        const run = {
          ...next,
          scanned: Math.max(old?.scanned ?? 0, next.scanned),
          inserted: Math.max(old?.inserted ?? 0, next.inserted),
          known: Math.max(old?.known ?? 0, next.known),
          startedAt: old?.startedAt ?? next.startedAt,
          errorCode: old?.errorCode ?? null,
          collectionId: old?.collectionId ?? null,
        };
        return [run, ...previous.filter((other) => other.id !== run.id)].slice(0, 200);
      });
      if (event.state !== 'running') refresh(); // C8 has no errorCode: hydrate terminal state from C4.
    };
    const stopProgress = api.onProgress(progress);
    const stopRefresh = api.onRefresh(() => {
      refresh();
      check();
    });
    refresh();
    check();
    const timer = setInterval(check, 10_000); // C9 retains planner state between its C4 steps.
    return () => {
      epoch.current = current + 1;
      abort.abort();
      clearInterval(timer);
      stopProgress();
      stopRefresh();
      refreshRef.current = () => {};
    };
  }, [api]);

  const active = useMemo(
    () =>
      Object.fromEntries(
        SYNC_PLATFORMS.map((platform) => [
          platform,
          !!pending[platform] ||
            !!connection.syncing[platform] ||
            runs.some(
              (run) =>
                run.platform === platform && run.state === 'running' && isInteractiveSync(run),
            ),
        ]),
      ) as Record<SyncPlatform, boolean>,
    [runs, pending, connection],
  );
  const latest = useMemo(
    () =>
      Object.fromEntries(
        SYNC_PLATFORMS.map((platform) => [
          platform,
          runs
            .filter((run) => run.platform === platform && isInteractiveSync(run))
            .sort((a, b) => b.startedAt - a.startedAt)[0],
        ]),
      ) as Partial<Record<SyncPlatform, SyncRun>>,
    [runs],
  );
  const begin = (platform: SyncPlatform) => {
    connectionGeneration.current++;
    actionLocks.current.add(platform);
    setBusy(new Set(actionLocks.current));
    setError(null);
  };
  const end = (platform: SyncPlatform) => {
    connectionGeneration.current++;
    actionLocks.current.delete(platform);
    setBusy(new Set(actionLocks.current));
  };
  const start = async (target: SyncTarget, steps?: number) => {
    if (!api || actionLocks.current.has(target.platform)) return;
    if (isMobileSyncDevice()) {
      setHelp('mobile');
      return;
    }
    const current = epoch.current;
    begin(target.platform);
    try {
      const status = await api.connection();
      if (epoch.current !== current) return;
      setConnection(status);
      const ext = status.extension;
      if (ext.state !== 'ready' || !ext.paired || ext.outdated) {
        setHelp(
          ext.state !== 'ready'
            ? ext.state
            : ext.outdated
              ? 'outdated'
              : (status.code ?? 'not_paired'),
        );
        return;
      }
      if (!status.accountId || !status.tokenId) {
        setHelp('account_mismatch');
        return;
      }
      const binding = { expectedAccountId: status.accountId, expectedTokenId: status.tokenId };
      if (steps === undefined) {
        const collections = await client.listCollections();
        if (epoch.current !== current) return;
        const folders = collections.filter(
          (collection) => collection.platform === target.platform && collection.externalId != null,
        ).length;
        steps =
          target.collectionId != null
            ? 1
            : target.platform === 'instagram'
              ? 1 + folders
              : target.platform === 'pinterest'
                ? Math.max(1, folders)
                : 1;
      }
      plans.current[target.platform] = { total: steps, ids: [] };
      setPending((old) => ({ ...old, [target.platform]: true }));
      const answer = await api.start(target, binding);
      if (epoch.current !== current) return;
      if (!answer.ok) {
        setError(answer.code);
        setHelp(answer.code);
        setPending((old) => ({ ...old, [target.platform]: false }));
        delete plans.current[target.platform];
      }
      refreshRef.current();
    } catch {
      if (epoch.current === current) {
        setError('unreachable');
        setHelp('unreachable');
        setPending((old) => ({ ...old, [target.platform]: false }));
      }
    } finally {
      if (epoch.current === current) end(target.platform);
    }
  };
  const stop = async (platform: SyncPlatform) => {
    if (!api || actionLocks.current.has(platform)) return;
    const current = epoch.current;
    begin(platform);
    try {
      const status = await api.connection();
      if (epoch.current !== current) return;
      setConnection(status);
      if (
        status.extension.state !== 'ready' ||
        !status.extension.paired ||
        !status.accountId ||
        !status.tokenId
      ) {
        setError(status.code ?? 'not_paired');
        setHelp(status.code ?? 'not_paired');
        return;
      }
      const answer = await api.stop(platform, {
        expectedAccountId: status.accountId,
        expectedTokenId: status.tokenId,
      });
      if (epoch.current !== current) return;
      if (!answer.ok) setError(answer.code);
      else {
        setPending((old) => ({ ...old, [platform]: false }));
        setConnection((old) => ({ ...old, syncing: { ...old.syncing, [platform]: false } }));
      }
      refreshRef.current();
    } catch {
      if (epoch.current === current) setError('unreachable');
    } finally {
      if (epoch.current === current) end(platform);
    }
  };
  const step = (run: SyncRun) => {
    const job = connection.planner?.find(
      (job) =>
        job.platform === run.platform &&
        job.startedAt <= run.startedAt &&
        ['navigating', 'syncing'].includes(job.status),
    );
    if (job) return { index: Math.max(1, job.step), total: job.total };
    const plan = plans.current[run.platform];
    return plan ? { index: Math.max(1, plan.ids.indexOf(run.id) + 1), total: plan.total } : null;
  };
  return {
    enabled: !!api,
    runs,
    active,
    latest,
    busy,
    error,
    connection,
    planner: connection.planner ?? [],
    start,
    stop,
    step,
    help,
    closeHelp: () => setHelp(null),
    refresh: () => refreshRef.current(),
  };
}

type SyncState = ReturnType<typeof useSyncState>;
const Context = createContext<SyncState | null>(null);
export function WebSyncProvider({ children }: { children: ReactNode }) {
  const sync = useSyncState();
  return (
    <Context.Provider value={sync}>
      {children}
      {sync.help && <SyncHelp code={sync.help} onClose={sync.closeHelp} />}
    </Context.Provider>
  );
}
export function useWebSync(): SyncState | null {
  return useContext(Context);
}
