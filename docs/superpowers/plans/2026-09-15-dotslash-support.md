# DotSlash Release Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish a single-file DotSlash pointer (`cq`) on every GitHub release, alongside the existing platform tarballs, so `curl` + `chmod +x` + run works without picking a platform archive by hand.

**Architecture:** Add a bare zstd-compressed binary per target (no tarball wrapper, no versioned directory) to the existing `build` matrix job in `.github/workflows/release.yml`, then add a new `dotslash` job (`needs: build`) that runs `facebook/dotslash-publish-release@v1` against a new `.github/workflows/dotslash-config.json` to generate and upload the `cq` pointer file to the same release.

**Tech Stack:** GitHub Actions, `zstd` CLI, `facebook/dotslash-publish-release@v1`, `gh` CLI.

**Spec:** `docs/superpowers/specs/2026-09-15-dotslash-support-design.md`

---

### Task 1: Add the DotSlash config file

**Files:**
- Create: `.github/workflows/dotslash-config.json`

- [ ] **Step 1: Create the config file**

```json
{
  "outputs": {
    "cq": {
      "platforms": {
        "macos-aarch64": {
          "name": "cq-aarch64-apple-darwin.zst",
          "format": "zst"
        },
        "macos-x86_64": {
          "name": "cq-x86_64-apple-darwin.zst",
          "format": "zst"
        },
        "linux-x86_64": {
          "name": "cq-x86_64-unknown-linux-gnu.zst",
          "format": "zst"
        },
        "linux-aarch64": {
          "name": "cq-aarch64-unknown-linux-gnu.zst",
          "format": "zst"
        }
      }
    }
  }
}
```

- [ ] **Step 2: Verify it's valid JSON**

Run: `python3 -m json.tool .github/workflows/dotslash-config.json > /dev/null && echo VALID`
Expected: `VALID`

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/dotslash-config.json
git commit -m "ci: add dotslash-publish-release config for cq"
```

---

### Task 2: Add a bare zstd-compressed binary artifact to the release build job

**Files:**
- Modify: `.github/workflows/release.yml`

The `build` job's matrix already produces `target/${{ matrix.target }}/release/cq` and packages it into a versioned tarball (`cq-<version>-<target>.tar.gz`). DotSlash needs a *second*, unversioned artifact per target: `cq-<target>.zst`, a bare zstd-compressed binary with no wrapper directory, so its in-archive path (`format: "zst"`, no `path` field) never has to change across releases.

- [ ] **Step 1: Add a compression step after "Package archive", before "Upload to release"**

Insert this step between the existing `Package archive` step and the `Upload to release` step:

```yaml
      - name: Compress binary for DotSlash
        shell: bash
        run: |
          dotslash_asset="cq-${{ matrix.target }}.zst"
          zstd -19 "target/${{ matrix.target }}/release/cq" -o "$dotslash_asset"
          echo "DOTSLASH_ASSET=${dotslash_asset}" >> "$GITHUB_ENV"
```

- [ ] **Step 2: Update the "Upload to release" step to upload both assets**

Change:

```yaml
      - name: Upload to release
        shell: bash
        env:
          GH_TOKEN: ${{ github.token }}
        run: gh release upload "$TAG" "$ASSET" --clobber --repo "$GITHUB_REPOSITORY"
```

to:

```yaml
      - name: Upload to release
        shell: bash
        env:
          GH_TOKEN: ${{ github.token }}
        run: gh release upload "$TAG" "$ASSET" "$DOTSLASH_ASSET" --clobber --repo "$GITHUB_REPOSITORY"
```

- [ ] **Step 3: Verify the workflow YAML is still syntactically valid**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml')); print('VALID')"`
Expected: `VALID`

- [ ] **Step 4: Locally sanity-check the zstd command round-trips correctly**

This can't run the actual matrix build, but it can prove the exact command in Step 1 produces a working, byte-identical artifact when decompressed — build any binary as a stand-in:

```bash
cargo build --release
zstd -19 target/release/cq -o /tmp/cq-sanity-check.zst
zstd -d /tmp/cq-sanity-check.zst -o /tmp/cq-sanity-check.out --force
cmp target/release/cq /tmp/cq-sanity-check.out && echo ROUNDTRIP_OK
rm /tmp/cq-sanity-check.zst /tmp/cq-sanity-check.out
```

Expected: `ROUNDTRIP_OK`

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: publish a bare zstd-compressed binary per target for DotSlash"
```

---

### Task 3: Add the dotslash publish job

**Files:**
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Add a new `dotslash` job after the `build` job**

Append this job at the end of the `jobs:` map (same indentation level as `build:`):

```yaml
  dotslash:
    name: publish dotslash file
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.event.release.tag_name }}

      - uses: facebook/dotslash-publish-release@v1
        env:
          GITHUB_TOKEN: ${{ github.token }}
        with:
          config: .github/workflows/dotslash-config.json
          tag: ${{ github.event.release.tag_name }}
```

Top-level `permissions: contents: write` (already present in this workflow) covers this job too, so no extra `permissions:` block is needed here.

- [ ] **Step 2: Verify the workflow YAML is still syntactically valid**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml')); print('VALID')"`
Expected: `VALID`

- [ ] **Step 3: Read back the full file and confirm the job graph makes sense**

Run: `cat .github/workflows/release.yml`
Expected: one `build` job (matrix, 4 targets, uploads tarball + `.zst`), followed by one `dotslash` job with `needs: build`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: add dotslash job to publish a cq pointer file per release"
```

---

### Task 4: Document the install path in README.md

**Files:**
- Modify: `README.md:68-72` (the `## Install` section)

Per this repo's `.claude/rules/readme.md`, install instructions belong in Part 2 (the plain reference section, not the screenplay montage). Add a new `### DotSlash` subsection next to the existing `### Prebuilt binary` and `### From source` subsections.

- [ ] **Step 1: Insert the new subsection**

Change:

```markdown
## Install

### Prebuilt binary

Grab the archive for your platform from the [latest release](https://github.com/technicalpickles/cq/releases/latest), extract it, and put `cq` on your `PATH`. Builds are published for macOS (Apple Silicon and Intel) and Linux (x86_64 and arm64).

### From source
```

to:

```markdown
## Install

### DotSlash

If you have [DotSlash](https://dotslash-cli.com/docs/installation/) installed, this is the simplest path — one file, no platform-picking:

```bash
curl -L https://github.com/technicalpickles/cq/releases/latest/download/cq -o cq
chmod +x cq
./cq --help
```

The downloaded `cq` is a small pointer file, not the binary itself: running it resolves your platform, fetches and caches the matching binary from the release (verifying it against a checksum baked into the pointer file), and execs it.

### Prebuilt binary

Grab the archive for your platform from the [latest release](https://github.com/technicalpickles/cq/releases/latest), extract it, and put `cq` on your `PATH`. Builds are published for macOS (Apple Silicon and Intel) and Linux (x86_64 and arm64).

### From source
```

- [ ] **Step 2: Confirm the section renders sensibly**

Run: `sed -n '60,100p' README.md`
Expected: `## Install` followed by `### DotSlash`, `### Prebuilt binary`, `### From source`, in that order, each with their content intact.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add DotSlash install instructions"
```

---

### Task 5: Document the new CI job in CLAUDE.md

**Files:**
- Modify: `CLAUDE.md` (the `## Releasing` section)

- [ ] **Step 1: Extend the numbered flow's last step and add a line about the config file**

Change:

```markdown
3. Merging that PR cuts the git tag + GitHub release. The `release: published` event
   then fires `release.yml`, which builds `cq` for macOS (arm64 + x86_64) and Linux
   (x86_64 + arm64) and attaches the archives to the release.
```

to:

```markdown
3. Merging that PR cuts the git tag + GitHub release. The `release: published` event
   then fires `release.yml`, which builds `cq` for macOS (arm64 + x86_64) and Linux
   (x86_64 + arm64), attaches the tarballs to the release, and (in a follow-up `dotslash`
   job) publishes a single-file DotSlash pointer (`cq`) generated from
   `.github/workflows/dotslash-config.json` via `facebook/dotslash-publish-release`.
```

- [ ] **Step 2: Confirm the edit landed correctly**

Run: `grep -n "dotslash" CLAUDE.md`
Expected: one match, in the updated sentence above.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: describe the dotslash release job in CLAUDE.md"
```

---

### Task 6: Final review pass

**Files:** none (verification only)

- [ ] **Step 1: Diff the whole branch against main**

Run: `git diff main...HEAD --stat`
Expected: `.github/workflows/release.yml`, `.github/workflows/dotslash-config.json` (new), `README.md`, `CLAUDE.md` — nothing else.

- [ ] **Step 2: Re-validate both changed/added structured files together**

Run:
```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml')); print('YAML_VALID')"
python3 -m json.tool .github/workflows/dotslash-config.json > /dev/null && echo JSON_VALID
```
Expected: `YAML_VALID` then `JSON_VALID`

- [ ] **Step 3: Note the one thing that can't be verified locally**

The `dotslash` job itself (whether `facebook/dotslash-publish-release@v1` accepts this config, whether `zstd` is preinstalled on all four matrix runners, whether the generated `cq` file actually resolves and runs) can only be verified by cutting a real release. Call this out explicitly when handing off / opening the PR: first real release is the actual integration test, and the two "Open questions" from the spec should be checked against that run's logs.

No commit for this task — it's a review checkpoint.
