const native = require("./native.js");

/** Parse the native JSON-string boundary into the ScanReport object. */
async function scan(image, options = {}) {
  // napi only observes the abort EVENT — a pre-aborted signal would hang
  // the contract silently. Standard early check.
  if (options.signal?.aborted) {
    throw new DOMException("scan aborted before start", "AbortError");
  }
  const json = await native.scanJson(image, options.profile, options.signal);
  return JSON.parse(json);
}

function scanSync(image, options = {}) {
  return JSON.parse(native.scanSyncJson(image, options.profile));
}

module.exports = { scan, scanSync, version: native.version };
