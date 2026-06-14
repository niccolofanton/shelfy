# diag-build.ps1 — riproduce la build che l'updater self-rebuild lancia e cattura
# tutto (percorsi/versioni di node/npm/npx + output completo della build) in un log.
#
#   irm https://raw.githubusercontent.com/niccolofanton/shelfy/main/diag-build.ps1 -OutFile diag-build.ps1
#   .\diag-build.ps1
#
# Poi invia il file che stampa alla fine (Downloads\shelfy-build.log).

param([string]$Version)

$out = Join-Path $env:USERPROFILE 'Downloads\shelfy-build.log'

function W($t) { Add-Content -Path $out -Value $t }

# ── Trova la cartella di rebuild dell'updater ────────────────────────────────
$rebuild = Join-Path $env:APPDATA 'shelfy\rebuild'
$srcDir = $null
if (Test-Path $rebuild) {
  if ($Version) {
    $cand = Join-Path $rebuild "src-$Version"
    if (Test-Path $cand) { $srcDir = $cand }
  }
  if (-not $srcDir) {
    $srcDir = Get-ChildItem $rebuild -Directory -Filter 'src-*' -ErrorAction SilentlyContinue |
              Sort-Object LastWriteTime -Descending | Select-Object -First 1 | ForEach-Object { $_.FullName }
  }
}

Set-Content -Path $out -Value "SHELFY diag-build  $(Get-Date -Format s)"
W "userData: $(Join-Path $env:APPDATA 'shelfy')"
W "rebuild dir: $rebuild"
W "srcDir: $srcDir"
W ""
W "node path: $(where.exe node 2>&1)"
W "npm  path: $(where.exe npm  2>&1)"
W "npx  path: $(where.exe npx  2>&1)"
W "node ver:  $(node -v 2>&1)"
W "npm  ver:  $(npm -v 2>&1)"
W "PATH: $env:PATH"
W ""

if (-not $srcDir) {
  W "ERRORE: nessuna cartella src-* in $rebuild (l'updater non ha ancora estratto i sorgenti)."
  Write-Host "Nessun srcDir trovato. Log: $out" -ForegroundColor Yellow
  return
}

# Versione da passare alla build: quella della cartella (src-<ver>).
if (-not $Version) { $Version = (Split-Path $srcDir -Leaf) -replace '^src-', '' }
W "----- build (build-windows.ps1 -Version $Version) -----"
Write-Host "Compilo $srcDir (v$Version) — l'output va in $out ..." -ForegroundColor Cyan

Push-Location $srcDir
try {
  & powershell.exe -ExecutionPolicy Bypass -NoProfile -File (Join-Path $srcDir 'build-windows.ps1') -Version $Version *>> $out
  W ""
  W "EXIT CODE: $LASTEXITCODE"
} finally {
  Pop-Location
}

Write-Host ""
Write-Host "Fatto. Inviami questo file: $out" -ForegroundColor Green
