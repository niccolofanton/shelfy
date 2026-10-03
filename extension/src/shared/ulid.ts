// ULIDs (26 characters, Crockford base32: 48-bit millisecond time + 80 random bits), used as
// ingest batch ids (`Idempotency-Key`, C5) and as local ids of queued batches and runs. They sort
// by creation time; ids made in the same millisecond increase monotonically.

const ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
const TIME_LEN = 10;
const RANDOM_LEN = 16;
const MAX_TIME = 2 ** 48 - 1;
const RANDOM_BITS = 80n;
const RANDOM_MAX = (1n << RANDOM_BITS) - 1n;

export type RandomBytes = (length: number) => Uint8Array;

const cryptoRandom: RandomBytes = (length) => crypto.getRandomValues(new Uint8Array(length));

function encodeTime(time: number): string {
  let value = time;
  let out = '';
  for (let i = 0; i < TIME_LEN; i++) {
    out = ALPHABET[value % 32] + out;
    value = Math.floor(value / 32);
  }
  return out;
}

function encodeRandom(value: bigint): string {
  let rest = value;
  let out = '';
  for (let i = 0; i < RANDOM_LEN; i++) {
    out = ALPHABET[Number(rest & 31n)] + out;
    rest >>= 5n;
  }
  return out;
}

function randomValue(random: RandomBytes): bigint {
  let value = 0n;
  for (const byte of random(10)) value = (value << 8n) | BigInt(byte);
  return value;
}

/** A ULID generator; `now` and `random` are injectable for tests. */
export function createUlid(
  now: () => number = Date.now,
  random: RandomBytes = cryptoRandom,
): () => string {
  let lastTime = -1;
  let lastRandom = 0n;
  return () => {
    let time = Math.min(Math.max(0, Math.floor(now())), MAX_TIME);
    if (time <= lastTime) {
      // Same (or an earlier, skewed) millisecond: keep the previous time and count up.
      time = lastTime;
      lastRandom = lastRandom === RANDOM_MAX ? 0n : lastRandom + 1n;
      if (lastRandom === 0n) time = lastTime = Math.min(lastTime + 1, MAX_TIME);
    } else {
      lastTime = time;
      lastRandom = randomValue(random);
    }
    return encodeTime(time) + encodeRandom(lastRandom);
  };
}

export const ulid = createUlid();

const ULID_PATTERN = /^[0-9A-HJKMNP-TV-Z]{26}$/;

export function isUlid(value: unknown): value is string {
  return typeof value === 'string' && ULID_PATTERN.test(value);
}

/** The millisecond time a ULID encodes. */
export function ulidTime(id: string): number {
  let time = 0;
  for (const char of id.slice(0, TIME_LEN)) time = time * 32 + ALPHABET.indexOf(char);
  return time;
}
