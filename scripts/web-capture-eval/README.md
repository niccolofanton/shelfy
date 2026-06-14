# web-capture-eval — harness cattura screenshot (tab "AI Website")

Verifica oggettiva che la pipeline di cattura (`electron/webcapture.js`) gestisca i
siti-reference creativi: smooth-scroll virtualizzato, hero WebGL/Three.js, pinned
section, lazy-load.

## Esecuzione

```bash
npm run eval:capture                  # tutti i casi
CAP_ONLY=scrollsmoother npm run eval:capture   # un solo caso (match su nome/file)
```

Gira dentro Electron reale (serve `BrowserWindow` offscreen). Exit code 0 = tutti PASS.

## Come funziona

- Serve `fixtures/*.html` su un server HTTP locale, raggiunto via
  `host-resolver-rules` (`capture.test` → `127.0.0.1`) per passare l'SSRF-guard di
  `net-safety` **senza** modificarlo.
- Chiama `webcapture.capturePage()` reale su ogni fixture e misura:
  - **coverage** = altezza catturata / altezza reale (ground-truth in `cases.json`).
    Smaschera il troncamento da scroll virtualizzato (pre-fix ≈ 0.18, post-fix ≈ 1.0).
  - **luma / variance** = luminanza media e deviazione std (ffmpeg, gray 64×64).
    Smaschera frame neri/bianchi/uniformi.
  - **heroLuma** = luminanza della fascia hero (top 900px). Smaschera il canvas WebGL nero.

## Fixture (deterministiche, offline)

| File | Failure mode | Atteso |
|---|---|---|
| `native-tall.html` | controllo scroll nativo | coverage ~1 |
| `lenis.html` | controllo Lenis (scroll nativo) — non deve regredire | coverage ~1 |
| `scrollsmoother.html` | GSAP ScrollSmoother (matrix3d su `#smooth-content`) | coverage ~1 dopo neutralizzazione |
| `locomotive.html` | Locomotive v3 (`[data-scroll-container]` translate) | coverage ~1 dopo neutralizzazione |
| `webgl-hero.html` | hero `<canvas>` WebGL fisso | hero non nero + canvas non collassato |
| `pinned.html` | pinned section (sticky / pin-spacer) | coverage ~1 |

Le fixture riproducono la **firma DOM** di ciascuna libreria (i marker che il fix
rileva), senza caricare le librerie reali → nessuna rete, risultati ripetibili.
Per una validazione su siti veri, vedi i caveat nel documento di analisi.

## Output

Stampa PASS/FAIL per caso + metriche; scrive il dettaglio in `last-run.json`.
