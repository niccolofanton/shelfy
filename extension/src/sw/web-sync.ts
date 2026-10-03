import type {
  PlannerSnapshot,
  SyncBinding,
  SyncTarget,
  Platform,
  ExternalAnswer,
} from '../shared/protocol';
import type { Pairing } from './settings';
import type { PlannerService } from './planner/service';

/** C9 carries identities only. The worker freezes its private credential and
 * the planner revalidates that credential across every asynchronous boundary. */
export function createWebSyncControls(
  pairing: () => Promise<Pairing | null>,
  planner: Pick<PlannerService, 'start' | 'stop' | 'snapshot' | 'syncing'>,
) {
  const matches = (value: Pairing | null, binding: SyncBinding): value is Pairing =>
    !!value?.accountId &&
    value.accountId === binding.expectedAccountId &&
    value.tokenId === binding.expectedTokenId;
  const same = (a: Pairing | null, b: Pairing | null) =>
    !!a && !!b && a.accountId === b.accountId && a.tokenId === b.tokenId && a.token === b.token;
  return {
    async start(target: SyncTarget, binding: SyncBinding): Promise<ExternalAnswer> {
      const frozen = await pairing();
      if (!frozen) return { ok: false, code: 'not_paired' };
      if (!matches(frozen, binding)) return { ok: false, code: 'account_mismatch' };
      return planner.start(target, { expectedPairing: frozen });
    },
    async stop(platform: Platform, binding: SyncBinding): Promise<ExternalAnswer> {
      const frozen = await pairing();
      if (!frozen) return { ok: false, code: 'not_paired' };
      if (!matches(frozen, binding)) return { ok: false, code: 'account_mismatch' };
      return planner.stop(platform, frozen);
    },
    async connection() {
      const frozen = await pairing();
      const empty = {
        accountId: null,
        tokenId: null,
        paired: false,
        syncing: { instagram: false, twitter: false, pinterest: false },
        planner: [] as PlannerSnapshot[],
      };
      if (!frozen?.accountId) return empty;
      const [snapshot, syncing] = await Promise.all([
        planner.snapshot(),
        planner.syncing(frozen.tokenId),
      ]);
      if (!same(frozen, await pairing())) return empty;
      return {
        accountId: frozen.accountId,
        tokenId: frozen.tokenId,
        paired: true,
        syncing,
        planner: snapshot.jobs
          .filter((job) => job.accountKey === frozen.tokenId)
          .map(({ platform, status, step, total, code, startedAt }) => ({
            platform,
            status,
            step,
            total,
            code,
            startedAt,
          })),
      };
    },
  };
}
