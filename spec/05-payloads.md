# 05 · Payload classification

`detection.payload` is an internally-tagged union (`"kind"` discriminator,
snake_case). Classification is TOTAL — anything unparseable is
`{"kind":"text"}`, never an error. Values are split structurally but NOT
percent-decoded (presentation belongs to the consumer). Parse leniently:
new kinds may appear (additive evolution) — treat an unknown `kind` as
`text`.

## Symbology-aware routing

Classification starts from the detection's symbology:

| Symbology | Route |
|---|---|
| FNC1-led carriers (QR `]Q3`/`]Q4` · DataMatrix `]d2` · Code 128 `]C1` = GS1-128) | `gs1` element string |
| retail 1D (`ean13` · `ean8` · `upc_a` · `upc_e`) | `gs1` — the symbol IS a GTIN: AI 01, value zero-padded to 14, `conformant` = the symbol's own mod-10 check digit |
| everything else | the text classifier below |

## Kinds

| `kind` | Trigger | Fields |
|---|---|---|
| `url` | `http://` / `https://` (case-insensitive) | `url` |
| `gs1_digital_link` | URL whose path matches GS1 DL grammar (below) | `url`, `elements[]`, `gtin`, `conformant`, `issues[]` |
| `gs1` | FNC1-in-first-position symbol (`]Q3`/`]Q4`) OR sniffed element string | `elements[]`, `gtin`, `conformant`, `issues[]` |
| `wifi` | `WIFI:` | `ssid`, `security`, `password?`, `hidden` |
| `email` | `mailto:` / `MATMSG:` | `to`, `subject?`, `body?` |
| `sms` | `sms:` / `SMSTO:` | `number`, `body?` |
| `tel` | `tel:` | `number` |
| `geo` | `geo:lat,lon` (validated ranges) | `lat`, `lon` |
| `me_card` | `MECARD:` | `name?`, `tel?`, `email?`, `url?` |
| `crypto` | `bitcoin:` (BIP-21) / `ethereum:` (ERC-681) | `scheme`, `address`, `amount?`¹ |
| `v_card` | `BEGIN:VCARD` | `raw` |
| `v_event` | `BEGIN:VEVENT` | `raw` |
| `text` | fallback | — |

¹ `amount` is the BIP-21 display value as written; ERC-681 `value` (wei
notation) is deliberately NOT mapped to it.

## GS1 — the conformance verdict

Both GS1 kinds carry the same verdict shape:

- `elements`: parsed `{ai, value}` pairs in symbol order.
- `gtin`: the AI 01 value when present — **as written**; check
  `conformant`/`issues` before trusting it.
- `conformant`: `true` iff EVERY element parsed and validated against the
  GenSpecs subset (check digits §7.2.7, YYMMDD dates §3.4, CSET 82
  charset, predefined lengths §7.8.4; for Digital Link additionally: path
  order 01→22→10→21, 14-digit GTIN law, qualifier formats per DL URI
  Syntax 1.6).
- `issues`: human-readable strings, **each citing its violated criterion**
  (e.g. `"AI 01: check digit invalid for '…' (GenSpecs 7.2.7)"`). Show
  them verbatim — they are the regenerate-guidance.

### The three GS1 situations a consumer must distinguish

| Situation | `kind` | `conformant` | Meaning |
|---|---|---|---|
| Proper GS1 QR (FNC1 header) with valid data | `gs1` | `true` | retail/B2B-ready element string |
| GS1 Digital Link URI, valid | `gs1_digital_link` | `true` | the Sunrise-2027 retail form — also a working URL |
| GS1 SYNTAX in a plain QR (no FNC1) | `gs1` | `false` + FNC1 issue | the classic generator mistake — flag "regenerate with GS1 mode" |

The no-FNC1 sniff is conservative: the whole payload must parse
issue-free AND carry a check-digit-valid GTIN or a literal GS byte
(0x1D) — plain numeric text (phone numbers, dates, order IDs) stays
`text`.

### Scope honesty

The validator covers the retail/product AI subset (00·01·02·10-22·30·37·
240/241/250/251·310n-369n·41x·8005·8011·8019·8020·8200·90-99). Unknown
AIs are flagged as issues and force `conformant: false` — never silently
accepted. This is a SYNTAX validator, not a full GenSpecs engine.
