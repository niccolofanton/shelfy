// Resource accounting for one capture, read from inside the container. Docker
// mounts the container's own cgroup (v2) at /sys/fs/cgroup, so these numbers
// cover every process in it: Node, Chromium and ffmpeg. Outside a container
// (local runs) the cgroup files are missing and the numbers are null.

import fs from 'fs';

const CG = '/sys/fs/cgroup';

function readText(file: string): string | null {
  try {
    return fs.readFileSync(file, 'utf8');
  } catch {
    return null;
  }
}

function readNumber(file: string): number | null {
  const t = readText(file);
  if (t === null) return null;
  const n = Number(t.trim());
  return Number.isFinite(n) ? n : null;
}

function readKeyed(file: string): Record<string, number> {
  const out: Record<string, number> = {};
  for (const line of (readText(file) || '').split('\n')) {
    const [k, v] = line.trim().split(/\s+/);
    if (k && v !== undefined && Number.isFinite(Number(v))) out[k] = Number(v);
  }
  return out;
}

export function cpuStat(): Record<string, number> {
  return readKeyed(`${CG}/cpu.stat`);
}

export function memoryEvents(): Record<string, number> {
  return readKeyed(`${CG}/memory.events`);
}

export function memoryPeak(): number | null {
  return readNumber(`${CG}/memory.peak`);
}

export function cgroupLimits(): { memoryMax: string | null; cpuMax: string | null } {
  return {
    memoryMax: readText(`${CG}/memory.max`)?.trim() ?? null,
    cpuMax: readText(`${CG}/cpu.max`)?.trim() ?? null,
  };
}

// Bytes on every interface but loopback. In the capture container that is the
// one link to the proxy: what Chromium and Node exchanged with the internet,
// plus the proxy's CONNECT overhead.
export function netBytes(): { rx: number; tx: number } | null {
  let names: string[];
  try {
    names = fs.readdirSync('/sys/class/net').filter((n) => n !== 'lo');
  } catch {
    return null;
  }
  let rx = 0;
  let tx = 0;
  for (const n of names) {
    rx += readNumber(`/sys/class/net/${n}/statistics/rx_bytes`) || 0;
    tx += readNumber(`/sys/class/net/${n}/statistics/tx_bytes`) || 0;
  }
  return { rx, tx };
}

export interface MemSample {
  current: number;
  anon: number;
  shmem: number;
  file: number;
  kernel: number;
}

function memSample(): MemSample | null {
  const current = readNumber(`${CG}/memory.current`);
  if (current === null) return null;
  const st = readKeyed(`${CG}/memory.stat`);
  return {
    current,
    anon: st.anon || 0,
    // tmpfs (/tmp scratch PNGs, Chromium's shared memory) is shmem: unlike page
    // cache it cannot be reclaimed, and the container has no swap.
    shmem: st.shmem || 0,
    file: st.file || 0,
    kernel: st.kernel || 0,
  };
}

// Polls memory and CPU. The peak of anon + shmem + kernel is the memory the
// capture really holds ("peak RSS" in the notes); memory.peak also counts
// reclaimable page cache.
export class Sampler {
  private timer: ReturnType<typeof setInterval> | null = null;
  private t0 = Date.now();
  peak = { current: 0, held: 0, anon: 0, shmem: 0, kernel: 0 };
  heldAtPeak: MemSample | null = null;
  series: [number, number, number, number][] = []; // [s, current MiB, held MiB, cpu s]
  private lastSeries = -1;

  start(intervalMs = 250): void {
    this.t0 = Date.now();
    this.tick();
    this.timer = setInterval(() => this.tick(), intervalMs);
    this.timer.unref();
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
    this.tick();
  }

  private tick(): void {
    const m = memSample();
    if (!m) return;
    const held = m.anon + m.shmem + m.kernel;
    this.peak.current = Math.max(this.peak.current, m.current);
    this.peak.anon = Math.max(this.peak.anon, m.anon);
    this.peak.shmem = Math.max(this.peak.shmem, m.shmem);
    this.peak.kernel = Math.max(this.peak.kernel, m.kernel);
    if (held > this.peak.held) {
      this.peak.held = held;
      this.heldAtPeak = m;
    }
    const s = Math.floor((Date.now() - this.t0) / 1000);
    if (s !== this.lastSeries) {
      this.lastSeries = s;
      const cpu = (cpuStat().usage_usec || 0) / 1e6;
      const mib = (b: number): number => Math.round(b / 1048576);
      this.series.push([s, mib(m.current), mib(held), Math.round(cpu * 10) / 10]);
    }
  }
}

// Total size of the files under a directory.
export function dirBytes(dir: string): { bytes: number; files: number } {
  let bytes = 0;
  let files = 0;
  const walk = (d: string): void => {
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      const p = `${d}/${e.name}`;
      if (e.isDirectory()) walk(p);
      else if (e.isFile()) {
        bytes += fs.statSync(p).size;
        files++;
      }
    }
  };
  walk(dir);
  return { bytes, files };
}
