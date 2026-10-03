#!/usr/bin/env node
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  ConfigError,
  baseUrl,
  configPath,
  loadConfig,
  privateFile,
  saveConfig,
  tokenValue,
} from './config.js';
import { createServer } from './server.js';

async function main() {
  const args = process.argv.slice(2);
  const configure = args[0] === 'configure';
  if (configure) args.shift();
  let path = configPath();
  let url: string | undefined;
  let tokenFile: string | undefined;
  let write = false;
  while (args.length) {
    const arg = args.shift();
    if (arg === '--write') write = true;
    else if (arg === '--config' && args[0] && !args[0].startsWith('--')) path = args.shift()!;
    else if (configure && arg === '--url' && args[0] && !args[0].startsWith('--'))
      url = args.shift();
    else if (configure && arg === '--token-file' && args[0] && !args[0].startsWith('--'))
      tokenFile = args.shift();
    else if (arg === '--help') {
      process.stderr.write(
        'Shelfy MCP stdio: shelfy-mcp [--config ABSOLUTE_PATH] [--write]\nConfigure: shelfy-mcp configure --url HTTPS_ORIGIN --token-file PRIVATE_0600_FILE [--config ABSOLUTE_PATH] [--write]\nWithout --token-file, configure reads the token from redirected stdin. No token arguments are accepted.\n',
      );
      return;
    } else throw new ConfigError('Unknown or missing command argument. Use --help.');
  }
  if (configure) {
    if (!url) throw new ConfigError('Configure requires --url.');
    let token: string;
    if (tokenFile) token = await privateFile(tokenFile);
    else {
      if (process.stdin.isTTY)
        throw new ConfigError(
          'Use a private --token-file or redirected stdin; do not paste credentials into command arguments.',
        );
      const chunks: Buffer[] = [];
      let size = 0;
      for await (const chunk of process.stdin) {
        size += chunk.length;
        if (size > 8192) throw new ConfigError('Token input is too large.');
        chunks.push(chunk);
      }
      token = Buffer.concat(chunks).toString('utf8');
    }
    const accessId = process.env.SHELFY_MCP_CF_ACCESS_CLIENT_ID;
    const accessSecret = process.env.SHELFY_MCP_CF_ACCESS_CLIENT_SECRET;
    if (Boolean(accessId) !== Boolean(accessSecret))
      throw new ConfigError('Cloudflare Access requires client id and secret together.');
    await saveConfig(path, {
      url: baseUrl(url),
      token: tokenValue(token),
      write,
      ...(accessId && accessSecret
        ? { access: { clientId: accessId, clientSecret: accessSecret } }
        : {}),
    });
    process.stderr.write('Saved private Shelfy MCP configuration.\n');
    return;
  }
  const config = await loadConfig(path, write);
  // Never print protocol-adjacent diagnostics or secrets to stdout.
  const server = createServer(config);
  const transport = new StdioServerTransport(process.stdin, process.stdout, {
    maxBufferSize: 256 * 1024,
  });
  let closing = false;
  const close = async () => {
    if (closing) return;
    closing = true;
    await server.close();
  };
  process.once('SIGINT', () => void close());
  process.once('SIGTERM', () => void close());
  await server.connect(transport);
}
main().catch((error) => {
  process.stderr.write(
    `${error instanceof ConfigError ? error.message : 'Shelfy MCP could not start.'}\n`,
  );
  process.exitCode = 1;
});
