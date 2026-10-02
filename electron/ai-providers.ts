// OpenAI-compatible chat providers used by image search. The renderer sees only
// public metadata; credentials are resolved in the main process for each call.
import { app } from 'electron';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { execFileSync } from 'child_process';

export interface SearchProviderInfo {
  id: string;
  name: string;
  selected: boolean;
  vision?: boolean;
}

export interface ProviderConfig {
  id: string;
  name: string;
  baseUrl: string;
  model: string;
  apiKey?: string;
  vision: boolean;
}

export interface EditableProvider {
  id: string;
  name: string;
  baseUrl: string;
  model: string;
  apiKeyEnv?: string;
  apiKeyPiProvider?: string;
  vision: boolean;
}

export interface ProviderSettingsView {
  searchProvider: string;
  visionProvider: string;
  providers: Array<EditableProvider & { secretConfigured: boolean; available: boolean }>;
}

export interface ProviderSettingsInput {
  searchProvider: string;
  visionProvider: string;
  providers: EditableProvider[];
  secrets?: Record<string, string>;
}

interface PiSettings {
  defaultProvider?: string;
  defaultModel?: string;
  llamaSettings?: {
    servers?: Array<{ id?: string; name?: string; url?: string }>;
  };
}

interface SavedSettings {
  searchProvider?: string;
}

function readJson(file: string): unknown {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8')) as unknown;
  } catch {
    return null;
  }
}

function allowedSecretName(name: string): boolean {
  return name === 'ORNITH_API_KEY' || /^SHELFY_AI_[A-Z0-9_]+$/.test(name);
}

function resolveEnvSecret(name: string): string | undefined {
  if (!allowedSecretName(name)) return undefined;
  if (process.platform === 'darwin') {
    try {
      // Prefer the editable Keychain item; the environment is a fallback when
      // the item has not been configured or is unavailable.
      const key = execFileSync(
        '/usr/bin/security',
        ['find-generic-password', '-a', os.userInfo().username, '-s', name, '-w'],
        {
          encoding: 'utf8',
          timeout: 1500,
          stdio: ['ignore', 'pipe', 'ignore'],
        },
      ).trim();
      if (key) return key;
    } catch {
      // An environment-only setup remains usable without a Keychain item.
    }
  }
  return process.env[name] || undefined;
}

function settingsPath(): string {
  return path.join(app.getPath('userData'), 'ai-providers.json');
}

function selectedId(): string {
  const saved = readJson(settingsPath()) as SavedSettings | null;
  return typeof saved?.searchProvider === 'string' ? saved.searchProvider : 'local';
}

// GUI apps launched from Finder/Explorer do not inherit the shell PATH, so the
// bare `tailscale` command is usually not found there: try the known install
// locations too (macOS app bundle, Homebrew, Windows, Linux).
const TAILSCALE_CANDIDATES = [
  'tailscale',
  '/Applications/Tailscale.app/Contents/MacOS/Tailscale',
  '/usr/local/bin/tailscale',
  '/opt/homebrew/bin/tailscale',
  '/usr/bin/tailscale',
  'C:\\Program Files\\Tailscale\\tailscale.exe',
];
let tailscaleBin: string | null = null;
let peerCache: { at: number; peers: { ips: string[]; dns: string; online: boolean }[] } | null =
  null;

function tailnetPeers(): { ips: string[]; dns: string; online: boolean }[] | null {
  if (peerCache && Date.now() - peerCache.at < 10_000) return peerCache.peers;
  const order = tailscaleBin ? [tailscaleBin, ...TAILSCALE_CANDIDATES] : TAILSCALE_CANDIDATES;
  for (const bin of order) {
    try {
      const status = JSON.parse(
        execFileSync(bin, ['status', '--json'], {
          encoding: 'utf8',
          timeout: 1500,
          maxBuffer: 1024 * 1024,
          stdio: ['ignore', 'pipe', 'ignore'],
        }),
      ) as {
        BackendState?: string;
        Peer?: Record<string, { TailscaleIPs?: string[]; DNSName?: string; Online?: boolean }>;
      };
      tailscaleBin = bin;
      const peers =
        status.BackendState === 'Running'
          ? Object.values(status.Peer || {}).map((p) => ({
              ips: p.TailscaleIPs || [],
              dns: (p.DNSName || '').toLowerCase().replace(/\.$/, ''),
              online: p.Online !== false,
            }))
          : [];
      peerCache = { at: Date.now(), peers };
      return peers;
    } catch {
      /* try the next location */
    }
  }
  return null;
}

function isVerifiedTailnetPeer(hostname: string): boolean {
  const peers = tailnetPeers();
  if (!peers) return false;
  const host = hostname.toLowerCase();
  return peers.some((p) => p.online && (p.ips.includes(host) || p.dns === host));
}

// Tailscale addresses (CGNAT 100.64.0.0/10) and MagicDNS names: the shape a
// tailnet peer has even while it is offline.
function looksLikeTailnetHost(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/\.$/, '');
  if (host.endsWith('.ts.net')) return true;
  const m = /^100\.(\d{1,3})\.\d{1,3}\.\d{1,3}$/.exec(host);
  return !!m && Number(m[1]) >= 64 && Number(m[1]) <= 127;
}

// verifyPeer=false only answers "is this endpoint shaped like an allowed one?"
// (used to tell a configured-but-offline node from no node at all). Requests
// that carry a Bearer token always go through the verified form.
function safeEndpoint(value: string, verifyPeer = true): string | null {
  try {
    const url = new URL(value);
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) return null;
    if (url.search || url.hash) return null;
    // HTTP is acceptable on loopback or an active Tailscale peer (whose
    // transport is encrypted). Never send a Bearer token to arbitrary HTTP.
    if (
      url.protocol === 'http:' &&
      !['localhost', '127.0.0.1', '[::1]'].includes(url.hostname.toLowerCase()) &&
      (verifyPeer ? !isVerifiedTailnetPeer(url.hostname) : !looksLikeTailnetHost(url.hostname))
    )
      return null;
    // Callers may paste either the server root or its OpenAI /v1 base URL.
    return url.href.replace(/\/$/, '').replace(/\/v1$/i, '');
  } catch {
    return null;
  }
}

function piProviders(verifyPeer = true): ProviderConfig[] {
  const root = process.env.SHELFY_PI_AGENT_DIR || path.join(os.homedir(), '.pi', 'agent');
  const settings = readJson(path.join(root, 'settings.json')) as PiSettings | null;
  const auth = readJson(path.join(root, 'auth.json')) as Record<string, unknown> | null;
  const servers = settings?.llamaSettings?.servers;
  if (!Array.isArray(servers)) return [];
  return servers.flatMap((server) => {
    if (typeof server?.id !== 'string' || typeof server.url !== 'string') return [];
    const baseUrl = safeEndpoint(server.url, verifyPeer);
    if (!baseUrl) return [];
    const model = settings?.defaultProvider === server.id ? settings.defaultModel : undefined;
    if (typeof model !== 'string' || !model.trim()) return [];
    const entry = auth?.[server.id] as { type?: string; key?: string } | undefined;
    return [
      {
        id: `pi:${server.id}`,
        name: typeof server.name === 'string' ? server.name : server.id,
        baseUrl,
        model,
        apiKey: entry?.type === 'api_key' && typeof entry.key === 'string' ? entry.key : undefined,
        vision: false,
      },
    ];
  });
}

function configuredProviders(verifyPeer = true): ProviderConfig[] {
  const saved = readJson(settingsPath()) as { providers?: unknown } | null;
  const providers = Array.isArray(saved?.providers) ? saved.providers : [];
  return providers.flatMap((raw) => {
    const item = raw as Record<string, unknown>;
    if (
      typeof item.id !== 'string' ||
      !/^[a-z0-9_-]{1,40}$/i.test(item.id) ||
      typeof item.name !== 'string' ||
      typeof item.baseUrl !== 'string' ||
      typeof item.model !== 'string'
    )
      return [];
    const baseUrl = safeEndpoint(item.baseUrl, verifyPeer);
    if (!baseUrl) return [];
    const envName =
      typeof item.apiKeyEnv === 'string' && allowedSecretName(item.apiKeyEnv)
        ? item.apiKeyEnv
        : null;
    const envKey = envName ? resolveEnvSecret(envName) : undefined;
    if (item.apiKeyEnv !== undefined && (!envName || !envKey)) return [];
    const piKeyId =
      typeof item.apiKeyPiProvider === 'string' && /^[a-z0-9_-]{1,40}$/i.test(item.apiKeyPiProvider)
        ? item.apiKeyPiProvider
        : null;
    const piRoot = process.env.SHELFY_PI_AGENT_DIR || path.join(os.homedir(), '.pi', 'agent');
    const piAuth = piKeyId
      ? (readJson(path.join(piRoot, 'auth.json')) as Record<string, unknown> | null)
      : null;
    const piEntry = piKeyId
      ? (piAuth?.[piKeyId] as { type?: string; key?: string } | undefined)
      : null;
    if (piKeyId && (piEntry?.type !== 'api_key' || !piEntry.key)) return [];
    return [
      {
        id: `custom:${item.id}`,
        name: item.name,
        baseUrl,
        model: item.model,
        apiKey:
          envKey ||
          (piEntry?.type === 'api_key' && typeof piEntry.key === 'string'
            ? piEntry.key
            : undefined),
        vision: item.vision === true,
      },
    ];
  });
}

function remoteProviders(verifyPeer = true): ProviderConfig[] {
  const configured = configuredProviders(verifyPeer);
  const saved = readJson(settingsPath()) as { providers?: unknown } | null;
  const configuredUrls = new Set(configured.map((provider) => provider.baseUrl));
  for (const raw of Array.isArray(saved?.providers) ? saved.providers : []) {
    const baseUrl = (raw as Record<string, unknown> | null)?.baseUrl;
    if (typeof baseUrl === 'string') {
      const safe = safeEndpoint(baseUrl, verifyPeer);
      if (safe) configuredUrls.add(safe);
    }
  }
  // Explicit provider entries supersede Pi's single default-model discovery for
  // the same server, which can otherwise leave a stale model in the picker.
  return [
    ...piProviders(verifyPeer).filter((provider) => !configuredUrls.has(provider.baseUrl)),
    ...configured,
  ];
}

export function getProviderSettings(): ProviderSettingsView {
  const saved = readJson(settingsPath()) as {
    searchProvider?: unknown;
    visionProvider?: unknown;
    providers?: unknown;
  } | null;
  const available = new Set(remoteProviders().map(({ id }) => id));
  const providers = (Array.isArray(saved?.providers) ? saved.providers : []).flatMap((raw) => {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return [];
    const item = raw as Record<string, unknown>;
    if (
      typeof item.id !== 'string' ||
      typeof item.name !== 'string' ||
      typeof item.baseUrl !== 'string' ||
      typeof item.model !== 'string'
    )
      return [];
    const apiKeyEnv = typeof item.apiKeyEnv === 'string' ? item.apiKeyEnv : undefined;
    const apiKeyPiProvider =
      typeof item.apiKeyPiProvider === 'string' ? item.apiKeyPiProvider : undefined;
    return [
      {
        id: item.id,
        name: item.name,
        baseUrl: item.baseUrl,
        model: item.model,
        apiKeyEnv,
        apiKeyPiProvider,
        vision: item.vision === true,
        secretConfigured: apiKeyEnv ? !!resolveEnvSecret(apiKeyEnv) : false,
        available: available.has(`custom:${item.id}`),
      },
    ];
  });
  return {
    searchProvider: typeof saved?.searchProvider === 'string' ? saved.searchProvider : 'local',
    visionProvider: typeof saved?.visionProvider === 'string' ? saved.visionProvider : '',
    providers,
  };
}

function validateBaseUrl(value: string): string {
  try {
    const url = new URL(value.trim());
    if (
      !['http:', 'https:'].includes(url.protocol) ||
      url.username ||
      url.password ||
      url.search ||
      url.hash
    )
      throw new Error('Invalid endpoint');
    return url.href.replace(/\/$/, '');
  } catch {
    throw new Error('URL del provider non valida');
  }
}

export function saveProviderSettings(input: ProviderSettingsInput): ProviderSettingsView {
  if (!input || !Array.isArray(input.providers) || input.providers.length > 12) {
    throw new Error('Configurazione provider non valida');
  }
  const ids = new Set<string>();
  const providers: EditableProvider[] = input.providers.map((item) => {
    if (!item || typeof item !== 'object') throw new Error('Provider non valido');
    const id = typeof item.id === 'string' ? item.id.trim() : '';
    const name = typeof item.name === 'string' ? item.name.trim() : '';
    const model = typeof item.model === 'string' ? item.model.trim() : '';
    if (!/^[a-z0-9_-]{1,40}$/i.test(id) || ids.has(id)) {
      throw new Error('ID provider non valido o duplicato');
    }
    if (!name || name.length > 80 || !model || model.length > 120) {
      throw new Error('Nome o ID modello non valido');
    }
    ids.add(id);
    const apiKeyEnv = typeof item.apiKeyEnv === 'string' ? item.apiKeyEnv.trim() : '';
    const apiKeyPiProvider =
      typeof item.apiKeyPiProvider === 'string' ? item.apiKeyPiProvider.trim() : '';
    if (apiKeyEnv && !allowedSecretName(apiKeyEnv)) {
      throw new Error('Nome del segreto non valido');
    }
    if (apiKeyPiProvider && !/^[a-z0-9_-]{1,40}$/i.test(apiKeyPiProvider)) {
      throw new Error('Riferimento Pi non valido');
    }
    if (apiKeyEnv && apiKeyPiProvider) throw new Error('Scegli una sola sorgente per la chiave');
    return {
      id,
      name,
      baseUrl: validateBaseUrl(item.baseUrl),
      model,
      ...(apiKeyEnv ? { apiKeyEnv } : {}),
      ...(apiKeyPiProvider ? { apiKeyPiProvider } : {}),
      vision: item.vision === true,
    };
  });
  const selected = new Set(providers.map(({ id }) => `custom:${id}`));
  if (input.searchProvider !== 'local' && !selected.has(input.searchProvider)) {
    throw new Error('Provider predefinito non valido');
  }
  if (
    input.visionProvider &&
    !providers.some(({ id, vision }) => `custom:${id}` === input.visionProvider && vision)
  )
    throw new Error('Provider visivo non valido');

  const secretNames = new Set(providers.map(({ apiKeyEnv }) => apiKeyEnv).filter(Boolean));
  const secrets = input.secrets || {};
  if (typeof secrets !== 'object' || Array.isArray(secrets)) {
    throw new Error('Segreti non validi');
  }
  for (const [name, value] of Object.entries(secrets)) {
    if (!secretNames.has(name) || typeof value !== 'string' || value.length > 4096) {
      throw new Error('Segreto non valido');
    }
    if (!value) continue;
    if (process.platform !== 'darwin') throw new Error('Portachiavi disponibile solo su macOS');
    try {
      execFileSync(
        '/usr/bin/security',
        ['add-generic-password', '-a', os.userInfo().username, '-s', name, '-w', value, '-U'],
        { timeout: 5000, stdio: 'ignore' },
      );
    } catch {
      // Never propagate the child-process error: it can contain argv with the key.
      throw new Error('Impossibile salvare la chiave nel Portachiavi');
    }
  }

  const file = settingsPath();
  const current = (readJson(file) as Record<string, unknown> | null) || {};
  const next = {
    ...current,
    searchProvider: input.searchProvider,
    visionProvider: input.visionProvider || '',
    providers,
  };
  const temporary = `${file}.tmp`;
  fs.writeFileSync(temporary, JSON.stringify(next, null, 2), { mode: 0o600 });
  fs.chmodSync(temporary, 0o600);
  fs.renameSync(temporary, file);
  selectionChanged();
  return getProviderSettings();
}

// The Ornith router keeps only one model loaded. Hold the slot until the entire
// response body has been consumed, so a chat stream cannot overlap a request to
// the other model. Serializing same-model calls also matches its parallel=1 preset.
let remoteRequestTail: Promise<void> = Promise.resolve();
export async function acquireRemoteRequest(signal?: AbortSignal): Promise<() => void> {
  const previous = remoteRequestTail;
  let release!: () => void;
  const current = new Promise<void>((resolve) => {
    release = resolve;
  });
  remoteRequestTail = previous.then(() => current);
  if (!signal) {
    await previous;
    return release;
  }
  const abortError = (): Error => Object.assign(new Error('AbortError'), { name: 'AbortError' });
  if (signal.aborted) {
    release();
    throw abortError();
  }
  let onAbort: (() => void) | undefined;
  try {
    await Promise.race([
      previous,
      new Promise<void>((_resolve, reject) => {
        onAbort = () => reject(abortError());
        signal.addEventListener('abort', onAbort, { once: true });
        if (signal.aborted) onAbort();
      }),
    ]);
    if (signal.aborted) throw abortError();
    return release;
  } catch (error) {
    release();
    throw error;
  } finally {
    if (onAbort) signal.removeEventListener('abort', onAbort);
  }
}

export function listSearchProviders(): SearchProviderInfo[] {
  const selected = selectedId();
  const remote = remoteProviders();
  const effective =
    selected !== 'local' && !remote.some((p) => p.id === selected) ? 'local' : selected;
  return [
    { id: 'local', name: 'Locale', selected: effective === 'local' },
    ...remote.map(({ id, name, vision }) => ({ id, name, selected: effective === id, vision })),
  ];
}

export function selectSearchProvider(id: string): SearchProviderInfo[] {
  if (id !== 'local' && !remoteProviders().some((p) => p.id === id)) {
    throw new Error('Provider AI non disponibile');
  }
  const file = settingsPath();
  const current = (readJson(file) as Record<string, unknown> | null) || {};
  fs.writeFileSync(file, JSON.stringify({ ...current, searchProvider: id }, null, 2), {
    mode: 0o600,
  });
  selectionChanged();
  return listSearchProviders();
}

function pickSearch(list: ProviderConfig[]): ProviderConfig | null {
  const id = selectedId();
  if (id === 'local') return null;
  return list.find((p) => p.id === id) || null;
}

function pickVision(list: ProviderConfig[]): ProviderConfig | null {
  const provider = pickSearch(list);
  if (!provider) return null;
  if (provider.vision) return provider;
  const saved = readJson(settingsPath()) as { visionProvider?: unknown } | null;
  if (typeof saved?.visionProvider !== 'string') return null;
  return (
    list.find((candidate) => candidate.id === saved.visionProvider && candidate.vision) || null
  );
}

export function selectedRemoteProvider(): ProviderConfig | null {
  return pickSearch(remoteProviders());
}

export function selectedVisionProvider(): ProviderConfig | null {
  return pickVision(remoteProviders());
}

// ─── Remote routing gate ────────────────────────────────────────────────────────
// A configured remote node is used only after a probe confirms it answers. While
// it is unreachable the AI work waits ('blocked') instead of silently switching
// to the local model; the user can opt into local models until the app restarts.

export type AiKind = 'search' | 'vision';
export type AiRoute =
  | { mode: 'local' }
  | { mode: 'remote'; provider: ProviderConfig }
  | { mode: 'blocked'; name: string };

export interface RemoteStatus {
  configured: boolean; // a remote node is selected and has its credentials
  reachable: boolean | null; // null until the first probe completes
  localOverride: boolean; // the user chose local models for this session
  providerName: string | null;
  checking: boolean;
}

const PROBE_TIMEOUT_MS = 5000;
const reachability = new Map<string, boolean>(); // baseUrl → last probe result
let localOverride = false;
let probing: Promise<RemoteStatus> | null = null;
const statusListeners = new Set<(status: RemoteStatus) => void>();
let lastNotified = '';

function configuredTarget(kind: AiKind): ProviderConfig | null {
  const list = remoteProviders(false);
  return kind === 'vision' ? pickVision(list) : pickSearch(list);
}

function configuredTargets(): ProviderConfig[] {
  const targets = [configuredTarget('search'), configuredTarget('vision')].filter(
    (p): p is ProviderConfig => !!p,
  );
  return targets.filter((p, i) => targets.findIndex((q) => q.baseUrl === p.baseUrl) === i);
}

export function aiRoute(kind: AiKind): AiRoute {
  if (localOverride) return { mode: 'local' };
  const target = configuredTarget(kind);
  if (!target) return { mode: 'local' };
  if (reachability.get(target.baseUrl) !== true) return { mode: 'blocked', name: target.name };
  // Re-resolve through the verified path: the Bearer token only travels over
  // HTTPS, loopback or a Tailscale peer that is online right now.
  const verified = kind === 'vision' ? selectedVisionProvider() : selectedRemoteProvider();
  if (!verified || verified.baseUrl !== target.baseUrl) {
    return { mode: 'blocked', name: target.name };
  }
  return { mode: 'remote', provider: verified };
}

export function getRemoteStatus(): RemoteStatus {
  const targets = configuredTargets();
  const configured = targets.length > 0;
  const known = targets.every((p) => reachability.has(p.baseUrl));
  return {
    configured,
    reachable: !configured
      ? null
      : known
        ? targets.every((p) => reachability.get(p.baseUrl) === true)
        : null,
    localOverride,
    providerName: targets[0]?.name ?? null,
    checking: probing !== null,
  };
}

function notifyStatus(): void {
  const status = getRemoteStatus();
  const key = JSON.stringify(status);
  if (key === lastNotified) return;
  lastNotified = key;
  for (const listener of statusListeners) listener(status);
}

export function onRemoteStatusChange(listener: (status: RemoteStatus) => void): () => void {
  statusListeners.add(listener);
  return () => statusListeners.delete(listener);
}

async function probeEndpoint(target: ProviderConfig): Promise<boolean> {
  // Same transport rule as real requests: never send the key to an unverified host.
  if (!safeEndpoint(target.baseUrl)) return false;
  try {
    const res = await fetch(`${target.baseUrl}/v1/models`, {
      headers: target.apiKey ? { Authorization: `Bearer ${target.apiKey}` } : {},
      signal: AbortSignal.timeout(PROBE_TIMEOUT_MS),
      redirect: 'error',
    });
    return res.ok;
  } catch {
    return false;
  }
}

export function probeRemote(): Promise<RemoteStatus> {
  if (probing) return probing;
  probing = (async () => {
    const targets = configuredTargets();
    const results = await Promise.all(targets.map(probeEndpoint));
    targets.forEach((target, i) => reachability.set(target.baseUrl, results[i]));
    return { ...getRemoteStatus(), checking: false };
  })().finally(() => {
    probing = null;
    notifyStatus();
  });
  notifyStatus();
  return probing;
}

export function useLocalModelsForSession(): RemoteStatus {
  localOverride = true;
  notifyStatus();
  return getRemoteStatus();
}

// Explicitly picking a remote node revokes the session's local-models choice;
// either way the new selection is probed right away.
function selectionChanged(): void {
  if (selectedId() !== 'local') localOverride = false;
  if (monitorStarted) void probeRemote();
}

let monitorStarted = false;
// Polls quickly while the node is down (so queued work resumes promptly) and
// slowly while it answers (to notice a drop before the next request does).
export function startRemoteMonitor(): void {
  if (monitorStarted) return;
  monitorStarted = true;
  const tick = async (): Promise<void> => {
    const status = getRemoteStatus();
    if (status.configured && !status.localOverride) await probeRemote();
    else notifyStatus();
    const next = getRemoteStatus();
    setTimeout(tick, next.configured && next.reachable !== true ? 15_000 : 60_000).unref?.();
  };
  void tick();
}

export function chatEndpoint(provider: ProviderConfig): {
  url: string;
  headers: Record<string, string>;
  model: string;
} {
  return {
    url: `${provider.baseUrl}/v1/chat/completions`,
    headers: {
      'Content-Type': 'application/json',
      ...(provider.apiKey ? { Authorization: `Bearer ${provider.apiKey}` } : {}),
    },
    model: provider.model,
  };
}
