// The capture service protocol (plan §2.18): the POST /v1/captures request and
// the NDJSON response lines, plus the manifest.json shape. zod validates the
// request; capture/protocol.schema.json is the generated JSON Schema of the
// manifest, the contract P4-14 (the capture job) validates artifacts against.

import { z } from 'zod';

// Crockford base32 ULID (26 chars, no I/L/O/U).
const ULID = z.string().regex(/^[0-9A-HJKMNP-TV-Z]{26}$/, 'not a ULID');

// ─── Request ────────────────────────────────────────────────────────────────

export const CaptureRequestSchema = z
  .object({
    captureId: ULID,
    url: z
      .string()
      .max(2048)
      .refine((u) => /^https?:\/\//i.test(u), 'url must be http(s)'),
    maxPages: z.number().int().min(1).max(8),
    singlePage: z.boolean(),
    video: z.boolean(),
    workDir: z.string(),
  })
  .refine((d) => d.workDir === `/work/${d.captureId}`, {
    message: 'workDir must equal /work/<captureId>',
    path: ['workDir'],
  });

export type CaptureRequest = z.infer<typeof CaptureRequestSchema>;

// ─── NDJSON response lines ────────────────────────────────────────────────────

// Scalar params only (P4-14 caps them); never UI prose.
export const ParamsSchema = z.record(
  z.string(),
  z.union([z.string(), z.number(), z.boolean(), z.null()]),
);
export type Params = z.infer<typeof ParamsSchema>;

export const EventLineSchema = z.object({
  type: z.literal('event'),
  kind: z.enum(['read', 'artifact', 'info', 'error']),
  code: z.string(),
  params: ParamsSchema.optional(),
});

export const AssetSchema = z.object({
  role: z.string(),
  seq: z.number().int().optional(),
  file: z.string(),
  w: z.number(),
  h: z.number(),
  top: z.number().optional(),
  cssHeight: z.number().optional(),
});
export type Asset = z.infer<typeof AssetSchema>;

export const PageLineSchema = z.object({
  type: z.literal('page'),
  index: z.number().int(),
  url: z.string(),
  pageType: z.string(),
  assets: z.array(AssetSchema),
});

export const FAILURE_CODES = [
  'capture_blocked',
  'timeout',
  'navigation',
  'empty',
  'internal',
] as const;
export const FailureCodeSchema = z.enum(FAILURE_CODES);
export type FailureCode = z.infer<typeof FailureCodeSchema>;

export const DoneLineSchema = z.object({
  type: z.literal('done'),
  manifest: z.literal('manifest.json'),
  durationMs: z.number(),
  peakRssBytes: z.number(),
  bytes: z.number(),
  partial: z.boolean().optional(), // site budget ran out but finished pages kept
});

export const FailedLineSchema = z.object({
  type: z.literal('failed'),
  code: FailureCodeSchema,
});

export const CaptureLineSchema = z.discriminatedUnion('type', [
  EventLineSchema,
  PageLineSchema,
  DoneLineSchema,
  FailedLineSchema,
]);
export type CaptureLine = z.infer<typeof CaptureLineSchema>;

// ─── Manifest ────────────────────────────────────────────────────────────────

// Design metadata is validated loosely (produced by our own assemble.ts); the
// structural fields P4-14 reads (assets, pages, skipped, outcome) are strict.
const LooseObject = z.record(z.string(), z.unknown());

export const ManifestPageSchema = z.object({
  index: z.number().int(),
  url: z.string(),
  requestedUrl: z.string(),
  pageType: z.string(),
  title: z.string(),
  status: z.number().nullable(),
  heightCss: z.number(),
  capped: z.boolean(),
  jacked: z.boolean(),
  qc: z.object({ status: z.string(), reason: z.string() }),
  contentText: z.string(),
  digest: z.object({ h1: z.string(), headings: z.array(z.string()), ctas: z.array(z.string()) }),
  sections: z.array(
    z.object({
      kind: z.string(),
      heading: z.string(),
      top: z.number(),
      cssHeight: z.number(),
    }),
  ),
  assets: z.array(AssetSchema),
});

export const ManifestSchema = z.object({
  schema: z.literal(2),
  version: z.number().int(),
  url: z.string(),
  finalUrl: z.string(),
  domain: z.string(),
  title: z.string(),
  siteName: z.string(),
  description: z.string(),
  lang: z.string().nullable(),
  languages: z.array(z.string()),
  engine: z.string(),
  userAgent: z.string(),
  viewport: z.object({ width: z.number(), height: z.number(), scale: z.number() }),
  palette: z.array(LooseObject),
  scheme: z.string().nullable(),
  contrast: z.union([LooseObject, z.null()]),
  typography: z.object({
    fonts: z.array(LooseObject),
    scale: z.array(LooseObject),
    baseSize: z.number().nullable(),
    ratio: z.number().nullable(),
  }),
  tech: z.array(LooseObject),
  traits: LooseObject,
  awards: z.array(LooseObject),
  awardTags: z.array(z.string()),
  awardEntities: z.array(z.string()),
  jsonldTypes: z.array(z.string()),
  organization: z.unknown(),
  social: z.array(LooseObject),
  credits: z.array(LooseObject),
  webMeta: LooseObject,
  cover: z.object({ role: z.string() }),
  og: z.object({ file: z.string(), w: z.number(), h: z.number() }).nullable(),
  favicon: z.object({ file: z.string(), w: z.number(), h: z.number() }).nullable(),
  pages: z.array(ManifestPageSchema),
  skipped: z.array(z.object({ url: z.string(), reason: z.string() })),
  qc: z.array(z.object({ index: z.number().int(), status: z.string(), reason: z.string() })),
  timeline: z.array(
    z.object({ kind: z.string(), code: z.string(), params: ParamsSchema.optional() }),
  ),
  durationMs: z.number(),
  peakRssBytes: z.number(),
  bytes: z.number(),
  partial: z.boolean(),
});

export type Manifest = z.infer<typeof ManifestSchema>;

// Asset file names in the manifest / page lines must match what P4-14 opens with
// O_NOFOLLOW inside the job dir: a short, safe, role-based name.
export const ASSET_FILE_RE = /^[a-z0-9-]{1,64}\.(webp|png|jpg|mp4)$/;

// The generated JSON Schema of the manifest (capture/protocol.schema.json). zod v4
// emits draft 2020-12.
export function manifestJsonSchema(): unknown {
  return z.toJSONSchema(ManifestSchema, { target: 'draft-2020-12' });
}
