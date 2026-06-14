# build-windows.ps1 — builds SHELFY into a (lightweight) Windows installer.
# The sidecar binaries (yt-dlp/ffmpeg/llama/whisper) are NO LONGER bundled in the
# installer: the app downloads them at runtime. So a normal build is small and fast.
#
#   .\build-windows.ps1                 # build the installer
#   .\build-windows.ps1 -Version X.Y.Z  # build a specific version
#
# Releasing is tag-driven via GitHub Actions (.github/workflows/release.yml); the
# -Publish / -BinaryPack flags (R2 uploads) are removed and now exit with a notice.
#
# Stand-alone: if run in an empty folder (no package.json) it fetches the sources
# from the GitHub Releases feed first, so you can just download this one .ps1 and run it.

param(
  [string]$LlamaBuild   = "b9500",
  [string]$LlamaVariant = "cuda-12.4-x64",  # cuda-12.4-x64 (NVIDIA) | vulkan-x64 (AMD/Intel) | cpu-x64
  # yt-dlp is PINNED to a tag (not releases/latest) for reproducible, tamper-evident
  # builds. Must match electron/binaries.js YTDLP_VERSION and provision-binaries.ps1.
  [string]$YtDlpVersion = "2026.03.17",
  # ffmpeg comes from gyan.dev's ROLLING "ffmpeg-release-essentials.zip" (no version in
  # the URL, content changes per build), so there is no stable tag to pin against yet —
  # mirrors the unresolved TODO(supply-chain) in electron/binaries.js. Until the source
  # is switched to a versioned one, pass -FfmpegExeSha256 <hash> of a known-good build
  # to ENABLE verification (the extracted ffmpeg.exe is then checked and the build aborts
  # on mismatch). Empty = "not pinned yet": download proceeds with a loud WARNING.
  [string]$FfmpegExeSha256 = "",
  [switch]$Publish,                         # build the installer + upload to R2
  [switch]$BinaryPack,                      # instead: download sidecar binaries + upload a pack to R2
  [string]$Version = ""                     # override the version for this build (doesn't touch package.json)
)

# Expected SHA256 of yt-dlp.exe for $YtDlpVersion, from the release's SHA2-256SUMS:
# https://github.com/yt-dlp/yt-dlp/releases/download/<TAG>/SHA2-256SUMS
# Refresh this whenever $YtDlpVersion changes.
$YtDlpExeSha256 = "3db811b366b2da47337d2fcfdfe5bbd9a258dad3f350c54974f005df115a1545"

$FEED_BASE = "https://github.com/niccolofanton/shelfy/releases/latest/download"

$ErrorActionPreference = "Stop"
$ProgressPreference     = "Continue"   # show progress bars (downloads, Expand-Archive)
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12  # for older Windows PowerShell
$root = $PSScriptRoot
Set-Location $root

function Log($msg)  { Write-Host "[build] $msg" -ForegroundColor Cyan }
function Ok($msg)   { Write-Host "[ ok  ] $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "[warn ] $msg" -ForegroundColor Yellow }

# Run a native command and FAIL the script if it returns a non-zero exit code.
# (PowerShell does not stop on native exit codes by default, which previously
# let failed steps slip by unnoticed.)
function Run($file, [string[]]$cmdArgs) {
  Log "> $file $($cmdArgs -join ' ')"
  & $file @cmdArgs
  if ($LASTEXITCODE -ne 0) { throw "Command failed (exit $LASTEXITCODE): $file $($cmdArgs -join ' ')" }
}

# Streamed download with a live progress bar (percent, MB downloaded, speed).
# Invoke-WebRequest's own bar is unreliable, so we read the stream ourselves.
function Get-FileWithProgress($Url, $OutFile, $Activity) {
  $req = [System.Net.WebRequest]::Create($Url)
  $req.UserAgent = "shelfy-build"
  $req.AllowAutoRedirect = $true
  $resp = $req.GetResponse()
  $total = [int64]$resp.ContentLength
  $in  = $resp.GetResponseStream()
  # Download to a temporary .part file and only rename to $OutFile once the stream
  # completes. An interrupted download (network drop, Ctrl-C, disk full) would
  # otherwise leave a truncated $OutFile that a later Test-Path treats as "already
  # present" and never re-fetches (the yt-dlp.exe SHA256 check is the only guard).
  $partFile = "$OutFile.part"
  $out = [System.IO.File]::Create($partFile)
  $buffer = New-Object byte[] (1MB)
  $totalRead = [int64]0
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $lastTick = 0
  $completed = $false
  try {
    while (($read = $in.Read($buffer, 0, $buffer.Length)) -gt 0) {
      $out.Write($buffer, 0, $read)
      $totalRead += $read
      # Throttle UI updates to ~10/s so the bar stays smooth, not flickery.
      if ($sw.ElapsedMilliseconds - $lastTick -ge 100) {
        $lastTick = $sw.ElapsedMilliseconds
        $mb    = [math]::Round($totalRead / 1MB, 1)
        $secs  = [math]::Max($sw.Elapsed.TotalSeconds, 0.001)
        $speed = [math]::Round($mb / $secs, 1)
        if ($total -gt 0) {
          $pct   = [math]::Min([int](($totalRead / $total) * 100), 100)
          $totMb = [math]::Round($total / 1MB, 1)
          Write-Progress -Activity $Activity -Status "$mb / $totMb MB  -  $speed MB/s" -PercentComplete $pct
        } else {
          Write-Progress -Activity $Activity -Status "$mb MB  -  $speed MB/s"
        }
      }
    }
    $completed = $true
  } finally {
    Write-Progress -Activity $Activity -Completed
    $out.Close(); $in.Close(); $resp.Close()
    if ($completed) {
      # Atomic publish: replace any existing $OutFile only after a full download.
      Move-Item -Path $partFile -Destination $OutFile -Force
    } else {
      # Failed/interrupted mid-stream: drop the partial so it can't be mistaken for
      # a complete file on the next run.
      Remove-Item -Force $partFile -ErrorAction SilentlyContinue
    }
  }
}

$started = Get-Date
Log "SHELFY Windows build starting in $root"

# ── 0. Stand-alone mode: fetch sources from the GitHub feed if run outside the repo ─
if (-not (Test-Path (Join-Path $root "package.json"))) {
  Log "No sources here - downloading them from GitHub Releases ..."
  $srcZip = Join-Path $env:TEMP "shelfy-src.zip"
  # Resolve the versioned source archive from the channel manifest — no separate
  # *-latest.zip alias needed (source.json already names the right zip).
  $srcName = (Invoke-RestMethod "$FEED_BASE/source.json").zip
  Get-FileWithProgress "$FEED_BASE/$srcName" $srcZip "Downloading sources"
  Expand-Archive -Path $srcZip -DestinationPath $root -Force
  Remove-Item -Force $srcZip
  Ok "Sources downloaded"
}

# ── 1. Toolchain check ────────────────────────────────────────────────────────
Log "Checking Node.js / npm ..."
$node = (Get-Command node -ErrorAction SilentlyContinue)
if (-not $node) { throw "Node.js not found in PATH. Install Node 20 LTS from https://nodejs.org and retry." }
$nodeVer = (node -v) -replace '^v',''
$nodeMajor = [int]($nodeVer.Split('.')[0])
# Node 22 LTS minimum: better-sqlite3 12.x ships no prebuilt for the Node runtime
# below ABI v127 (Node 22), so an older Node makes `npm install` compile it from
# source via node-gyp — which needs Visual Studio C++ and fails on a clean box.
# Keep in sync with install-shelfy.ps1 ($MinNodeMajor).
if ($nodeMajor -lt 22) {
  throw "Node $nodeVer is too old. This project needs Node 22 LTS or newer: better-sqlite3 has no prebuilt binary for older Node, so 'npm install' would try to compile it with Visual Studio (not installed). Install Node 22 LTS from https://nodejs.org and retry."
}
Ok "node v$nodeVer / npm $(npm -v)"

# ── 2. Dependencies + native modules for Electron ──────────────────────────────
Log "Installing npm dependencies (also fetches the Windows ffmpeg) ..."
Run "npm" @("install")
Ok "Dependencies installed"

# better-sqlite3 is a native module: it must match Electron's ABI, not Node's.
# install-app-deps downloads the matching prebuilt binary (no Visual Studio /
# node-gyp compiler needed) — this is what failed silently before.
Log "Fetching native modules for Electron's ABI (better-sqlite3) ..."
Run "npx" @("electron-builder", "install-app-deps")
Ok "Native module ready"

# ── Binary pack mode (-BinaryPack) ─────────────────────────────────────────────
# Download the sidecar binaries and upload a per-platform pack to R2. These are
# NOT needed to build the installer (the app downloads them at runtime); this
# block runs only when refreshing the packs.
if ($BinaryPack) {
  Log "[-BinaryPack] obsoleto: Windows scarica i sidecar direttamente da upstream (electron/binaries.js); i mini-pack mac/Linux li produce la CI (scripts/make-binary-packs.ts). Niente piu' upload R2."
  exit 1

# ── 3. yt-dlp.exe ──────────────────────────────────────────────────────────────
$binDir = Join-Path $root "bin"
$ytDlp  = Join-Path $binDir "yt-dlp.exe"
if (Test-Path $ytDlp) {
  Ok "yt-dlp.exe already present, skipping download"
} else {
  Log "Downloading yt-dlp.exe (pinned $YtDlpVersion) ..."
  New-Item -ItemType Directory -Force -Path $binDir | Out-Null
  Get-FileWithProgress "https://github.com/yt-dlp/yt-dlp/releases/download/$YtDlpVersion/yt-dlp.exe" $ytDlp "Downloading yt-dlp.exe"
  # Verify the pinned artifact against the published SHA256; on mismatch delete the
  # bad file (so it can't be packed/shipped) and abort the build.
  $actual = (Get-FileHash -Path $ytDlp -Algorithm SHA256).Hash
  if ($actual -ne $YtDlpExeSha256.ToUpper()) {
    Remove-Item -Force $ytDlp -ErrorAction SilentlyContinue
    throw "yt-dlp.exe SHA256 mismatch (yt-dlp $YtDlpVersion): expected $YtDlpExeSha256, got $actual"
  }
  Ok "yt-dlp.exe downloaded to bin\ (sha256 verified)"
}

# ── 3b. ffmpeg.exe ─────────────────────────────────────────────────────────────
$ffmpeg = Join-Path $binDir "ffmpeg.exe"
if (Test-Path $ffmpeg) {
  Ok "ffmpeg.exe already present, skipping download"
} else {
  Log "Downloading ffmpeg.exe (extracting from gyan.dev essentials build) ..."
  New-Item -ItemType Directory -Force -Path $binDir | Out-Null
  $zip = Join-Path $env:TEMP "ffmpeg-essentials.zip"
  $tmp = Join-Path $env:TEMP "ffmpeg-extract"
  # NOTE: the gyan.dev URL is rolling (no version, content changes per build), so two
  # runs on different days can pack different ffmpeg binaries. Until a versioned source
  # is adopted we can still make a given build tamper-evident by pinning the extracted
  # ffmpeg.exe via -FfmpegExeSha256 — verified below, mirroring the yt-dlp block above.
  Get-FileWithProgress "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip" $zip "Downloading ffmpeg"
  if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
  Expand-Archive -Path $zip -DestinationPath $tmp -Force
  $exe = Get-ChildItem -Path $tmp -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1
  if (-not $exe) { throw "ffmpeg.exe not found inside the ffmpeg archive" }
  # Verify integrity against the pinned hash BEFORE the binary lands in bin\ (and gets
  # packed/uploaded). Empty pin = "not pinned yet": warn loudly but proceed, preserving
  # current behaviour until a real hash is supplied.
  $ffActual = (Get-FileHash -Path $exe.FullName -Algorithm SHA256).Hash
  if ($FfmpegExeSha256) {
    if ($ffActual -ne $FfmpegExeSha256.ToUpper()) {
      Remove-Item -Force $zip -ErrorAction SilentlyContinue
      Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
      throw "ffmpeg.exe SHA256 mismatch: expected $FfmpegExeSha256, got $ffActual"
    }
  } else {
    Warn "ffmpeg.exe downloaded WITHOUT integrity verification (no hash pinned; gyan.dev source is mutable). sha256: $ffActual - pass -FfmpegExeSha256 to make this build tamper-evident."
  }
  Copy-Item $exe.FullName -Destination $ffmpeg -Force
  Remove-Item -Force $zip
  Remove-Item -Recurse -Force $tmp
  if ($FfmpegExeSha256) {
    Ok "ffmpeg.exe downloaded to bin\ (sha256 verified)"
  } else {
    Ok "ffmpeg.exe downloaded to bin\"
  }
}

# ── 4. llama.cpp Windows binaries ──────────────────────────────────────────────
# The folder name is fixed (the app resolves it via resolveLlamaServer), so we keep
# a .variant marker inside it to know which build (cpu/cuda/vulkan) is on disk. If it
# doesn't match the requested -LlamaVariant we wipe and re-download — otherwise a
# previously-downloaded CPU build would silently win and the GPU would go unused.
$llamaDir     = Join-Path $root ".vlm\llama-b9500-win"
$llamaSrv     = Join-Path $llamaDir "llama-server.exe"
$variantMark  = Join-Path $llamaDir ".variant"
$wantVariant  = "$LlamaBuild/$LlamaVariant"
$haveVariant  = if (Test-Path $variantMark) { (Get-Content $variantMark -Raw).Trim() } else { "" }
if ((Test-Path $llamaSrv) -and ($haveVariant -ne $wantVariant)) {
  Warn "Existing llama build is '$haveVariant' but '$wantVariant' was requested - re-downloading."
  Remove-Item -Recurse -Force $llamaDir
}
# A CUDA build is only complete if the runtime DLLs are present too. An older
# run (or one before the cudart fix) may have left llama-server.exe without
# them — force a re-download so the GPU build isn't shipped half-broken.
if ((Test-Path $llamaSrv) -and ($LlamaVariant -like "cuda*") -and
    -not (Get-ChildItem -Path $llamaDir -Filter "cudart64_*.dll" -ErrorAction SilentlyContinue)) {
  Warn "CUDA build present but the CUDA runtime DLLs are missing - re-downloading."
  Remove-Item -Recurse -Force $llamaDir
}
if (Test-Path $llamaSrv) {
  Ok "llama-server.exe already present ($haveVariant), skipping download"
} else {
  $asset = "llama-$LlamaBuild-bin-win-$LlamaVariant.zip"
  $url   = "https://github.com/ggml-org/llama.cpp/releases/download/$LlamaBuild/$asset"
  $zip   = Join-Path $env:TEMP $asset
  $tmp   = Join-Path $env:TEMP "llama-extract-$LlamaBuild"
  Log "Downloading llama.cpp ($asset) ..."
  try {
    Get-FileWithProgress $url $zip "Downloading llama.cpp ($LlamaVariant)"
  } catch {
    throw "Failed to download $url`nCheck the release page https://github.com/ggml-org/llama.cpp/releases/tag/$LlamaBuild and re-run with the correct -LlamaVariant (e.g. cpu-x64, vulkan-x64, cuda-12.4-x64)."
  }
  Log "Extracting ..."
  if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
  Expand-Archive -Path $zip -DestinationPath $tmp -Force

  # The exe may sit at the archive root or inside a subfolder — find it.
  $srv = Get-ChildItem -Path $tmp -Recurse -Filter "llama-server.exe" | Select-Object -First 1
  if (-not $srv) { throw "llama-server.exe not found inside $asset" }

  New-Item -ItemType Directory -Force -Path $llamaDir | Out-Null
  Copy-Item -Path (Join-Path $srv.DirectoryName "*") -Destination $llamaDir -Recurse -Force
  Remove-Item -Force $zip
  Remove-Item -Recurse -Force $tmp

  # CUDA build: the main archive does NOT contain the CUDA runtime DLLs
  # (cudart64_*, cublas64_*, cublasLt64_*). They ship in a SEPARATE asset
  # (cudart-llama-bin-win-<variant>.zip). Without them llama-server.exe either
  # fails to start or silently falls back to CPU on a machine that lacks the
  # CUDA toolkit in PATH — which is exactly why a GPU box can run inference at
  # CPU speed. Bundle them next to the exe so the GPU is actually used.
  if ($LlamaVariant -like "cuda*") {
    $cudartAsset = "cudart-llama-bin-win-$LlamaVariant.zip"
    $cudartUrl   = "https://github.com/ggml-org/llama.cpp/releases/download/$LlamaBuild/$cudartAsset"
    $cudartZip   = Join-Path $env:TEMP $cudartAsset
    $cudartTmp   = Join-Path $env:TEMP "cudart-extract-$LlamaBuild"
    Log "Downloading CUDA runtime ($cudartAsset) ..."
    try {
      Get-FileWithProgress $cudartUrl $cudartZip "Downloading CUDA runtime"
    } catch {
      throw "Failed to download $cudartUrl`nThe CUDA runtime DLLs are required for GPU inference. Check https://github.com/ggml-org/llama.cpp/releases/tag/$LlamaBuild for the matching cudart asset."
    }
    if (Test-Path $cudartTmp) { Remove-Item -Recurse -Force $cudartTmp }
    Expand-Archive -Path $cudartZip -DestinationPath $cudartTmp -Force
    # Copy every DLL from the archive next to llama-server.exe.
    Get-ChildItem -Path $cudartTmp -Recurse -Filter "*.dll" | ForEach-Object {
      Copy-Item -Path $_.FullName -Destination $llamaDir -Force
    }
    Remove-Item -Force $cudartZip
    Remove-Item -Recurse -Force $cudartTmp
    Ok "CUDA runtime DLLs placed alongside llama-server.exe"
  }

  Set-Content -Path $variantMark -Value $wantVariant -NoNewline
  Ok "llama.cpp binaries placed in .vlm\llama-b9500-win\ ($LlamaVariant)"
  if ($LlamaVariant -like "cuda*") {
    Warn "CUDA build selected: the target machine needs a recent NVIDIA driver. The app spawns llama-server with -ngl 99 (full GPU offload)."
  } else {
    Warn "'$LlamaVariant' selected (no NVIDIA GPU offload). For an NVIDIA card use the default cuda-12.4-x64."
  }
}

  # Build and upload the binary pack for this platform/variant, then stop.
  $packVariant = if ($LlamaVariant -like "cuda*") { "cuda" } elseif ($LlamaVariant -like "vulkan*") { "vulkan" } else { "cpu" }
  Log "Creating binary pack (variant $packVariant) and uploading to R2 ..."
  Run "npx" @("tsx", "scripts/make-binary-packs.ts", "--variant", $packVariant)
  Ok "Binary pack uploaded to R2"
  exit 0
}  # end -BinaryPack

# ── 5. Build the installer (lightweight: no sidecar binaries inside) ───────────
if ($Publish) {
  Log "[-Publish] obsoleto: il rilascio e' ora tag-driven via GitHub Actions (.github/workflows/release.yml). Per un installer locale one-off, builda senza -Publish."
  exit 1
} else {
  if ($Version) {
    Log "Building renderer + Windows installer v$Version ..."
    # This -Version branch bypasses the npm build script, so replicate it here:
    # esbuild the main process into dist-electron/ (the new app entry), run
    # prepare-playwright to bundle build/ms-playwright (chromium-headless-shell),
    # then package with tsx registered so electron-builder can load the .ts
    # afterPack/afterSign hooks — otherwise fuses/signing silently no-op.
    Run "npx" @("tsx", "build/prepare-playwright.ts")
    Run "npx" @("tsx", "build/esbuild-electron.ts")
    Run "npx" @("vite", "build")
    $env:NODE_OPTIONS = "--import=tsx"
    Run "npx" @("electron-builder", "-c.extraMetadata.version=$Version")
    Remove-Item Env:\NODE_OPTIONS
  } else {
    Log "Building renderer + Windows installer (vite build + electron-builder) ..."
    Run "npm" @("run", "build")
  }
  Ok "Build finished"
}

$elapsed = [int]((Get-Date) - $started).TotalSeconds
$out = Join-Path $root "release"
Write-Host ""
Ok "Done in ${elapsed}s. Executable is in: $out"
Get-ChildItem $out -Filter "*.exe" -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "       -> $($_.Name)" -ForegroundColor Green }
Write-Host ""
Write-Host "The AI vision model (~3 GB) is NOT bundled: it downloads automatically on first use of the analyzer." -ForegroundColor DarkGray
