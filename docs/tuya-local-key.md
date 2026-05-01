# Tuya Local Key Recovery (Smart Life / ERV)

This is the repeatable process to restore ERV local control when Tuya local commands start failing (e.g., Err 914 or dashboard control not working). Two paths depending on whether the local key has actually rotated or just gotten out of sync:

- **Path A: Re-extract** the cached local key from the Smart Life app. Works if the key in `config.yaml` is wrong but Smart Life still has working local control.
- **Path B: Re-pair** the ERV through Smart Life. Required when both the device's key AND Smart Life's cached copy are stale (which is what happened on 2026-04-30 — see Troubleshooting).

Always start with Path A. Drop to Path B only if the diagnostic in step 4 says so.

## When To Run This

- Local control fails with Err 914 (“Check device key or version”).
- Dashboard or automations can’t turn on the ERV but the Smart Life app still works locally.
- The device firmware was updated (most common trigger for key rotation).

## ERV Hardware (Site-Specific)

- **Model:** Pioneer ECOasis 150 Ductless ERV (`ERV150AHRPM25L`, shown in Smart Life as `ERVQ-H-F-BM`).
- **Pair-mode shortcut:** with the ERV powered ON, **long-press On/Off + Speed together** until the Wi-Fi symbol on the display flashes.
  - Fast flash = EZ mode (use this — direct Wi-Fi pair).
  - Slow flash = AP mode (fallback if EZ doesn't work).
- **Wi-Fi:** 2.4 GHz only. 5 GHz networks won't pair.
- **Smart Life category:** Small Home Appliances → Ventilation System.

## Prereqs

- Android emulator (arm64) and ADB.
- **Use `system-images;android-30;google_apis;arm64-v8a`** — NOT the `_playstore` variant. Play Store images are production builds and refuse `adb root`, which the dumper needs to `su` as the app UID.
- Java 17 (for Android SDK tools).
- About 1.5GB disk for the system image.

## 1) Quick Checks (Before Installing Anything)

```sh
command -v adb
command -v emulator
test -d ~/android-sdk && echo "android-sdk present"
~/android-sdk/cmdline-tools/latest/bin/avdmanager list avd
ls ~/android-sdk/system-images/android-30/google_apis/arm64-v8a/ 2>/dev/null && echo "image present"
```

If all of those are present and the AVD already exists, skip to **3**. Otherwise continue with **2**.

## 2) Emulator Setup (Apple Silicon Friendly)

```sh
export JAVA_HOME="$(brew --prefix openjdk@17)/libexec/openjdk.jdk/Contents/Home"
export PATH="$JAVA_HOME/bin:$PATH"

~/android-sdk/cmdline-tools/latest/bin/sdkmanager \
  "platform-tools" "emulator" "platforms;android-30" \
  "system-images;android-30;google_apis;arm64-v8a" \
  "build-tools;30.0.3"

echo "no" | ~/android-sdk/cmdline-tools/latest/bin/avdmanager create avd \
  -n tuya \
  -k "system-images;android-30;google_apis;arm64-v8a"
```

Optional but recommended quality-of-life tweaks in `~/.android/avd/tuya.avd/config.ini`:
```
hw.keyboard=yes        # host keyboard passes through (default forces on-screen IME)
hw.lcd.density=320     # 720p portrait at xhdpi instead of cramped 320x640
hw.lcd.height=1280
hw.lcd.width=720
```

Boot:
```sh
~/android-sdk/emulator/emulator -avd tuya -port 5556 \
  -no-snapshot -no-audio -gpu swiftshader_indirect &
```

## 3) Install Smart Life

The `google_apis` image has no Play Store, so you sideload. Two options:

### Option A: Use a known-good Smart Life APK you've stashed somewhere

```sh
adb -s emulator-5556 install /path/to/smartlife.apk
```

Skip ahead to login.

### Option B: Pull split APKs from a temporary Play Store AVD (recommended on first run)

Smart Life on Play Store is split into `base.apk` + `split_config.arm64_v8a.apk`. The native libs (e.g. `libthingmmkv.so`) are in the arm64 split — installing only `base.apk` will crash on launch with `dlopen failed: library "libthingmmkv.so" not found`. You need both.

```sh
# One-shot Play Store AVD, runs alongside the rooted one
~/android-sdk/cmdline-tools/latest/bin/sdkmanager \
  "system-images;android-30;google_apis_playstore;arm64-v8a"
echo "no" | ~/android-sdk/cmdline-tools/latest/bin/avdmanager create avd \
  -n tuya-play -k "system-images;android-30;google_apis_playstore;arm64-v8a"
~/android-sdk/emulator/emulator -avd tuya-play -port 5558 \
  -no-snapshot -no-audio -gpu swiftshader_indirect &

# (Sign in to Google in the new emulator window, install Smart Life from Play Store —
#  no Tuya login needed here, just need the APK files.)

# Pull both splits
adb -s emulator-5558 shell pm path com.tuya.smartlife   # confirm two paths
adb -s emulator-5558 pull <base.apk path>            /tmp/smartlife-base.apk
adb -s emulator-5558 pull <split_config.arm64_v8a.apk path> /tmp/smartlife-arm64.apk

# Install both atomically on the rooted AVD
adb -s emulator-5556 install-multiple -r \
  /tmp/smartlife-base.apk /tmp/smartlife-arm64.apk

# Tear down the Play Store AVD when done
adb -s emulator-5558 emu kill
~/android-sdk/cmdline-tools/latest/bin/avdmanager delete avd -n tuya-play
```

Now log in to Smart Life on `emulator-5556` (the rooted one) with your Tuya account and confirm the ERV is in the device list.

## 4) Extract Local Key (Path A)

### 4.1 App UID

```sh
adb -s emulator-5556 root
adb -s emulator-5556 shell "ls -ld /data/data/com.tuya.smartlife"
```

The owner is `u0_a###`. The exact number depends on install order; don't assume the value from the previous run.

### 4.2 Run the dumper

```sh
APP_UID=<u0_a###> scripts/tuya-local-key.sh
```

Output is a series of `MMKV[...] ... => {...}` lines. Find the entry whose `name` is your ERV (`ERVQ-H-F-BM`) and grab its `localKey`.

```
"name":"ERVQ-H-F-BM" ... "devId":"<your-id>" ... "localKey":"<THE_KEY_YOU_NEED>"
```

### 4.3 Diagnostic: did the key actually rotate?

In the same JSON entry, look at `localConnectLastUpdateTime`:

- **Recent (within hours)**: Smart Life is talking locally to the device → the dumped `localKey` is fresh and matches reality. Proceed to step 5.
- **Old (days/weeks/months)**: Smart Life is also stuck on cloud-only because its cached key is stale too. The key you just dumped is the same one already in `config.yaml` and won't fix anything. **Switch to Path B (re-pair).**

### 4.4 Cleanup

```sh
adb -s emulator-5556 shell rm -f /data/local/tmp/dumptuya.dex.jar
```

## 4-B) Re-Pair the ERV (Path B)

When step 4.3 says the dumped key is stale, you need a hardware re-pair to mint a fresh key in Tuya cloud.

1. **Reset the ERV to pair mode** — see "ERV Hardware" above (long-press On/Off + Speed, fast flash).
2. **In Smart Life on a real phone (not the emulator):**
   - Tap the existing ERV → settings (pencil/gear) → Remove Device. This clears the stale cloud entry.
   - `+` (top-right) → Add Device → Small Home Appliances → Ventilation System.
   - Phone must be on 2.4 GHz Wi-Fi. Enter SSID + password.
   - Wait for pairing (1–2 minutes).
3. Once Smart Life shows the ERV online, **force-restart Smart Life on `emulator-5556`** so it pulls the new device list:
   ```sh
   adb -s emulator-5556 shell am force-stop com.tuya.smartlife
   adb -s emulator-5556 shell monkey -p com.tuya.smartlife -c android.intent.category.LAUNCHER 1
   ```
4. Re-run the dumper. The new `localKey` will be different from the one in `config.yaml`.

## 5) Update Config + Restart

Update `config.yaml`:
```yaml
erv:
  type: "tuya"
  device_id: "<device-id-unchanged>"
  local_key: "<new-local-key>"
```

Local keys can contain shell-special characters (`$`, `}`, `>`, `^`, etc.). YAML double quotes are safe but `sed` over SSH will fight you — easiest is `scp` the file local, edit, `scp` back.

Deploy + restart:
```sh
scp config.yaml USER@SERVER_IP:/path/to/office-automate/config.yaml
ssh USER@SERVER_IP 'U=$(id -u); launchctl kickstart -k gui/$U/com.office-automate.orchestrator'
ssh USER@SERVER_IP 'tail -n 50 /tmp/office-automate.error.log'
```

You should see:
```
Connected to ERV via local API. Status: ...
```

## 6) Tear Down (Reclaim ~1.7GB)

```sh
adb -s emulator-5556 emu kill
~/android-sdk/cmdline-tools/latest/bin/avdmanager delete avd -n tuya
~/android-sdk/cmdline-tools/latest/bin/sdkmanager --uninstall \
  "system-images;android-30;google_apis;arm64-v8a" \
  "system-images;android-30;google_apis_playstore;arm64-v8a"
rm -f /tmp/smartlife*.apk
rm -rf /tmp/tuya-dump
```

## Troubleshooting

### Smart Life crashes on launch with `libthingmmkv.so not found`
You only sideloaded `base.apk`. You also need `split_config.arm64_v8a.apk`. See step 3 Option B.

### `adbd cannot run as root in production builds`
You're on a `_playstore` system image. Recreate the AVD with the plain `google_apis` image.

### Dumper prints `__SECURITY_KEY__={"GLOBAL_SECURITY_KEY":"..."}` then nothing else
This was the old behavior of the script — fixed in-tree. Newer Smart Life versions store per-MMKV-file encryption keys *inside* the `GLOBAL_SECURITY_KEY` MMKV file (encrypted with the global key from the JSON). The script handles this format now. If you see this symptom, you're running an older script — pull latest.

### Dumped `localKey` matches the one already in `config.yaml`
Both the device and Smart Life are stuck on a stale key. See step 4.3 → Path B.

### Tuya Cloud fallback returns 0 devices
Tuya cloud projects need devices linked via the IoT Platform asset model. Don't rely on cloud — local-only is the goal. The orchestrator's cloud fallback in `erv_client.py` is best-effort and will likely fail silently here.

## Prevention

The 914 error is almost always triggered by an ERV firmware OTA. To minimize occurrences:

- **Disable auto firmware updates on the ERV** in Smart Life device settings if the toggle exists (look for "Check for firmware updates" or similar).
- **Alert on 914** in the orchestrator logs so you catch the rotation immediately, not days/weeks later when CO2 is high and the dashboard is silent.

## Notes

- Local keys can change after re-pair, factory reset, or firmware OTA.
- Keep local keys private (don’t commit them to Git). `docs/tuya-local-key-private.md` is gitignored for this.
- Site-specific values (device ID, server IP, repo path on server, last-known UID) live in `docs/tuya-local-key-private.md`. The UID can shift between installs (saw `u0_a166` and `u0_a167` on consecutive installs in the same session).
