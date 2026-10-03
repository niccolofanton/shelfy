// Loopback-only process control for the real-server e2e provider. Tests stop
// the actual TCP server; neither provider status nor API answers are mocked.
import { spawn, type ChildProcess } from 'node:child_process';
import { createServer } from 'node:http';
import { E2E } from './env';

export async function startAiStub(): Promise<() => void> {
  let stub: ChildProcess | null = null;
  const start = async (): Promise<void> => {
    if (stub && stub.exitCode === null && !stub.killed) return;
    const child = spawn(
      E2E.stubBin,
      [
        '--listen',
        `127.0.0.1:${E2E.stubPort}`,
        '--key-env',
        'SHELFY_E2E_STUB_KEY',
        '--latency-ms',
        String(E2E.stubLatencyMs),
      ],
      { env: process.env, stdio: ['ignore', 'ignore', 'inherit'] },
    );
    stub = child;
    let launchError: unknown;
    child.once('error', (error) => {
      launchError = error;
    });
    for (let attempt = 0; attempt < 100; attempt++) {
      if (launchError) throw launchError;
      if (child.exitCode !== null) throw new Error('AI stub stopped during startup');
      try {
        if ((await fetch(`${E2E.stubUrl}/health`)).ok) return;
      } catch {
        /* still starting */
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    throw new Error('AI stub did not start');
  };
  const stop = async (): Promise<void> => {
    const current = stub;
    if (!current || current.exitCode !== null) return;
    await new Promise<void>((resolve) => {
      current.once('exit', () => resolve());
      current.kill('SIGTERM');
    });
    stub = null;
  };
  await start();
  const control = createServer((request, response) => {
    if (request.method !== 'POST' || !['/stop', '/start'].includes(request.url ?? '')) {
      response.writeHead(404).end();
      return;
    }
    void (request.url === '/stop' ? stop() : start())
      .then(() => response.writeHead(204).end())
      .catch(() => response.writeHead(500).end());
  });
  await new Promise<void>((resolve, reject) => {
    control.once('error', reject);
    control.listen(E2E.stubControlPort, '127.0.0.1', resolve);
  });
  return () => {
    control.close();
    if (stub?.exitCode === null) stub.kill('SIGTERM');
  };
}
