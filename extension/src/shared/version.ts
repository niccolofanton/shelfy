// The extension's version (manifest `version`, `X-Shelfy-Extension`, C2 `version`, C5
// `client.ext`) and the comparison against the server's `minVersion` (C1, C3).

export const EXTENSION_VERSION = '0.2.0';

interface ParsedVersion {
  core: number[];
  prerelease: boolean;
}

function parseVersion(value: string): ParsedVersion | null {
  const match =
    /^(\d{1,9})(?:\.(\d{1,9}))?(?:\.(\d{1,9}))?(-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.exec(
      value.trim(),
    );
  if (!match) return null;
  return {
    core: [match[1], match[2], match[3]].map((part) => Number(part ?? 0)),
    prerelease: match[4] !== undefined,
  };
}

/**
 * Compares two versions on major.minor.patch; a pre-release sorts before its release.
 * Returns a negative number, 0 or a positive number; null when either is not a version.
 */
export function compareVersions(a: string, b: string): number | null {
  const left = parseVersion(a);
  const right = parseVersion(b);
  if (!left || !right) return null;
  for (let i = 0; i < 3; i++) {
    const diff = left.core[i] - right.core[i];
    if (diff !== 0) return diff;
  }
  if (left.prerelease === right.prerelease) return 0;
  return left.prerelease ? -1 : 1;
}

/** True when `version` is below `minVersion`. An unreadable minVersion never blocks. */
export function isBelowMinVersion(version: string, minVersion: string | null | undefined): boolean {
  if (!minVersion) return false;
  const order = compareVersions(version, minVersion);
  return order !== null && order < 0;
}
