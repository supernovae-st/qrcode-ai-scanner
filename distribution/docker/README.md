# Scanner CLI container

The image runs the published `qrscan` 0.10.0 CLI locally. It accepts mounted
PNG/JPEG/WebP/GIF files and prints the existing JSON report. It is a command-line
tool, not an HTTP service. QR payloads are data: no decoded link is followed.

Build and qualify each native architecture:

```sh
docker build -f distribution/docker/Dockerfile -t qrscan:0.10.0 .
python3 distribution/docker/verify.py qrscan:0.10.0 fixtures/clean/gen_v5_q.png
```

For a registry image, use its immutable published digest. Mount only the input
directory, read-only; no network is required:

```sh
docker run --rm --network none --read-only --cap-drop ALL \
  --security-opt no-new-privileges --memory 512m --cpus 2 --pids-limit 128 \
  -v "$PWD/images:/data:ro" IMAGE@sha256:DIGEST \
  /data/example.png --profile full --budget-ms 0 > report.json
```

JSON-mode exits: **0** detection, **1** a valid empty report, **2** invalid
input/usage/error. `--score-only` additionally uses **3** for a detected symbol
whose score was not completed within its budget. Never discard exit 1's JSON
or turn a missing score into zero. `--budget-ms 0` is unbounded, not a hard
timeout. Use an outer process deadline if your application needs cancellation.

The default image user is UID/GID 65532; mounted inputs must be readable by it.
To run as the current Linux user, add `--user "$(id -u):$(id -g)"`. Reports are
written on the host via stdout redirection, without granting container write
access. The image contains no compiler, registry credential or source checkout.

Linux amd64 and arm64 are qualified independently on native runners. A build
alone is not runtime qualification. The workflow exports tested image archives;
publication pulls and tests the same registry digests without rebuilding them.
Registry availability and supported architectures are recorded after publication.

See [corresponding source](SOURCE.md), [license](../../LICENSE) and
[licensing options](../../LICENSING.md). Source and product homepage links serve
different purposes; the later online scanner page belongs to its product owner.
