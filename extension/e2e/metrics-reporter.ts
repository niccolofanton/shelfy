import { mkdirSync, writeFileSync } from 'node:fs';
import type { Reporter, FullResult, Suite } from '@playwright/test/reporter';
export default class MetricsReporter implements Reporter {
  private suite?: Suite;
  onBegin(_config: unknown, suite: Suite) {
    this.suite = suite;
  }
  onEnd(result: FullResult) {
    const tests = this.suite?.allTests() ?? [];
    const flaky = tests.filter((test) => test.outcome() === 'flaky').length;
    mkdirSync('extension/e2e-results', { recursive: true });
    writeFileSync(
      'extension/e2e-results/metrics.json',
      JSON.stringify(
        {
          scope: 'synthetic CI with real server; live-account P2-23/24 remains separate',
          status: result.status,
          runtimeMs: result.duration,
          tests: tests.length,
          flaky,
          flakeRate: tests.length ? flaky / tests.length : 0,
          maxRetries: 1,
          maxAttemptsObserved: Math.max(0, ...tests.map((test) => test.results.length)),
        },
        null,
        2,
      ),
    );
  }
}
