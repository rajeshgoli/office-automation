# Supply Chain Gates

Ticket #113 adds release-blocking checks for dependency and build integrity.

## CI Gates

GitHub Actions runs `Security Gates` on pull requests and `main`:

- `cargo fetch --locked`
- `cargo audit --deny warnings`
- `cargo deny --locked check advisories bans sources`
- Rust release build from the checked-in `Cargo.lock`
- frontend `npm ci`, `npm audit --audit-level=high`, and `npm run build`
- Android Gradle wrapper validation
- Android Gradle dependency verification with `android/gradle/verification-metadata.xml`
- Android debug APK build with strict dependency verification
- release provenance output with SHA-256 for the Rust binary

## Local Commands

Run the same checks before release-sensitive changes:

```bash
cargo fetch --locked
cargo audit --deny warnings
cargo deny --locked check advisories bans sources

cd frontend
npm ci
npm audit --audit-level=high
npm run build
cd ..

cd android
./gradlew --dependency-verification=strict :app:dependencies --configuration debugRuntimeClasspath
./gradlew --dependency-verification=strict :app:dependencies --configuration releaseRuntimeClasspath
./gradlew --dependency-verification=strict :app:assembleDebug
cd ..

cargo build --locked --manifest-path rust/office-automate-server/Cargo.toml --release
scripts/security/release-provenance.sh target/release/office-automate-server
```

The provenance script fails if tracked files are dirty and prints the commit, commit date, artifact size, and SHA-256.

## Rust Dependency Review

Network and parser crates are intentional but high-signal review points:

| Crate | Surface | Reason |
| --- | --- | --- |
| `axum`, `tower-http`, `tokio-tungstenite` | HTTP, static files, WebSocket parsing | Public/local API and dashboard transport. |
| `reqwest`, `rumqttc`, `rumqttd`, `rustuya` | outbound cloud/device clients and embedded MQTT | Required for YoLink/Kumo/device communication and Qingping MQTT. |
| `serde_json`, `serde_yaml`, `urlencoding` | config, payload, and route parsing | Required for config, Cloudflare evidence, device payloads, and auth redirects. |
| `jsonwebtoken`, `base64`, `sha2` | auth and artifact integrity | Required for Office JWTs, Basic auth, device enrollment, and APK digest checks. |
| `rusqlite` | local data store | Required for sensor, event, registration, and telemetry history. |

Adding a new network, parser, crypto, updater, or process-execution dependency requires an explicit PR note explaining why the existing crates are insufficient.

## Cloudflared Provenance

`cloudflared` is part of the public edge. Keep launchd configured with `--no-autoupdate`, update on an operator-controlled cadence, and record the installed binary/version before cutover:

```bash
command -v cloudflared
cloudflared --version
shasum -a 256 "$(command -v cloudflared)"
```

Review the Cloudflare changelog before upgrading the deployed binary.
