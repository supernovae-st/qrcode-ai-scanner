# Guide Nicolas — le scanner en 10 minutes 🦋

Salut Nico ! Voilà tout ce qu'il faut pour comprendre, tester et merger.
(Ton Claude Code a son propre brief : [AGENTS.md](AGENTS.md).)

## C'est quoi

Notre scanner QR/barcode **en Rust**, compilé en wasm pour le navigateur.
Il remplace `qr-scanner-wechat` sur le landing ET ajoute le flow
« importer un QR existant ». Tout tourne **en local chez l'utilisateur**
— l'image ne quitte jamais son navigateur, c'est instantané (3-50 ms
typique), et chaque scan dit **ce que c'est** (symbologie + type de
contenu).

La comparaison complète avant/après : [docs/comparison.mdx](docs/comparison.mdx).

## La PR (#11 sur qrcode-ai_landing, base `scanner`)

Deux features :

1. **Le verify** — `onVerify()` dans `Body.vue` : ta boucle 3 niveaux de
   filtres canvas devient UN appel `scanImage(bytes, { profile: "full",
   budgetMs: 1500 })`. Le mapping de ton meter : score ≥80 → HIGH ·
   60-79 → MEDIUM · décodé <60 → LOW · rien → NO. Ton tracking
   `landing_qrcode_scan_failed` et `Preview.vue` sont intacts.
2. **L'import d'un QR existant** — dans l'étape Destination
   (`Target.vue`) : drop / bouton / **Ctrl+V d'un screenshot** → scan
   local → ton `checkTargetType` route vers le bon type (instagram,
   wifi, mecard…) → switch + prefill par TON mécanisme suggestion
   (`setValue`/`switchTargetType`). Toast « Détecté : WEBSITE URL » +
   extrait. Ça lit aussi les codes-barres (EAN, Code 128, DataMatrix…).
   Event `landing_qrcode_imported` ajouté à ton union typée.

Plus `pages/scan.vue` : un prototype **dev-only** (exclu en prod comme
`/preview`) de la future page « scan & convert ».

## Tester en local (avant que le npm soit publié)

```bash
# 1. dans un clone du scanner (ou demande-moi le pkg/)
cd qrcode-ai-scanner && ./scripts/build-wasm.sh   # besoin: wasm-pack + binaryen

# 2. dans le landing, branche feat/qrcode-ai-scanner-wasm
npm i
mkdir -p node_modules/@supernovae-st
ln -s /chemin/vers/qrcode-ai-scanner/crates/qrcode-ai-scanner-wasm/pkg \
      node_modules/@supernovae-st/qrcode-ai-scanner-wasm

# 3.
npm run dev
# → l'éditeur : génère un QR custom → le meter verify est branché sur le score
# → l'import : dépose/colle n'importe quel QR (ou un code-barres EAN)
# → http://localhost:3000/scan : le prototype de page
```

Dès que `@supernovae-st/qrcode-ai-scanner-wasm@0.3.0` est sur npm, le
symlink dégage et `npm i` suffit — je un-draft la PR à ce moment-là.

## Ce que le rapport te donne (pour la suite)

```ts
const report = await scanImage(bytes, { profile: "full", budgetMs: 1500 });
report.detections[0].symbology     // "qr_code" | "ean13" | "data_matrix" | …
report.detections[0].payload.kind  // "url" | "wifi" | "gs1" | "me_card" | …
report.score?.value                // 0-100 — le VRAI margin, pas "ça a décodé"
report.hints                       // quoi corriger: fix_finder_pattern, …
```

Tout est typé (le paquet embarque `report-types.d.ts`) et documenté :
le site docs est dans `docs/` (Mintlify), la spec normative dans `spec/`.

## Idées faciles à shipper ensuite (déjà câblées côté data)

- **Badge « Retail-ready ✓ »** : `payload.kind === "gs1_digital_link" &&
  payload.conformant` — aucun générateur concurrent ne l'affiche.
- **« Décode mais peu fiable — régénère »** : si un hint
  `low_correction_margin` est présent, traite le verify comme un échec.
- **Guidance design** : mappe les hints sur des messages (« dégage l'art
  du coin haut-gauche », « raccourcis l'URL »…).
- La vraie page `/scan` publique (le prototype montre le flow) + les
  locales via ton pipeline TIER (j'ai mis en-US + fr-FR, le reste
  fallback en-US).

## Pièges connus

- Vite/Nuxt : garde `optimizeDeps.exclude:
  ["@supernovae-st/qrcode-ai-scanner-wasm"]` (déjà dans la PR) — esbuild
  casse la résolution du `.wasm`.
- Le scan wasm est **synchrone sur le main thread** : toujours un
  `budgetMs` (1500-2500 dans la PR). Pour un usage caméra live plus
  tard : `scan_frame` + un Worker.
- Le profil `fast` ne décode PAS les styles blob/dot (tes templates !) —
  le verify doit rester en `full`.

Des questions → je suis là. — Thibaut 🦋
