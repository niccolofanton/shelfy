# provision-binaries.ps1 — popola <userData>\runtime-bin scaricando i binari sidecar
# (yt-dlp, ffmpeg, llama-server, whisper-server) DIRETTAMENTE dalle fonti upstream.
#
# Serve a sbloccare un'installazione esistente quando il provisioning in-app fallisce
# (es. nessun pack sul feed). NON usa R2: scarica tutto da GitHub / gyan.dev.
#
#   .\provision-binaries.ps1                       # rileva la GPU e CHIEDE quale variante usare
#   .\provision-binaries.ps1 -Auto                 # usa la variante consigliata senza chiedere
#   .\provision-binaries.ps1 -Variant cuda         # forza NVIDIA (CUDA 12.4)
#   .\provision-binaries.ps1 -Variant vulkan       # forza AMD / Intel (Vulkan)
#   .\provision-binaries.ps1 -Variant cpu          # forza solo CPU
#
# La scelta dell'accelerazione e' SEMPRE dell'utente: il rilevamento e' solo un consiglio.
#
# Layout prodotto (quello che i resolver dell'app si aspettano):
#   runtime-bin\bin\yt-dlp.exe
#   runtime-bin\bin\ffmpeg.exe
#   runtime-bin\llama\llama-server.exe   (+ DLL, + CUDA runtime se cuda)
#   runtime-bin\whisper\whisper-server.exe (+ DLL)

param(
  [string]$Variant     = "",                # vuoto = scelta interattiva. Accetta: cuda|nvidia|vulkan|amd|intel|cpu o il nome completo
  [switch]$Auto,                            # usa la variante consigliata dal rilevamento, senza chiedere
  [string]$LlamaBuild  = "b9500",
  [string]$WhisperTag  = "v1.8.5",
  # yt-dlp è PINNATO a un tag (non releases/latest) per download riproducibili e
  # verificabili. Tieni allineato con electron/binaries.js YTDLP_VERSION e build-windows.ps1.
  [string]$YtDlpVersion = "2026.03.17"
)

# SHA256 atteso di yt-dlp.exe per $YtDlpVersion, dal file SHA2-256SUMS della release:
# https://github.com/yt-dlp/yt-dlp/releases/download/<TAG>/SHA2-256SUMS
# Aggiorna questo valore ogni volta che cambi $YtDlpVersion.
$YtDlpExeSha256 = "3db811b366b2da47337d2fcfdfe5bbd9a258dad3f350c54974f005df115a1545"

$ErrorActionPreference = "Stop"

function Log($m) { Write-Host "  $m" -ForegroundColor Cyan }
function Ok($m)  { Write-Host "  $m" -ForegroundColor Green }

# ── Catalogo varianti (ordine = ordine nel menu) ──────────────────────────────
$VARIANTS = [ordered]@{
  'cuda-12.4-x64' = @{
    Title = 'NVIDIA (CUDA 12.4)'
    Desc  = 'GPU NVIDIA GeForce / RTX / Quadro con driver recenti. Prestazioni massime. Scarica anche il runtime CUDA (~300 MB in piu).'
  }
  'vulkan-x64' = @{
    Title = 'AMD / Intel (Vulkan)'
    Desc  = 'GPU AMD Radeon o Intel (Arc / iGPU recenti) tramite Vulkan. Buona accelerazione senza CUDA.'
  }
  'cpu-x64' = @{
    Title = 'Solo CPU'
    Desc  = 'Nessuna accelerazione GPU: compatibile ovunque ma piu lento. Scegli questo se gli altri danno problemi.'
  }
}

# Normalizza un alias (cuda/nvidia/vulkan/amd/intel/cpu) o un nome completo nel nome canonico.
function Normalize-Variant($s) {
  if ([string]::IsNullOrWhiteSpace($s)) { return "" }
  switch -Regex ($s.Trim().ToLower()) {
    '^(cuda|nvidia|cuda-12\.4-x64)$'         { return 'cuda-12.4-x64' }
    '^(vulkan|amd|radeon|intel|vulkan-x64)$' { return 'vulkan-x64' }
    '^(cpu|cpu-x64)$'                        { return 'cpu-x64' }
    default {
      if ($VARIANTS.Contains($s)) { return $s }
      return ""
    }
  }
}

# ── Rilevamento GPU (solo come consiglio) ─────────────────────────────────────
# NOTA: stessa regola di electron/hardware.js → recommendedVariant (la app usa quella
# a runtime; questo e' il percorso di recovery offline). NVIDIA→cuda, AMD/Intel→vulkan,
# altrimenti cpu. Tienile allineate.
function Detect-Recommended {
  $names = @()
  try   { $names = @(Get-CimInstance Win32_VideoController -ErrorAction Stop | Select-Object -ExpandProperty Name) }
  catch { try { $names = @(Get-WmiObject Win32_VideoController -ErrorAction Stop | Select-Object -ExpandProperty Name) } catch {} }
  $joined  = ($names -join ' ')
  $hasNvSmi = [bool](Get-Command nvidia-smi -ErrorAction SilentlyContinue)

  # NVIDIA per nvidia-smi O per nome (geforce/nvidia) — come gpu.cuda || vendor==nvidia in hardware.js.
  if ($hasNvSmi -or $joined -match 'NVIDIA|GeForce') {
    return @{ Variant = 'cuda-12.4-x64'; Gpu = $joined; Why = 'rilevata GPU NVIDIA' }
  }
  if ($joined -match 'AMD|Radeon|Intel\s*Arc|Intel.*Graphics') {
    return @{ Variant = 'vulkan-x64'; Gpu = $joined; Why = 'rilevata GPU AMD/Intel' }
  }
  return @{ Variant = 'cpu-x64'; Gpu = $joined; Why = 'nessuna GPU dedicata rilevata' }
}

# ── Menu interattivo (la scelta resta dell'utente) ────────────────────────────
function Choose-Variant($recommended, $why) {
  $keys = @($VARIANTS.Keys)
  Write-Host ""
  Write-Host "Quale accelerazione vuoi installare?" -ForegroundColor White
  Write-Host ""
  for ($i = 0; $i -lt $keys.Count; $i++) {
    $k = $keys[$i]; $v = $VARIANTS[$k]
    $isRec = ($k -eq $recommended)
    $tag   = if ($isRec) { "   <- consigliato ($why)" } else { "" }
    $col   = if ($isRec) { 'Green' } else { 'Gray' }
    Write-Host ("  [{0}] {1}{2}" -f ($i + 1), $v.Title, $tag) -ForegroundColor $col
    Write-Host ("      {0}" -f $v.Desc) -ForegroundColor DarkGray
  }
  Write-Host ""
  $defIdx = [Array]::IndexOf($keys, $recommended) + 1
  $ans = Read-Host ("Scelta [1-{0}] (Invio = {1}, consigliato)" -f $keys.Count, $defIdx)
  if ([string]::IsNullOrWhiteSpace($ans)) { return $recommended }
  $n = 0
  if ([int]::TryParse($ans, [ref]$n) -and $n -ge 1 -and $n -le $keys.Count) { return $keys[$n - 1] }
  $norm = Normalize-Variant $ans
  if ($norm) { return $norm }
  Write-Host "  Scelta non valida: uso il consiglio ($recommended)." -ForegroundColor Yellow
  return $recommended
}

# ── Determina la variante: -Variant esplicito > -Auto > menu interattivo ───────
$rec = Detect-Recommended

Write-Host ""
Write-Host "SHELFY — provisioning binari" -ForegroundColor White
if ($rec.Gpu) { Write-Host ("Scheda video rilevata: {0}" -f $rec.Gpu) -ForegroundColor DarkGray }
Write-Host ("Il tuo sistema supporta: {0}  (consiglio: {1} — {2})" -f $VARIANTS[$rec.Variant].Title, $rec.Variant, $rec.Why) -ForegroundColor DarkGray

if ($Variant) {
  $norm = Normalize-Variant $Variant
  if (-not $norm) { throw "Variante '$Variant' non riconosciuta. Usa: cuda | vulkan | cpu (o il nome completo)." }
  $Variant = $norm
} elseif ($Auto) {
  $Variant = $rec.Variant
  Write-Host "Modalita -Auto: uso la variante consigliata ($Variant)." -ForegroundColor DarkGray
} else {
  $Variant = Choose-Variant $rec.Variant $rec.Why
}

# ── Individua la cartella userData dell'app (shelfy / SHELFY) ──────────────────
$appData = $env:APPDATA
$dir = Get-ChildItem -Path $appData -Directory -ErrorAction SilentlyContinue |
       Where-Object { $_.Name -ieq 'shelfy' } | Select-Object -First 1
$userData = if ($dir) { $dir.FullName } else { Join-Path $appData 'shelfy' }

$rb       = Join-Path $userData 'runtime-bin'
$binDir   = Join-Path $rb 'bin'
$llamaDir = Join-Path $rb 'llama'
$whispDir = Join-Path $rb 'whisper'
New-Item -ItemType Directory -Force -Path $binDir, $llamaDir, $whispDir | Out-Null

Write-Host ""
Write-Host ("Variante scelta: {0} ({1})" -f $VARIANTS[$Variant].Title, $Variant) -ForegroundColor White
Write-Host "userData: $userData" -ForegroundColor DarkGray
Write-Host ""

# curl.exe (Win10+) è molto più veloce di Invoke-WebRequest e mostra il progresso.
function Dl($url, $dest, $label) {
  Log "Scarico $label ..."
  & curl.exe -L --fail --retry 3 -o $dest $url
  if ($LASTEXITCODE -ne 0) { throw "download fallito ($label): $url" }
}

$tmp = Join-Path $env:TEMP "shelfy-prov"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

# ── yt-dlp.exe ────────────────────────────────────────────────────────────────
$ytdlp = Join-Path $binDir 'yt-dlp.exe'
if (Test-Path $ytdlp) { Ok "yt-dlp.exe gia presente" }
else {
  Dl "https://github.com/yt-dlp/yt-dlp/releases/download/$YtDlpVersion/yt-dlp.exe" $ytdlp "yt-dlp.exe (pin $YtDlpVersion)"
  # Verifica l'integrità contro lo SHA256 pubblicato: su mismatch cancella il file
  # corrotto/manomesso (così non viene eseguito) e interrompe il provisioning.
  $actual = (Get-FileHash -Path $ytdlp -Algorithm SHA256).Hash
  if ($actual -ne $YtDlpExeSha256.ToUpper()) {
    Remove-Item -Force $ytdlp -ErrorAction SilentlyContinue
    throw "yt-dlp.exe SHA256 non corrisponde (yt-dlp $YtDlpVersion): atteso $YtDlpExeSha256, ottenuto $actual"
  }
  Ok "yt-dlp.exe ok (sha256 verificato)"
}

# ── ffmpeg.exe (build statica gyan.dev) ───────────────────────────────────────
$ffmpeg = Join-Path $binDir 'ffmpeg.exe'
if (Test-Path $ffmpeg) { Ok "ffmpeg.exe gia presente" }
else {
  $z = Join-Path $tmp 'ffmpeg.zip'; $x = Join-Path $tmp 'ffmpeg-x'
  Dl "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip" $z "ffmpeg"
  Remove-Item -Recurse -Force $x -ErrorAction SilentlyContinue
  Expand-Archive -Path $z -DestinationPath $x -Force
  $exe = Get-ChildItem -Path $x -Recurse -Filter 'ffmpeg.exe' | Select-Object -First 1
  if (-not $exe) { throw "ffmpeg.exe non trovato nell'archivio" }
  Copy-Item $exe.FullName $ffmpeg -Force
  Ok "ffmpeg.exe ok"
}

# ── llama-server.exe (+ DLL, + CUDA runtime) ──────────────────────────────────
$llamaSrv = Join-Path $llamaDir 'llama-server.exe'
if (Test-Path $llamaSrv) { Ok "llama-server.exe gia presente" }
else {
  $z = Join-Path $tmp 'llama.zip'
  Dl "https://github.com/ggml-org/llama.cpp/releases/download/$LlamaBuild/llama-$LlamaBuild-bin-win-$Variant.zip" $z "llama.cpp ($Variant)"
  Expand-Archive -Path $z -DestinationPath $llamaDir -Force
  if ($Variant -like 'cuda*') {
    $cz = Join-Path $tmp 'cudart.zip'
    Dl "https://github.com/ggml-org/llama.cpp/releases/download/$LlamaBuild/cudart-llama-bin-win-$Variant.zip" $cz "CUDA runtime"
    Expand-Archive -Path $cz -DestinationPath $llamaDir -Force
  }
  if (-not (Test-Path $llamaSrv)) { throw "llama-server.exe non trovato dopo l'estrazione" }
  Ok "llama-server.exe ok"
}

# ── whisper-server.exe (+ DLL) ────────────────────────────────────────────────
$whispSrv = Join-Path $whispDir 'whisper-server.exe'
if (Test-Path $whispSrv) { Ok "whisper-server.exe gia presente" }
else {
  $z = Join-Path $tmp 'whisper.zip'; $x = Join-Path $tmp 'whisper-x'
  Dl "https://github.com/ggml-org/whisper.cpp/releases/download/$WhisperTag/whisper-bin-x64.zip" $z "whisper.cpp"
  Remove-Item -Recurse -Force $x -ErrorAction SilentlyContinue
  Expand-Archive -Path $z -DestinationPath $x -Force
  $srv = Get-ChildItem -Path $x -Recurse -Filter 'whisper-server.exe' | Select-Object -First 1
  if (-not $srv) { throw "whisper-server.exe non trovato nell'archivio" }
  Copy-Item (Join-Path $srv.Directory.FullName '*') $whispDir -Recurse -Force
  Ok "whisper-server.exe ok"
}

Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue

Write-Host ""
Ok "Fatto. Riavvia SHELFY: i binari sono in $rb"
Write-Host ""
