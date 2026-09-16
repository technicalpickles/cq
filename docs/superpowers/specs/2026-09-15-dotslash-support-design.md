# DotSlash support for cq releases

## Problem

`cq` releases four platform tarballs (`cq-<version>-<target>.tar.gz` for
`aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`) attached to each GitHub release by
`.github/workflows/release.yml`. Installing means picking the right tarball
by hand, downloading it, and extracting it.

[DotSlash](https://dotslash-cli.com/) lets us ship a single small pointer
file instead: users download one file named `cq`, and running it resolves
the current platform, fetches the matching binary from the GitHub release
(caching it locally), and execs it. One README line, no platform-picking.

## Goal

Add DotSlash as an **additional** install path. The existing tarballs are
unaffected — this is pure addition to the release pipeline, not a
replacement.

This also gets us checksum verification for free. `dotslash-publish-release`
computes a size and hash (blake3 by default) for each platform artifact and
bakes them straight into the generated `cq` pointer file. The `dotslash` CLI
verifies the downloaded artifact against that digest before ever executing
it — integrity checking happens automatically on every install/run, with no
separate `checksums.txt` to publish or for users to check by hand.

## Why a new artifact, not the existing tarball

DotSlash's per-platform `path` field (the location of the binary inside an
extracted archive) is static across releases. Today's tarballs extract into
a version-named directory (`cq-0.7.0-aarch64-apple-darwin/cq`), so that path
changes every release — DotSlash has no way to template it.

Meta's own DotSlash-distributed tools (buck2, sapling) sidestep this
entirely: instead of pointing DotSlash at their tarballs, they publish a
second, minimal artifact per platform — a bare zstd-compressed binary, no
tar wrapper, no versioned directory — and point DotSlash at that. We follow
the same pattern. This means zero changes to the existing tarball packaging
step or its output shape.

## Design

### 1. New per-target artifact: bare zstd-compressed binary

In the existing `build` matrix job in `release.yml`, after the current
"Package archive" step, add a step that compresses the raw release binary
directly (no tar):

```bash
zstd -19 "target/${{ matrix.target }}/release/cq" -o "cq-${{ matrix.target }}.zst"
```

Upload it in the same `gh release upload` call as the existing tarball.
Naming: `cq-<target>.zst`, e.g. `cq-aarch64-apple-darwin.zst`. No version in
the filename — DotSlash's `dotslash-publish-release` action matches assets
by exact name or regex per release, so the name doesn't need to encode the
version.

`ubuntu-latest` and `macos-14`/`macos-13` runners ship `zstd` preinstalled;
no extra install step expected. (Implementation plan should verify this
during the first CI run and add an install step if not.)

### 2. New CI job: generate and publish the DotSlash file

Add a `dotslash` job to `release.yml`:

```yaml
dotslash:
  name: publish dotslash file
  needs: build
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: facebook/dotslash-publish-release@v1
      env:
        GITHUB_TOKEN: ${{ github.token }}
      with:
        config: .github/workflows/dotslash-config.json
        tag: ${{ github.event.release.tag_name }}
```

Same trigger as today (`release: published`) — no `workflow_run` indirection
needed, since this job runs in the same workflow as the builds and
`needs: build` already guarantees all four `.zst` assets exist on the
release before it starts.

### 3. New config file: `.github/workflows/dotslash-config.json`

```json
{
  "outputs": {
    "cq": {
      "platforms": {
        "macos-aarch64": {
          "name": "cq-aarch64-apple-darwin.zst",
          "format": "zst",
          "path": "cq"
        },
        "macos-x86_64": {
          "name": "cq-x86_64-apple-darwin.zst",
          "format": "zst",
          "path": "cq"
        },
        "linux-x86_64": {
          "name": "cq-x86_64-unknown-linux-gnu.zst",
          "format": "zst",
          "path": "cq"
        },
        "linux-aarch64": {
          "name": "cq-aarch64-unknown-linux-gnu.zst",
          "format": "zst",
          "path": "cq"
        }
      }
    }
  }
}
```

`path` is required by `dotslash-publish-release` on every platform entry,
even for a bare (non-archive) `format: "zst"` binary where there's nothing
inside it to locate — omitting it isn't valid and crashes the action with an
unhandled `TypeError` instead of a clean error. Set it to the output name
(`cq`) on all four platforms. Hash algorithm defaults to `blake3`
(DotSlash's native hash) — no reason to override to `sha256`.

This produces one DotSlash file, named `cq`, uploaded to the same release.

### 4. README: install instructions

Add a DotSlash install option alongside the existing tarball instructions:

```
## Install via DotSlash

curl -L https://github.com/technicalpickles/cq/releases/latest/download/cq -o cq
chmod +x cq
./cq --help
```

Note that this requires [`dotslash`](https://dotslash-cli.com/docs/installation/)
itself on `PATH` — the downloaded `cq` file is a DotSlash pointer file (a
`#!/usr/bin/env dotslash` script), not a standalone binary.

## Out of scope

- Committing a `cq` DotSlash file to the repo root for a stable
  `raw.githubusercontent.com/.../main/cq` URL. Release-asset-only for now;
  can revisit if that workflow turns out to matter.
- Windows. `cq` doesn't build for Windows today (no matrix target), so no
  `windows-*` platform key is included in the DotSlash config.
- Changing the existing tarball packaging or naming in any way.

## Docs to update

Per `docs/cli-ux-conventions.md`'s "keeping docs in sync" table:

- `README.md` — new install section (above).
- `CLAUDE.md` — "Releasing" section gains a line describing the new
  `dotslash` job and its config file.

## Open questions for the implementation plan

- Confirm `zstd` is actually present on all four matrix runners without an
  extra install step (verify in first CI run rather than assuming).
- Confirm `facebook/dotslash-publish-release@v1` needs no extra permissions
  beyond the default `GITHUB_TOKEN` for `gh release upload` on this repo
  (release.yml's existing tarball upload already does this with
  `github.token`, so this should be consistent).
