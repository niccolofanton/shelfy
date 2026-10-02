// Golden-fixture plumbing shared by the generators in this directory.
//
// A golden set runs one desktop function on fixed inputs and records each
// output. The file format is JSON Lines, so every case is one line and the Rust
// side can read each `output` back as the exact bytes `JSON.stringify` wrote:
//
//   {"golden":"<name>","source":"<file>#<export>","generator":"<path>","format":1}
//   {"id":"<case id>","args":[...],"output":...}
//   ...
//
// See README.md for the workflow.

export interface GoldenCase {
  /** Stable, unique id; it names the case in Rust test failures. */
  id: string;
  /** Arguments of the call, as JSON. */
  args: unknown[];
  /** What the desktop function returned, as JSON. */
  output: unknown;
}

export interface GoldenSet {
  /** File name under shared/golden/, without `.jsonl`. */
  name: string;
  /** The desktop function: `<repo path>#<export name>`. */
  source: string;
  /** This generator's path, relative to the repo root. */
  generator: string;
  /** Runs the desktop function on every input. */
  build(): GoldenCase[];
}

/** The JSONL text of a set. Throws on duplicate case ids. */
export function render(set: GoldenSet): string {
  const cases = set.build();
  const seen = new Set<string>();
  for (const c of cases) {
    if (seen.has(c.id)) throw new Error(`${set.name}: duplicate case id "${c.id}"`);
    seen.add(c.id);
  }
  const header = { golden: set.name, source: set.source, generator: set.generator, format: 1 };
  const lines = [header, ...cases.map((c) => ({ id: c.id, args: c.args, output: c.output }))];
  return lines.map((l) => JSON.stringify(l)).join('\n') + '\n';
}
