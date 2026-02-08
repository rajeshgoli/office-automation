# Tuya Local Key Recovery (Smart Life / ERV)

This is the repeatable process we used to restore ERV local control when Tuya local commands started failing (e.g., Err 914 or dashboard control not working). The fix is to extract the **current local key** from the Smart Life app and update `config.yaml`.

## When To Run This

Run this if any of the following happen:
- Local control fails with Err 914 (“Check device key or version”).
- Dashboard or automations can’t turn on the ERV but the Smart Life app works.
- You re-pair/reset the ERV (local key can change).

## Prereqs

- Android emulator (arm64) and ADB
- Smart Life app installed and logged in
- Root access in emulator (`adb root`)
- Java 17 (for Android SDK tools)

## 1) Quick Checks (Before Installing Anything)

These tools often already exist on this machine. Check first:

```sh
command -v adb
command -v emulator
test -d ~/android-sdk && echo "android-sdk present"
```

If all are present, skip to **2) Install Smart Life + Login**. Otherwise continue.

## 2) Emulator Setup (Apple Silicon Friendly)

```sh
# SDK setup
export JAVA_HOME="$(brew --prefix openjdk@17)/libexec/openjdk.jdk/Contents/Home"
~/android-sdk/cmdline-tools/latest/bin/sdkmanager \
  "platform-tools" "emulator" "platforms;android-30" \
  "system-images;android-30;google_apis;arm64-v8a" \
  "build-tools;30.0.3"

# Create emulator
~/android-sdk/cmdline-tools/latest/bin/avdmanager create avd \
  -n tuya \
  -k "system-images;android-30;google_apis;arm64-v8a"

# Start emulator
~/android-sdk/emulator/emulator \
  -avd tuya \
  -no-snapshot \
  -no-audio \
  -gpu swiftshader_indirect
```

## 3) Install Smart Life + Login

- Install Smart Life via Play Store or APKPure (arm64).
- Log in with your account.
- Verify the ERV device appears in the app.

## 4) Extract Local Key (On-Emulator Helper)

We run a small Java helper inside the emulator. It:
- Decrypts `global_preference.xml` using Android Keystore
- Gets the `__SECURITY_KEY__` map (MMKV encryption keys)
- Opens encrypted MMKV and prints entries containing `localKey`

### 4.1 Identify the app UID (needed for `su`)

```sh
~/android-sdk/platform-tools/adb -s emulator-5556 root
~/android-sdk/platform-tools/adb -s emulator-5556 shell \
  "ls -ld /data/data/com.tuya.smartlife"
```

The owner will look like `u0_a###`. Use that in the command below.

### 4.2 Build + Run the helper (scripted)

Use the repo script (it builds, pushes, and runs the helper):

```sh
APP_UID=<APP_UID> scripts/tuya-local-key.sh
```

If you need the implementation details, see `scripts/tuya-local-key.sh`.

### 4.3 Grab the local key

The script prints a big JSON blob. Find the ERV entry and grab its `localKey`.

Example (do **not** reuse this key):
```
name":"ERVQ-H-F-BM" ... "localKey":"<THE_KEY_YOU_NEED>"
```

### 4.4 Cleanup

```sh
~/android-sdk/platform-tools/adb -s emulator-5556 shell rm -f /data/local/tmp/dumptuya.dex.jar
```

## 5) Update Office Automate Config + Restart

Update `config.yaml`:
```yaml
erv:
  type: "tuya"
  device_id: "<your-device-id>"
  local_key: "<new-local-key>"
```

Deploy to the server and restart:
```sh
scp config.yaml USER@SERVER_IP:/path/to/office-automate/config.yaml
ssh USER@SERVER_IP 'U=$(id -u); launchctl kickstart -k gui/$U/com.office-automate.orchestrator'
```

Verify:
```sh
ssh USER@SERVER_IP 'tail -n 50 /tmp/office-automate.error.log'
```

You should see:
```
Connected to ERV via local API. Status: ...
```

## Notes

- Local keys can change after re-pair or firmware changes.
- Tuya cloud API can expire; local control will still work if the local key is correct.
- Keep local keys private (don’t commit them to Git).
- Store site-specific values (device ID, local key, server IP, local paths, app UID) in a private file excluded from Git.

## Scripted Flow (Recommended)

This repo includes a helper script to avoid copy/paste errors. It builds the dumper, pushes it to the emulator, and runs it.

```sh
APP_UID=<APP_UID> scripts/tuya-local-key.sh
```

The output will include `localKey` entries in the device list. Grab the ERV one.
