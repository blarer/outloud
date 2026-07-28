# Signing and notarization runbook

Everything in this document requires a human with a payment method and a legal
identity. It is the one part of the release pipeline that cannot be automated
away, so it is written as a checklist rather than as prose.

The pipeline already degrades safely without any of it: every signing step in
`.github/workflows/release.yml` is gated on its secret being present, so
unsigned builds still produce artifacts. What you lose without certificates is
covered under "what breaks while unsigned" below.

## Why this is urgent, not cosmetic

Signing is usually treated as a distribution concern to be handled just before
1.0. For this project it is a **permissions** concern, which makes it a
development concern from the first external tester onward.

macOS pins an Accessibility grant to the binary's `cdhash`. With an ad-hoc
signature, every rebuild produces a new `cdhash`, so the grant silently stops
applying while the toggle in System Settings continues to read "on". This was
hit repeatedly during M0 and is documented in `docs/macos-permissions.md`.

A Developer ID certificate fixes it structurally: the designated requirement is
anchored to the team identifier rather than to a per-build hash, so the grant
survives rebuilds. `doctor` reports the current state under the
`code-signature` check.

## macOS

**What to buy:** Apple Developer Program membership, 99 USD/year.
<https://developer.apple.com/programs/>

Requires either an individual identity or, for an organization, a D-U-N-S
number, which can take several days to obtain. Start this before you need it.

**Certificate to create:** "Developer ID Application". This is the one for
software distributed outside the App Store. Create it in the Developer portal
under Certificates, then export it from Keychain Access as a `.p12` with a
strong password.

**Repository secrets to set:**

| Secret | Value |
|---|---|
| `MACOS_CERT_P12` | base64 of the exported `.p12` |
| `MACOS_CERT_PASSWORD` | the `.p12` export password |
| `MACOS_SIGN_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | Apple ID used for notarization |
| `APPLE_TEAM_ID` | 10-character team identifier |
| `APPLE_APP_PASSWORD` | app-specific password, **not** the account password |

Generate the app-specific password at <https://appleid.apple.com> under
Sign-In and Security. Never put a real account password in CI.

```bash
base64 -i DeveloperID.p12 | pbcopy   # value for MACOS_CERT_P12
```

**Notarization** is separate from signing and equally required: without it,
Gatekeeper refuses to launch a downloaded application. `scripts/build-macos-release.sh`
submits with `notarytool` and staples the ticket, so the result works offline.

**Verify a release build:**

```bash
codesign --verify --deep --strict --verbose=2 dist/AquaSpike.app
spctl --assess --type execute --verbose dist/AquaSpike.app   # expect: accepted
xcrun stapler validate dist/AquaSpike.app
```

## Windows

**What to buy:** an OV or EV code-signing certificate, roughly 200-600 USD/year
from DigiCert, Sectigo, or SSL.com. Azure Trusted Signing is cheaper (about 10
USD/month) and is the better default for a new project.

Since June 2023 Microsoft requires the private key to live in an FIPS 140-2
Level 2 HSM, so a certificate can no longer be a file you hold. In practice
this means either a cloud signing service or a hardware token, and a hardware
token is awkward in CI.

**The EV question:** an OV certificate signs correctly but accumulates
SmartScreen reputation slowly, so early users still see a warning. EV
certificates get reputation immediately. For a project whose first impression
matters, EV is worth the difference.

**Repository secrets:** `WINDOWS_CERT_PFX` (base64) and
`WINDOWS_CERT_PASSWORD`, or the Azure Trusted Signing credentials if using
that route.

## Linux

No certificate authority is involved. Package signing uses your own GPG key.

```bash
gpg --full-generate-key            # RSA 4096, no expiry or a long one
gpg --armor --export KEYID > hexavoice-signing.asc
```

Publish the public key alongside releases so users can verify. For Flathub,
the repository is signed by Flathub itself; you sign the submission commit.

## What breaks while unsigned

| Platform | Symptom |
|---|---|
| macOS | Gatekeeper refuses to open downloads; Accessibility grant dies on every rebuild |
| Windows | SmartScreen warns on every download until reputation accrues |
| Linux | Nothing blocks, but users cannot verify authenticity |

The macOS row is the one that matters during development, because it makes the
product look broken rather than unsigned.

## Order of operations

1. Start the Apple Developer enrolment now. It gates the most painful symptom
   and can take days if a D-U-N-S number is needed.
2. Set up Azure Trusted Signing when a Windows build first goes to a tester.
3. Generate the GPG key whenever Linux packages are first published.

Until step 1 completes, developers should keep using `tccutil reset` after each
rebuild, exactly as `doctor` recommends.
