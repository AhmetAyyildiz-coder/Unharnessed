#!/usr/bin/env python3
"""Refresh the catalog parity fixture (tests/fixtures/catalog.v1.json).

The fixture is a byte-faithful snapshot of the real catalog repo
(PrimeIntellect-ai/prime-agent-catalog, models/catalog.v1.json). The
published payload already IS the catalog aggregate `createModelCatalog`
builds from it (packages/ai/src/model-catalog.ts semantics): schemaVersion
1, headers stripped, prime-inference excluded (it is fetched live),
entries sorted by (provider, id). CI parses this committed snapshot so the
parity verifier runs against real data at real scale, hermetically.

The default mode fetches the live catalog (public repo; optional
GITHUB_TOKEN / PRIME_CATALOG_REPO_TOKEN Bearer; aborts after 5 s, refuses
redirects, caps responses at MAX_REMOTE_CATALOG_BYTES);
`--catalog-dir <prime-agent-catalog>` reads a local checkout instead, so
the refresh also works offline.

Usage:
  python3 scripts/generate-catalog-fixture.py
  python3 scripts/generate-catalog-fixture.py --catalog-dir <prime-agent-catalog>
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
FIXTURE = REPO / "crates/uh-models/tests/fixtures/catalog.v1.json"

DEFAULT_MODEL_CATALOG_URL = (
    "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog"
    "/main/models/catalog.v1.json"
)

# Fetch limits: 20 MiB cap, 5 s hard timeout, no redirects (a moved catalog
# must be a client change).
MAX_REMOTE_CATALOG_BYTES = 20 * 1024 * 1024
FETCH_TIMEOUT_SECONDS = 5


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise urllib.error.HTTPError(
            req.full_url, code,
            f"refused redirect to {newurl} (catalog moves require a client change)",
            headers, fp)


def fetch_catalog(url: str, label: str) -> str:
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("PRIME_CATALOG_REPO_TOKEN")
    request = urllib.request.Request(url, headers={
        "accept": "application/json",
        "cache-control": "no-cache",
        **({"authorization": f"Bearer {token}"} if token else {}),
    })
    opener = urllib.request.build_opener(_NoRedirect)
    try:
        response = opener.open(request, timeout=FETCH_TIMEOUT_SECONDS)
    except urllib.error.HTTPError as error:
        hint = ""
        if error.code in (401, 404):
            hint = (" The catalog repo is private or absent; set GITHUB_TOKEN "
                    "or PRIME_CATALOG_REPO_TOKEN.")
        elif 300 <= error.code < 400:
            hint = f" ({error.reason})"
        fail(f"Failed to fetch {label} catalog from {url}: HTTP {error.code}.{hint}")
    except (urllib.error.URLError, OSError, TimeoutError) as error:
        fail(f"Failed to fetch {label} catalog from {url}: {error}")
    with response:
        content_length = response.headers.get("content-length")
        if content_length is not None:
            try:
                if int(content_length) > MAX_REMOTE_CATALOG_BYTES:
                    fail(
                        f"{label} catalog is too large: {content_length} bytes "
                        f"exceeds {MAX_REMOTE_CATALOG_BYTES}"
                    )
            except ValueError:
                pass
        body = bytearray()
        # The 5 s timeout is the TOTAL fetch budget, not per blocking op:
        # read1 returns whatever is available, so the absolute deadline is
        # checked between chunks.
        deadline = time.monotonic() + FETCH_TIMEOUT_SECONDS
        while True:
            if time.monotonic() >= deadline:
                fail(
                    f"{label} catalog fetch exceeded the "
                    f"{FETCH_TIMEOUT_SECONDS}s hard timeout"
                )
            chunk = response.read1(64 * 1024)
            if not chunk:
                break
            body.extend(chunk)
            if len(body) > MAX_REMOTE_CATALOG_BYTES:
                fail(
                    f"{label} catalog is too large: exceeds "
                    f"{MAX_REMOTE_CATALOG_BYTES} bytes"
                )
        text = body.decode("utf-8")
    return text if text.endswith("\n") else f"{text}\n"


def parse_aggregate(text: str) -> list:
    """The fixture contract: the real catalog payload, aggregate-shaped."""
    catalog = json.loads(text)
    models = catalog["models"]
    if catalog.get("schemaVersion") != 1 or not isinstance(models, list) or not models:
        fail("the payload is not the models/catalog.v1.json aggregate")
    prime_inference = [m for m in models if m.get("provider") == "prime-inference"]
    if prime_inference:
        fail(
            "the catalog aggregate must exclude prime-inference "
            f"({len(prime_inference)} entries present)"
        )
    with_headers = [m for m in models if "headers" in m]
    if with_headers:
        fail(f"catalog entries must not carry headers ({len(with_headers)} do)")
    return models


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--catalog-dir",
        type=Path,
        default=None,
        help="read models/catalog.v1.json from a local prime-agent-catalog "
        "checkout instead of fetching the live catalog",
    )
    args = parser.parse_args()

    if args.catalog_dir is not None:
        text = (args.catalog_dir / "models" / "catalog.v1.json").read_text()
        if not text.endswith("\n"):
            text = f"{text}\n"
    else:
        text = fetch_catalog(DEFAULT_MODEL_CATALOG_URL, "model")
    models = parse_aggregate(text)
    FIXTURE.write_text(text)
    providers = {model["provider"] for model in models}
    print(f"{FIXTURE}: {len(models)} models, {len(providers)} providers")


if __name__ == "__main__":
    main()
