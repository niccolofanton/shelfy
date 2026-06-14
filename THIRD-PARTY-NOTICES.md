# Third-Party Notices

Shelfy is licensed under the Apache License 2.0 (see [LICENSE](LICENSE)). It bundles,
links to, or downloads at runtime a number of third-party components that remain under
their own licenses. This file collects the required attributions. It is provided for
informational purposes and does not modify the licenses of the components listed.

> ⚠️ **GPL component.** The `ffmpeg` binary that Shelfy downloads/bundles is distributed
> under the GPL. Distributing it imposes the GPL obligations on **that binary**
> (notably making the corresponding source available). Shelfy invokes `ffmpeg` and `yt-dlp`
> as **separate processes** (not linked), so the GPL does not extend to Shelfy's own code.

## Bundled / linked npm dependencies

| Component | License | Notes |
|-----------|---------|-------|
| react, react-dom | MIT | © Meta Platforms, Inc. and affiliates |
| @tanstack/react-virtual | MIT | © Tanner Linsley |
| better-sqlite3 | MIT | native SQLite binding |
| electron-updater (electron-builder) | MIT | |
| lucide-react | ISC | icon set |
| pdfjs-dist | Apache-2.0 | © Mozilla Foundation — see upstream `NOTICE` |
| playwright-core | Apache-2.0 | © Microsoft — see upstream `NOTICE` |
| ffmpeg-static | GPL-3.0-or-later | wrapper that fetches a GPL `ffmpeg` build |
| electron | MIT | © GitHub / OpenJS Foundation |

A complete, machine-generated manifest of every transitive npm dependency and its license
can be produced with `npx license-checker --production --summary` and should be regenerated
before each release.

## Tools and binaries downloaded at runtime

These are fetched from their official upstreams onto the user's machine (not redistributed
inside the source repository):

| Component | License | Upstream |
|-----------|---------|----------|
| FFmpeg (`ffmpeg`) | GPL (the builds Shelfy uses are GPL) | https://ffmpeg.org — Windows: gyan.dev "essentials"; macOS/Linux: ffmpeg-static (npm), redistributed in the mini-pack |
| yt-dlp | Unlicense (public domain) | https://github.com/yt-dlp/yt-dlp |
| llama.cpp (`llama-server`) | MIT | https://github.com/ggml-org/llama.cpp |
| whisper.cpp | MIT | https://github.com/ggml-org/whisper.cpp |

## AI model weights (downloaded at runtime)

Model weights are downloaded by the user on demand. Their licenses differ and some are
**not** OSI-approved:

| Model | License | Source |
|-------|---------|--------|
| Qwen3-VL-4B / 8B-Instruct (GGUF) | Apache-2.0 | https://huggingface.co/Qwen |
| Gemma 3 / 4 (GGUF) | **Gemma Terms of Use** (Google, non-OSI, use restrictions apply) | https://ai.google.dev/gemma/terms |
| multilingual-e5-small (GGUF) | MIT | https://huggingface.co/intfloat/multilingual-e5-small |
| whisper ggml models | MIT | https://github.com/ggml-org/whisper.cpp |

Users selecting the Gemma presets are bound by Google's Gemma Terms of Use and Prohibited
Use Policy. Shelfy does not redistribute these weights.

---

If you believe an attribution is missing or incorrect, please open an issue.
