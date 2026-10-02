# Provider AI per la ricerca immagini

La ricerca immagini usa tag e descrizioni salvati in Shelfy. Il toggle nella chat sceglie il modello che interpreta la richiesta. Se il provider selezionato dichiara `vision: true`, anche l'analisi di immagini, video e screenshot usa quel modello remoto: i frame locali sono ridimensionati e inviati come immagini nella richiesta OpenAI. Se il nodo è configurato ma non risponde, Shelfy non passa da solo al modello locale: le analisi restano in coda e un banner propone **Riprova**, **Usa modelli locali** (fino al riavvio dell'app) oppure **Scarica modello locale** se manca. La raggiungibilità è verificata con `GET /v1/models` all'avvio, ogni 15 secondi mentre il nodo è giù e ogni 60 secondi mentre risponde; quando torna disponibile la coda riparte da sola.

Shelfy rileva automaticamente i server `llamaSettings.servers` di Pi da `~/.pi/agent/settings.json`. Usa `defaultProvider` e `defaultModel` per identificare il modello e legge l'eventuale API key da `~/.pi/agent/auth.json` nel solo processo principale. Il toggle è salvato in `<userData>/ai-providers.json`; le credenziali non sono copiate lì.

In **Impostazioni → Intelligenza artificiale → Provider AI remoti** puoi aggiungere o rimuovere provider, cambiare Base URL e model ID, scegliere il modello predefinito per chat e ricerca e quello per la visione, e inserire o sostituire la chiave API. Il campo della chiave resta vuoto quando riapri la schermata: Shelfy mostra soltanto se una chiave è disponibile. Su macOS la chiave inserita viene salvata nel Portachiavi con il nome indicato in `Nome segreto`. Il Portachiavi ha precedenza su una variabile d'ambiente omonima, che resta disponibile come fallback.

Il file `ai-providers.json` può anche essere modificato manualmente. Per aggiungere altri server OpenAI compatibili:

```json
{
  "searchProvider": "local",
  "providers": [
    {
      "id": "mio-nodo",
      "name": "Mio nodo",
      "baseUrl": "https://ai.example.com",
      "model": "nome-modello",
      "apiKeyEnv": "SHELFY_AI_MIO_NODO_KEY",
      "vision": true
    }
  ]
}
```

`apiKeyEnv` è opzionale. La chiave va nella variabile d'ambiente indicata o, su macOS, nel segreto omonimo del Portachiavi dell'utente. In alternativa, `apiKeyPiProvider` può riferirsi all'ID di una chiave già salvata in `~/.pi/agent/auth.json`; Shelfy la legge nel solo processo principale e non la copia nel suo file di configurazione. `baseUrl` accetta sia la radice del server sia la forma terminante in `/v1`. L'endpoint usato è `/v1/chat/completions`, con l'ID `model` esplicito in ogni richiesta. La UI riceve soltanto la configurazione pubblica e lo stato della chiave, mai il suo valore.

Per Ornith, la configurazione locale usa due voci sulla stessa URL Tailscale e legge solo `ORNITH_API_KEY` dall'ambiente o dal Portachiavi:

```json
{
  "searchProvider": "custom:ornith-qwen",
  "visionProvider": "custom:ornith-qwen",
  "providers": [
    { "id": "ornith-qwen", "name": "Ornith · Qwen 27B Vision", "baseUrl": "http://100.94.208.39:8080/v1", "model": "qwen3.8-27b", "apiKeyEnv": "ORNITH_API_KEY", "vision": true },
    { "id": "ornith-text", "name": "Ornith · 35B testo/coding", "baseUrl": "http://100.94.208.39:8080/v1", "model": "ornith-1.5-35b-a3b", "apiKeyEnv": "ORNITH_API_KEY", "vision": false }
  ]
}
```

Ogni richiesta include l'ID esatto nel campo `model`. `visionProvider` mantiene Qwen per l'analisi visiva anche quando nel toggle della ricerca è selezionato il modello testo; selezionando il provider locale resta locale anche la visione. Le richieste remote sono serializzate fino alla fine della risposta, così i due modelli non vengono interrogati contemporaneamente durante uno scambio del router. Se `ORNITH_API_KEY` manca sia dall'ambiente sia dal Portachiavi, le due voci sono indisponibili e Shelfy torna al modello locale. Il segreto è disponibile nella sessione macOS corrente tramite `launchctl setenv` e in modo persistente nel Portachiavi con servizio `ORNITH_API_KEY`; il file JSON non contiene la chiave.

Il 28 settembre 2026, usando `ORNITH_API_KEY`, `GET /v1/models` ha restituito entrambi gli ID; `POST /v1/chat/completions` con `qwen3.8-27b` ha risposto HTTP 200 a una richiesta testuale e a una richiesta con testo più `image_url`. Quest'ultima ha riconosciuto il rosso di un'immagine sintetica. La build di test installata in `/Applications/SHELFY.app` include questa integrazione.

Gli endpoint remoti richiedono HTTPS. HTTP è ammesso solo su loopback o verso un peer che il client Tailscale locale conferma attivo: in quel caso il trasporto è cifrato da Tailscale. Se il peer o il client Tailscale non è disponibile, il nodo risulta non raggiungibile: la ricerca usa solo le parole chiave, le analisi attendono in coda e la selezione remota riprende quando il peer ricompare.
