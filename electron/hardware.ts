'use strict';

// Hardware probe + tuning heuristics for the local AI backends.
//
// The llama-server / whisper-server spawns used to pass identical flags on every
// machine (an M1 Air and an RTX 4090 box got the same `-ngl 99 -ub 1024`, and
// whisper got no `-t` at all). This module detects the host once (cached) and
// derives sensible *defaults* for the spawn flags. Everything here is advisory:
// analyzer.js / stt.js layer the user's manual overrides on top (see resolveTuning).
//
// Detection is best-effort and never throws: each native probe (sysctl, nvidia-smi,
// wmic/PowerShell) runs with a short timeout and any failure falls back to the
// portable `os` numbers. A machine where every probe fails still yields a usable
// (if conservative) profile.

import os from 'os';
import { spawnSync } from 'child_process';

const GB = 1024 ** 3;

// ── File-internal shapes (no Shelfy.* domain type fits these host-only structs) ──
interface CpuInfo {
  logical: number;
  physical: number;
  perf: number; // performance/big cores; 0 = unknown
}

type GpuVendor = 'nvidia' | 'apple' | 'intel' | 'amd' | 'unknown';

interface GpuInfo {
  vendor: GpuVendor;
  name: string;
  vramGB: number | null;
  cuda: boolean;
  unified: boolean;
}

type RecommendedVariant = 'metal' | 'cuda' | 'vulkan' | 'cpu';

interface HostProfile {
  platform: NodeJS.Platform;
  arch: string;
  appleSilicon: boolean;
  totalRamGB: number;
  cpu: CpuInfo;
  gpu: GpuInfo;
  recommendedVariant: RecommendedVariant;
}

type KvCacheType = 'f16' | 'q8_0';

interface LlamaTuning {
  threads: number;
  threadsBatch: number;
  gpuLayers: 'fit' | 0;
  ubatch: number;
  kvCache: KvCacheType;
  memoryWarning: string | null;
}

interface LlamaTuningOptions {
  variant?: string; // the llama.cpp build actually installed
  modelSizeGB?: number;
  concurrency?: number;
  ctxPerSlot?: number;
}

interface ThreadTuning {
  threads: number;
}

// Minimal preset projection recommendModel consumes (analyzer's MODELS mapped down).
interface ModelPreset {
  id: string;
  minRamGB: number;
  sizeGB?: number;
}

// Run a native command with a hard timeout; return trimmed stdout or '' on any
// failure (missing binary, non-zero exit, timeout). Never throws.
function probe(cmd: string, args: string[], timeout = 1500): string {
  try {
    const r = spawnSync(cmd, args, {
      timeout,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      windowsHide: true,
    });
    if (r.status !== 0 || !r.stdout) return '';
    return r.stdout.trim();
  } catch {
    return '';
  }
}

// ── CPU: physical vs logical cores, and performance ("big") cores ──────────────
// `-t` should target physical (not HT/logical) cores, and on hybrid CPUs (Apple
// Silicon, Intel P+E) only the performance cores — oversubscribing the E-cores
// hurts throughput. We fall back to a halved logical count when we can't tell.
function detectCpu(): CpuInfo {
  const logical = os.cpus()?.length || 4;
  let physical = 0;
  let perf = 0; // performance/big cores; 0 = unknown

  if (process.platform === 'darwin') {
    physical = Number(probe('sysctl', ['-n', 'hw.physicalcpu'])) || 0;
    // perflevel0 = P-cores on Apple Silicon; absent on Intel Macs.
    perf = Number(probe('sysctl', ['-n', 'hw.perflevel0.physicalcpu'])) || 0;
  } else if (process.platform === 'win32') {
    // `NumberOfCores` is the physical core count (per socket; summed for safety).
    const out = probe('wmic', ['cpu', 'get', 'NumberOfCores', '/value']);
    physical = out.split(/\r?\n/).reduce((sum, line) => {
      const m = line.match(/NumberOfCores=(\d+)/);
      return sum + (m ? Number(m[1]) : 0);
    }, 0);
    // wmic is deprecated and absent by default on Windows 11 24H2+ — fall back to
    // PowerShell's CIM cmdlet, which is always present.
    if (!physical) {
      physical =
        Number(
          probe(
            'powershell',
            [
              '-NoProfile',
              '-Command',
              '(Get-CimInstance Win32_Processor | Measure-Object -Property NumberOfCores -Sum).Sum',
            ],
            4000,
          ),
        ) || 0;
    }
  } else {
    // Linux: count distinct physical "core id" entries in /proc/cpuinfo.
    try {
      const fs = require('fs') as typeof import('fs');
      const txt = fs.readFileSync('/proc/cpuinfo', 'utf8');
      const ids = new Set<string>();
      let pkg: string | undefined = '',
        core: string | undefined = '';
      for (const line of txt.split('\n')) {
        if (line.startsWith('physical id')) pkg = line.split(':')[1]?.trim();
        else if (line.startsWith('core id')) core = line.split(':')[1]?.trim();
        else if (line.trim() === '') {
          if (core) ids.add(`${pkg}:${core}`);
          pkg = core = '';
        }
      }
      physical = ids.size;
    } catch {
      /* fall through to estimate */
    }
  }

  // Estimate when a probe failed: assume hyper-threading (logical/2) for x64,
  // and 1:1 for arm (most arm cores aren't SMT).
  if (!physical || physical > logical) {
    physical = process.arch === 'arm64' ? logical : Math.max(1, Math.round(logical / 2));
  }
  return { logical, physical, perf };
}

// ── GPU + VRAM + CUDA ──────────────────────────────────────────────────────────
// Returns { vendor, name, vramGB|null, cuda, unified }. VRAM is null when it
// can't be read reliably (notably macOS unified memory, where there is no
// separate VRAM pool — callers should reason against system RAM there).
function detectGpu(totalRamGB: number): GpuInfo {
  // NVIDIA first: if nvidia-smi answers we know it's CUDA-capable and get exact VRAM.
  const smi = probe('nvidia-smi', [
    '--query-gpu=name,memory.total',
    '--format=csv,noheader,nounits',
  ]);
  if (smi) {
    const [name, mib] = smi
      .split('\n')[0]
      .split(',')
      .map((s) => s.trim());
    const vramGB = Number(mib) ? Number(mib) / 1024 : null;
    return { vendor: 'nvidia', name: name || 'NVIDIA GPU', vramGB, cuda: true, unified: false };
  }

  if (process.platform === 'darwin') {
    // Apple Silicon: unified memory — the GPU shares system RAM. macOS caps the
    // GPU working set at ~70% of physical RAM by default (recommendedMaxWorkingSetSize).
    const isArm = process.arch === 'arm64';
    const name =
      probe('sysctl', ['-n', 'machdep.cpu.brand_string']) || (isArm ? 'Apple Silicon' : 'Mac');
    return {
      vendor: isArm ? 'apple' : 'intel',
      name: isArm ? `Apple GPU (${name})` : 'Intel/AMD GPU',
      vramGB: isArm ? Math.round(totalRamGB * 0.7) : null,
      cuda: false,
      unified: isArm,
    };
  }

  if (process.platform === 'win32') {
    // No NVIDIA: read the adapter name (AMD/Intel → Vulkan candidate). AdapterRAM
    // is a 32-bit field that overflows above 4 GB, so we only trust the name.
    let out = probe('wmic', ['path', 'win32_VideoController', 'get', 'Name', '/value']);
    // wmic-less Windows 11 24H2+: fall back to PowerShell.
    if (!out) {
      out = probe(
        'powershell',
        [
          '-NoProfile',
          '-Command',
          '(Get-CimInstance Win32_VideoController).Name | ForEach-Object { "Name=" + $_ }',
        ],
        4000,
      );
    }
    const names = out
      .split(/\r?\n/)
      .map((l) => l.match(/Name=(.+)/)?.[1]?.trim())
      .filter(Boolean) as string[];
    const name =
      names.find((n) => /radeon|amd|arc|intel|nvidia|geforce/i.test(n)) || names[0] || '';
    const vendor: GpuVendor = /radeon|amd/i.test(name)
      ? 'amd'
      : /arc|intel/i.test(name)
        ? 'intel'
        : /nvidia|geforce/i.test(name)
          ? 'nvidia'
          : 'unknown';
    return { vendor, name: name || 'GPU sconosciuta', vramGB: null, cuda: false, unified: false };
  }

  // Linux without NVIDIA: leave GPU undetermined (Vulkan may still work).
  return { vendor: 'unknown', name: '', vramGB: null, cuda: false, unified: false };
}

// ── Public: cached host profile ────────────────────────────────────────────────
let _cache: HostProfile | null = null;

function detect(): HostProfile {
  if (_cache) return _cache;
  const totalRamGB = os.totalmem() / GB;
  const cpu = detectCpu();
  const gpu = detectGpu(totalRamGB);
  // Recommended llama.cpp acceleration variant for this host. Only meaningful on
  // Windows (where the user picks/downloads a variant build); macOS is always
  // Metal and Linux ships CPU here.
  // NOTE: keep this mapping in sync with provision-binaries.ps1 → Detect-Recommended
  // (the offline PowerShell recovery path uses the same NVIDIA→cuda / AMD·Intel→vulkan
  // / else→cpu rule). NVIDIA is matched by nvidia-smi OR adapter name, so a card whose
  // driver lacks nvidia-smi still gets cuda.
  const recommendedVariant: RecommendedVariant =
    process.platform === 'darwin'
      ? 'metal'
      : gpu.cuda || gpu.vendor === 'nvidia'
        ? 'cuda'
        : gpu.vendor === 'amd' || gpu.vendor === 'intel'
          ? 'vulkan'
          : 'cpu';

  _cache = {
    platform: process.platform,
    arch: process.arch,
    appleSilicon: process.platform === 'darwin' && process.arch === 'arm64',
    totalRamGB: Math.round(totalRamGB * 10) / 10,
    cpu,
    gpu,
    recommendedVariant,
  };
  return _cache;
}

// Test seam / safety valve: drop the cache (e.g. after a GPU driver change).
function reset(): void {
  _cache = null;
}

// ── Tuning heuristics ──────────────────────────────────────────────────────────
// Given the host profile + the active variant + the workload (model size,
// concurrency), derive conservative spawn defaults. Pure function of its inputs
// so it's trivially testable; callers overlay user overrides afterwards.
//
// `variant` is the llama.cpp build actually installed ('metal'|'cuda'|'vulkan'|'cpu').
// `modelSizeGB` is the on-disk weight size (model + projector), `concurrency` the
// server slot count, `ctxPerSlot` the per-slot context budget.
function computeLlamaTuning({
  variant = 'cpu',
  modelSizeGB = 0,
  concurrency = 1,
  ctxPerSlot = 4096,
}: LlamaTuningOptions = {}): LlamaTuning {
  const hw = detect();
  const gpuAccel = variant === 'metal' || variant === 'cuda' || variant === 'vulkan';

  // threads: physical cores, but only the performance cores on Apple Silicon.
  // Cap at 8 — beyond that llama.cpp sees diminishing/negative returns, and with
  // full GPU offload the CPU does little anyway.
  const baseCores = hw.appleSilicon && hw.cpu.perf ? hw.cpu.perf : hw.cpu.physical;
  const threads = Math.max(1, Math.min(8, baseCores));
  // Prefill (batch) is more CPU-bound: allow all physical cores, capped a bit higher.
  const threadsBatch = Math.max(threads, Math.min(12, hw.cpu.physical));

  // gpu-layers: on a GPU build, default to 'fit' — i.e. DON'T pass -ngl and let
  // llama-server's --fit (default on) pick how many layers fit in device memory.
  // That's more reliable than guessing here, and it's the only sane option when we
  // can't read VRAM (AMD/Intel on Windows). On a CPU build, 0 (no offload).
  // Surface a soft warning when we *can* see the model won't fully fit (NVIDIA, where
  // nvidia-smi gives exact VRAM) so the UI can hint, without overriding --fit.
  const gpuLayers: 'fit' | 0 = gpuAccel ? 'fit' : 0;
  let memoryWarning: string | null = null;
  if (gpuAccel && hw.gpu.vramGB != null && !hw.gpu.unified && modelSizeGB > 0) {
    const kvGB = estimateKvCacheGB(ctxPerSlot * concurrency, modelSizeGB);
    if (modelSizeGB + kvGB + 1 > hw.gpu.vramGB) {
      memoryWarning = `Il modello (~${modelSizeGB.toFixed(1)} GB + cache) supera la VRAM (~${hw.gpu.vramGB.toFixed(0)} GB): alcuni layer resteranno su CPU.`;
    }
  }

  // ubatch: larger = faster prefill but more activation memory. Bump to 2048 only
  // with comfortable memory; shrink to 512 on CPU-only or tight VRAM.
  let ubatch = 1024;
  if (!gpuAccel) ubatch = 512;
  else if (hw.gpu.unified && hw.totalRamGB >= 16) ubatch = 2048;
  else if (hw.gpu.vramGB != null && hw.gpu.vramGB >= 12) ubatch = 2048;
  else if (hw.gpu.vramGB != null && hw.gpu.vramGB < 6) ubatch = 512;

  // KV cache type: q8_0 roughly halves KV memory with negligible quality loss
  // (requires flash-attn, which we keep on). Worth it once the total context is
  // large (high concurrency) or VRAM is tight. Needs -fa on (analyzer passes it).
  let kvCache: KvCacheType = 'f16';
  const bigCtx = ctxPerSlot * concurrency >= 16384;
  const tightVram = hw.gpu.vramGB != null && !hw.gpu.unified && hw.gpu.vramGB < 8;
  if (gpuAccel && (bigCtx || tightVram)) kvCache = 'q8_0';

  return { threads, threadsBatch, gpuLayers, ubatch, kvCache, memoryWarning };
}

// Rough KV-cache footprint (GB) for a given total context. We don't read the GGUF
// metadata, so we approximate from the model's on-disk size as a proxy for layer
// count / hidden dim: ~0.12 GB per 1K tokens per ~4 GB of (Q4) weights, f16 cache.
function estimateKvCacheGB(totalCtx: number, modelSizeGB: number): number {
  const perKTokenPer4GB = 0.12;
  return (totalCtx / 1024) * perKTokenPer4GB * Math.max(1, modelSizeGB / 4);
}

// whisper-server only needs a thread count (it auto-selects its compiled GPU
// backend; there's no -ngl). Physical cores, capped at 8. No flash-attn: it
// degrades non-English transcription quality (whisper.cpp #3020) and our
// dictation is Italian.
function computeWhisperTuning(): ThreadTuning {
  const hw = detect();
  const baseCores = hw.appleSilicon && hw.cpu.perf ? hw.cpu.perf : hw.cpu.physical;
  return { threads: Math.max(1, Math.min(8, baseCores)) };
}

// Embedding server (llama-server --embedding) tuning. The embedder is a tiny
// model (multilingual-e5-small, ~118M params) that runs entirely in prefill, so a
// thread count is all that matters — same heuristic as whisper (physical/perf
// cores, capped at 8). No -ngl: it's faster to keep it on CPU than to pay the
// host↔device transfer for such a small graph.
function computeEmbeddingTuning(): ThreadTuning {
  const hw = detect();
  const baseCores = hw.appleSilicon && hw.cpu.perf ? hw.cpu.perf : hw.cpu.physical;
  return { threads: Math.max(1, Math.min(8, baseCores)) };
}

// Pick the most capable VLM preset that fits comfortably in available memory.
// `presets` is [{ id, minRamGB, sizeGB }...] from analyzer's MODELS. Advisory:
// surfaced as a UI suggestion, never an automatic switch.
function recommendModel(presets: ModelPreset[]): string | null {
  const hw = detect();
  // On unified/Apple, the budget is system RAM; on dedicated GPUs, the smaller of
  // RAM and (VRAM scaled up a touch, since CPU offload can cover a small overflow).
  const budgetGB =
    hw.gpu.vramGB != null && !hw.gpu.unified
      ? Math.min(hw.totalRamGB, hw.gpu.vramGB + 2)
      : hw.totalRamGB;
  // "Comfortable" = the preset's stated minRamGB fits with headroom.
  const fit = presets
    .filter((p) => p.minRamGB <= budgetGB)
    .sort((a, b) => (b.sizeGB || 0) - (a.sizeGB || 0));
  return fit[0]?.id || null;
}

export {
  detect,
  reset,
  computeLlamaTuning,
  computeWhisperTuning,
  computeEmbeddingTuning,
  recommendModel,
  estimateKvCacheGB,
};
