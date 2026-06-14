# install-shelfy.ps1 — one-shot Windows bootstrap for non-developers.
#
# Ensures Node.js 22+ is present (installs it via winget, falling back to the
# official MSI), downloads the SHELFY build script from GitHub (the sources come
# from the GitHub Releases feed), compiles the installer with build-windows.ps1,
# then launches it. Everything happens in a temp working folder.
#
# Designed for remote execution — the friend just pastes ONE line in PowerShell:
#
#   irm https://raw.githubusercontent.com/niccolofanton/shelfy/main/install-shelfy.ps1 | iex
#
# For options, download it first and run with params:
#   .\install-shelfy.ps1 -NoInstall              # build only, don't launch the installer
#   .\install-shelfy.ps1 -LlamaVariant vulkan-x64 # pass a GPU variant through to the build
#   .\install-shelfy.ps1 -Version 1.3.1          # pin a specific source version

param(
  [string]$Version = "",          # default: latest source on GitHub (package.json version)
  [string]$LlamaVariant = "",     # passthrough to build-windows.ps1 (GPU variant)
  [switch]$NoInstall              # build only; don't run the resulting installer
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$REPO_RAW = "https://raw.githubusercontent.com/niccolofanton/shelfy/main"
$work = Join-Path $env:TEMP "shelfy-build"
New-Item -ItemType Directory -Force -Path $work | Out-Null
Set-Location $work

function Log($m)  { Write-Host "[install] $m" -ForegroundColor Cyan }
function Ok($m)   { Write-Host "[  ok   ] $m" -ForegroundColor Green }
function Warn($m) { Write-Host "[ warn  ] $m" -ForegroundColor Yellow }

# Allow .ps1 execution for THIS session: the default ExecutionPolicy on Windows
# client is `Restricted`, which would let this one-liner run (it's piped to iex,
# i.e. in-memory) but then block `& $build` below — build-windows.ps1 is a file on
# disk, so its execution IS policy-checked, failing with "running scripts is
# disabled on this system". `-Scope Process` is session-only, needs no admin, and
# can't be set by a GPO that forces AllSigned/Restricted machine-wide (rare).
try {
  Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass -Force -ErrorAction Stop
} catch {
  Warn "Impossibile sbloccare l'esecuzione degli script (policy aziendale?). Se la build fallisce con 'running scripts is disabled', esegui prima: Set-ExecutionPolicy -Scope CurrentUser RemoteSigned"
}

# Minimum Node major. better-sqlite3 (our native dep) ships NO prebuilt binary
# for the Node runtime below this ABI, so an older Node forces a source compile
# via node-gyp — which needs Visual Studio C++ and fails on a normal user's PC.
# Node 22 LTS has a published prebuild (node-v127), so `npm install` stays
# compiler-free. Keep in sync with build-windows.ps1.
$MinNodeMajor = 22

# Read the installed Node major version (0 if missing / unparseable).
function Get-NodeMajor {
  try {
    $v = (& node -v) 2>$null
    if ($v -match 'v(\d+)\.') { return [int]$Matches[1] }
  } catch {}
  return 0
}

# Re-read PATH from the registry so a freshly-installed Node is visible in THIS
# session (installers update the persisted env, not the current process).
function Update-SessionPath {
  $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
  $user    = [Environment]::GetEnvironmentVariable('Path', 'User')
  $env:Path = (@($machine, $user) | Where-Object { $_ }) -join ';'
}

Log "Cartella di lavoro: $work"

# ── 1. Ensure Node.js 20+ ────────────────────────────────────────────────────
$major = Get-NodeMajor
if ($major -ge $MinNodeMajor) {
  Ok "Node.js già presente (v$major)"
} else {
  if ($major -gt 0) { Warn "Node v$major troppo vecchio (serve $MinNodeMajor+) — aggiorno" }
  else { Log "Node.js non trovato — installazione in corso" }

  $installed = $false

  # Preferred: winget (handles its own UAC elevation for the MSI).
  if (Get-Command winget -ErrorAction SilentlyContinue) {
    Log "Installo Node.js LTS via winget (conferma la richiesta UAC se compare) ..."
    try {
      winget install -e --id OpenJS.NodeJS.LTS --accept-source-agreements --accept-package-agreements --silent
    } catch { Warn "winget ha riportato un errore: $($_.Exception.Message)" }
    Update-SessionPath
    if ((Get-NodeMajor) -ge $MinNodeMajor) { $installed = $true; Ok "Node.js installato via winget" }
  }

  # Fallback: download the official LTS MSI and run it elevated.
  if (-not $installed) {
    $nodeVer = "v22.11.0"  # current LTS; bump if it 404s
    $msi = Join-Path $work "node-lts-x64.msi"
    Warn "winget non disponibile/fallito — scarico l'installer MSI di Node $nodeVer ..."
    try {
      Invoke-WebRequest "https://nodejs.org/dist/$nodeVer/node-$nodeVer-x64.msi" -OutFile $msi -UseBasicParsing
    } catch {
      throw "Download di Node $nodeVer non riuscito ($($_.Exception.Message)). Quella versione potrebbe non essere piu disponibile: installa Node 22 LTS manualmente da https://nodejs.org/, poi CHIUDI e riapri PowerShell e rilancia questo comando."
    }
    Log "Avvio l'installer di Node (accetta la richiesta UAC) ..."
    Start-Process msiexec.exe -ArgumentList "/i `"$msi`" /passive /norestart" -Wait -Verb RunAs
    Update-SessionPath
    if ((Get-NodeMajor) -ge $MinNodeMajor) { $installed = $true; Ok "Node.js installato (MSI)" }
  }

  if (-not $installed) {
    throw "Impossibile installare Node.js in automatico. Installa Node 22 LTS da https://nodejs.org, CHIUDI e riapri PowerShell, poi rilancia questo comando."
  }
}

# ── 2. Fetch the build script from GitHub ────────────────────────────────────
Log "Scarico lo script di build ..."
$build = Join-Path $work "build-windows.ps1"
Invoke-WebRequest "$REPO_RAW/build-windows.ps1" -OutFile $build -UseBasicParsing

# ── 3. Build the installer (build-windows.ps1 fetches the sources from R2,
#       installs deps and runs electron-builder; runs in THIS folder) ──────────
Log "Compilo SHELFY — la prima volta richiede qualche minuto ..."
$buildArgs = @()
if ($Version)       { $buildArgs += @('-Version', $Version) }
if ($LlamaVariant)  { $buildArgs += @('-LlamaVariant', $LlamaVariant) }
& $build @buildArgs
if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { throw "build-windows.ps1 fallito (exit $LASTEXITCODE) — controlla i log sopra." }

# ── 4. Locate + launch the installer ─────────────────────────────────────────
$exe = Get-ChildItem (Join-Path $work "release") -Filter "SHELFY-Setup-*.exe" -ErrorAction SilentlyContinue |
       Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $exe) { throw "Build completata ma non trovo l'installer in release\. Controlla i log sopra." }
Ok "Installer pronto: $($exe.FullName)"

if ($NoInstall) {
  Log "Avvio saltato (-NoInstall). Per installare esegui: $($exe.FullName)"
} else {
  Log "Avvio l'installer di SHELFY ..."
  Start-Process $exe.FullName
  Ok "Segui l'installazione a schermo. Al primo avvio l'app scaricherà i componenti runtime e (a richiesta) il modello AI."
}
