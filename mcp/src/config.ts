import { constants } from 'node:fs';
import { lstat, mkdir, open, rename, rm, stat } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, isAbsolute, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { z } from 'zod';

export class ConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ConfigError';
  }
}
export interface Config {
  url: string;
  token: string;
  write: boolean;
  access?: { clientId: string; clientSecret: string };
}
const credential = z
  .string()
  .min(1)
  .max(1024)
  .regex(/^[^\r\n]+$/);
const access = z.object({ clientId: credential, clientSecret: credential }).strict();
const schema = z
  .object({
    url: z.string(),
    token: z.string(),
    write: z.boolean().default(false),
    access: access.optional(),
  })
  .strict();

export function baseUrl(value: string): string {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new ConfigError('A valid Shelfy server URL is required.');
  }
  if (url.username || url.password || url.search || url.hash || url.pathname !== '/') {
    throw new ConfigError(
      'Use only the Shelfy origin, without credentials, path, query or fragment.',
    );
  }
  const loopback = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname);
  if (url.protocol !== 'https:' && !(url.protocol === 'http:' && loopback)) {
    throw new ConfigError('Use HTTPS; HTTP is accepted only on loopback.');
  }
  return url.origin;
}
export function tokenValue(value: string): string {
  const token = value.trim();
  if (!/^shx_[A-Za-z0-9_-]{20,512}$/.test(token))
    throw new ConfigError('A Shelfy library API token is required.');
  return token;
}
export function configPath(env = process.env): string {
  return join(
    env.XDG_CONFIG_HOME ||
      (process.platform === 'win32' ? env.APPDATA : undefined) ||
      join(homedir(), '.config'),
    'shelfy',
    'mcp.json',
  );
}
export async function privateFile(path: string): Promise<string> {
  if (!isAbsolute(path))
    throw new ConfigError('Configuration and token file paths must be absolute.');
  let file;
  try {
    if ((await lstat(path)).isSymbolicLink())
      throw new ConfigError('Credential files must not be symbolic links.');
    file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    const info = await file.stat();
    if (
      !info.isFile() ||
      info.size > 8192 ||
      info.nlink !== 1 ||
      (process.platform !== 'win32' &&
        ((info.mode & 0o077) !== 0 || (process.getuid && info.uid !== process.getuid())))
    ) {
      throw new ConfigError(
        'Credential files must be regular, private (0600), owned by you and at most 8KiB.',
      );
    }
    return await file.readFile('utf8');
  } catch (error) {
    if (error instanceof ConfigError) throw error;
    throw new ConfigError('Cannot read the private configuration or token file.');
  } finally {
    await file?.close();
  }
}
export async function loadConfig(path: string, write: boolean, env = process.env): Promise<Config> {
  let parsed: Config | undefined;
  if (env.SHELFY_MCP_URL && (env.SHELFY_MCP_TOKEN || env.SHELFY_MCP_TOKEN_FILE)) {
    if (env.SHELFY_MCP_TOKEN && env.SHELFY_MCP_TOKEN_FILE)
      throw new ConfigError('Choose exactly one token source.');
    parsed = {
      url: env.SHELFY_MCP_URL,
      token: env.SHELFY_MCP_TOKEN || (await privateFile(env.SHELFY_MCP_TOKEN_FILE!)),
      write: false,
    };
  } else {
    if (env.SHELFY_MCP_URL || env.SHELFY_MCP_TOKEN || env.SHELFY_MCP_TOKEN_FILE)
      throw new ConfigError('Set URL and exactly one token source together.');
    try {
      parsed = schema.parse(JSON.parse(await privateFile(path)));
    } catch (error) {
      if (error instanceof ConfigError) throw error;
      throw new ConfigError('The private configuration is invalid.');
    }
  }
  let credentials = parsed.access;
  if (env.SHELFY_MCP_CF_ACCESS_CLIENT_ID || env.SHELFY_MCP_CF_ACCESS_CLIENT_SECRET) {
    const parsedAccess = access.safeParse({
      clientId: env.SHELFY_MCP_CF_ACCESS_CLIENT_ID,
      clientSecret: env.SHELFY_MCP_CF_ACCESS_CLIENT_SECRET,
    });
    if (!parsedAccess.success)
      throw new ConfigError(
        'Cloudflare Access requires a valid client id and client secret together.',
      );
    credentials = parsedAccess.data;
  }
  return {
    url: baseUrl(parsed.url),
    token: tokenValue(parsed.token),
    write: write || parsed.write,
    ...(credentials ? { access: credentials } : {}),
  };
}
export async function saveConfig(path: string, config: Config): Promise<void> {
  if (!isAbsolute(path)) throw new ConfigError('Configuration path must be absolute.');
  const dir = dirname(path);
  await mkdir(dir, { recursive: true, mode: 0o700 });
  const info = await stat(dir);
  if (
    process.platform !== 'win32' &&
    ((info.mode & 0o077) !== 0 || (process.getuid && info.uid !== process.getuid()))
  )
    throw new ConfigError('Use a private configuration directory (0700).');
  const temporary = join(dir, `.mcp-${randomUUID()}.tmp`);
  try {
    const file = await open(
      temporary,
      constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY,
      0o600,
    );
    try {
      await file.writeFile(
        JSON.stringify({
          url: baseUrl(config.url),
          token: tokenValue(config.token),
          write: config.write,
          ...(config.access ? { access: access.parse(config.access) } : {}),
        }) + '\n',
      );
      await file.sync();
    } finally {
      await file.close();
    }
    await rename(temporary, path);
  } finally {
    await rm(temporary, { force: true });
  }
}
