const native = require("./native.js");

/** Parse the native JSON-string boundary into the ScanReport object. */
async function scan(image, options = {}) {
  // napi only observes the abort EVENT — a pre-aborted signal would hang
  // the contract silently. Standard early check.
  if (options.signal?.aborted) {
    throw new DOMException("scan aborted before start", "AbortError");
  }
  const json = await native.scanJson(
    image,
    options.profile,
    options.signal,
    options.maxDimension,
    options.maxPixels,
    options.budgetMs,
    options.scoreSkipAxes,
    options.scoreSkipChecks,
    options.alphaBackground,
  );
  return JSON.parse(json);
}

function scanSync(image, options = {}) {
  return JSON.parse(
    native.scanSyncJson(
      image,
      options.profile,
      options.maxDimension,
      options.maxPixels,
      options.budgetMs,
      options.scoreSkipAxes,
      options.scoreSkipChecks,
      options.alphaBackground,
    ),
  );
}

module.exports = { scan, scanSync, version: native.version };
