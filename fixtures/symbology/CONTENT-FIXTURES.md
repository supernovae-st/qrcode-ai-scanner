# Content fixtures — provenance

Synthetic symbols for the exact-content rows of
`crates/qrcode-ai-scanner/tests/exact_content.rs` (spec/01 § Content): non-QR
`raw` (the decoding engine's reading), Micro QR and rMQR Kanji. Every payload
is public test text. They are test inputs, not corpus entries: corpus.toml
does not list them.

Writers: zxing-cpp 3.1.1 (Python binding, `zxingcpp.create_barcode`, then
`to_image` saved as PNG through Pillow) and segno 1.6.6
(`segno.make_micro(text, mode='kanji')`, `save(path, scale=10, border=4)`).
Text written as UTF-8 bytes means `create_barcode(text.encode('utf-8'), …)`.

| File | Writer | Content | Parameters | sha256 |
|---|---|---|---|---|
| `datamatrix-utf8-url-japanese.png` | zxing-cpp, DataMatrix | UTF-8 bytes of `https://qrc-ai.com/日本語テスト` | `to_image(scale=8)` | `2cb024fb858ff68857631e08bc152132f0b9fe61f58971ac146ee7ce7d2a7efe` |
| `datamatrix-utf8-cafe.png` | zxing-cpp, DataMatrix | UTF-8 bytes of `Café crème brûlée` | `to_image(scale=8)` | `32f99b137033977c7559092b22dc425634fe7fd0c842a9db47cc14e1c9e9e7e7` |
| `datamatrix-utf8-url-cafe.png` | zxing-cpp, DataMatrix | UTF-8 bytes of `https://qrc-ai.com/café` | `to_image(scale=8)` | `4186f4f8b4e5db4d168e21100f3ef35a763368c2c27c08c94fe51ed6d4d730b6` |
| `datamatrix-pair-japanese.png` | zxing-cpp, DataMatrix ×2 | UTF-8 bytes of `https://a.example/日本語テスト` and `https://b.example/日本語テスト` | each `to_image(scale=8)`, converted to luma; canvas `w₁ + w₂ + 80` × `max(h) + 40`, white; pasted at (20, 20) and (w₁ + 60, 20) | `82e9db9525008438852995c8ec12bc061b8af4b4a4b4196f15e2e98404879755` |
| `pdf417-utf8-cafe.png` | zxing-cpp, PDF417 | UTF-8 bytes of `Café crème brûlée` | `to_image(scale=4)` | `3c3bf3c004abc7e2028ea3b18a22499c06cf0c4f0b35e04048cade8990e447c4` |
| `databar-gtin.png` | zxing-cpp, DataBar | `09506000134352` | `to_image(scale=4, add_quiet_zones=True)` | `026567259bfd62ffeccad16af1db7baff651436a4e6746ce795f8ad8a0614157` |
| `rmqr-url-kanji.png` | zxing-cpp, RMQRCode | `https://qrc-ai.com/日本` (the writer encodes 日本 in Kanji mode, no ECI) | `to_image(scale=6, add_quiet_zones=True)` | `97bcf495768b09cbcdd870deeac2497c03d7d4bebc90adfba81b5e667797995b` |
| `microqr-kanji-nihongo.png` | zxing-cpp, MicroQRCode | `日本語` (Kanji mode) | `to_image(scale=6, add_quiet_zones=True)` | `037e356ac1b9727bcba76ae4efc3c80f4e8b8bd6f2dfc3fe174cb44ab3153a7a` |
| `microqr-kanji-yamada.png` | segno, M3-M | `山田` (Shift JIS 8E 52 93 63) | `make_micro('山田', mode='kanji')` | `88be141cda9d6c27fe5c2044205b79b5261b46adb11a206385530ec5324c08d1` |
| `microqr-kanji-yamamoto.png` | segno, M3-M | `山本` (8E 52 96 7B) | `make_micro('山本', mode='kanji')` | `d6e6d7590f4374a031146bf7ba908c8aa3755639cb19a95a23e64cfe15f70cea` |
| `microqr-kanji-nihon.png` | segno, M3-M | `日本` (93 FA 96 7B) | `make_micro('日本', mode='kanji')` | `0f946999b2d65a63f4d6540dca0bd8d11ac91813cd4e8374cb1456732e8e5cd9` |
| `microqr-kanji-tesuto.png` | segno, M3-M | `テスト` (83 65 83 58 83 67) | `make_micro('テスト', mode='kanji')` | `554ff99dc319d0e793d5d34abab9f8bd30decc4a92d1b5080829e56b3ee131ed` |
| `datamatrix-gs1-fnc1.png` | zxing-cpp, DataMatrix, GS1 (FNC1 in first position; zxing-cpp reads it back as `]d2`) | `(01)09506000134352(10)AB12` | `create_barcode(…, gs1=True)`, `write_barcode_to_image(scale=8)`, 16 px white border, 8-bit greyscale PNG | `11b2ba0fb1b9d4cd7e547055c09d6b8cf2e6c4c771a7d5bb6fea28e62bbe1228` |

`datamatrix-gs1.png` (a corpus fixture) is not GS1 mode: it carries the
element string after a literal GS character, with no FNC1 codeword.
