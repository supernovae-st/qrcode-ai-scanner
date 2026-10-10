# qrcode-ai-scanner-cli

`qrscan` — scan and validate QR codes from the command line (artistic /
AI-generated specialty). JSON `ScanReport` by default, `--pretty` for humans,
`-s` for the score alone. Exit codes: `0` found · `1` none · `2` invalid
input, usage or fault, and with `-s` a configuration that computes no score
· `3` (`-s` only) decoded, not judged within the budget.

```bash
cargo install qrcode-ai-scanner-cli
qrscan image.png --pretty
```

Powered by [qrcode-ai-scanner](https://crates.io/crates/qrcode-ai-scanner).
License: AGPL-3.0-or-later · © SuperNovae Studio.
