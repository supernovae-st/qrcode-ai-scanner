//! Rescue internals over adversarial bytes: the errors-and-erasures RS
//! corrector (`ee::correct`) and the data-codeword bitstream parser
//! (`bitstream::parse`) both run on the S5 rescue path — a symbol the engines
//! refused — and each is the ONLY parser over its content, so neither may
//! panic (refusal / `None` is the correct answer for garbage). Every
//! parameter is derived from the fuzz input; the crate forbids RNG by
//! contract. Seed corpus: `fuzz/corpus/fuzz_rescue_bitstream/`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use qrcode_ai_scanner::fuzzing;

fuzz_target!(|data: &[u8]| {
    // Peel a 3-byte parameter header off the front (version · parity · erasure
    // shape); the remaining body is the codeword stream. Too short for the
    // header: still drive the parser on the raw slice at version 1.
    let Some((&version_byte, rest)) = data.split_first() else {
        fuzzing::parse_bitstream(data, 1);
        return;
    };
    let version = (version_byte % 10) + 1; // 1..=10 spans all count-bit classes

    let Some((&npar_byte, rest)) = rest.split_first() else {
        fuzzing::parse_bitstream(rest, version);
        return;
    };
    let npar = usize::from(npar_byte % 30) + 1; // 1..=30 parity codewords

    let Some((&erase_byte, body)) = rest.split_first() else {
        fuzzing::parse_bitstream(rest, version);
        return;
    };

    // Bitstream parse over the raw body (attacker-controlled data codewords).
    fuzzing::parse_bitstream(body, version);

    // Errors-and-erasures correct: the block IS the body, erasure POWER
    // positions are folded deterministically from the header byte.
    // `correct` guards out-of-range / over-budget marks itself.
    let mut block = body.to_vec();
    let mut erasures = Vec::new();
    if !block.is_empty() {
        let want = usize::from(erase_byte) % (npar + 1);
        let mut pos = usize::from(erase_byte) % block.len();
        for step in 0..want {
            pos = (pos + step + 1) % block.len();
            erasures.push(pos);
        }
    }
    fuzzing::correct_block(&mut block, npar, &erasures);
});
