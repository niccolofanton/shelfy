// State belongs to the currently paired account. C2 exposes the token's stable
// id, not a user id; re-pairing or forgetting it starts a fresh account session.
export const ACCOUNT_STATE_KEYS = [
  'shelfy.instagramBacklogSyncDay',
  'shelfy.planner',
  'shelfy.syncs',
] as const;
