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

## 1) Emulator Setup (Apple Silicon Friendly)

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

## 2) Install Smart Life + Login

- Install Smart Life via Play Store or APKPure (arm64).
- Log in with your account.
- Verify the ERV device appears in the app.

## 3) Extract Local Key (On-Emulator Helper)

We run a small Java helper inside the emulator. It:
- Decrypts `global_preference.xml` using Android Keystore
- Gets the `__SECURITY_KEY__` map (MMKV encryption keys)
- Opens encrypted MMKV and prints entries containing `localKey`

### 3.1 Identify the app UID (needed for `su`)

```sh
~/android-sdk/platform-tools/adb -s emulator-5556 root
~/android-sdk/platform-tools/adb -s emulator-5556 shell \
  "ls -ld /data/data/com.tuya.smartlife"
```

The owner will look like `u0_a###`. Use that in the command below.

### 3.2 Build + Push the helper

```sh
mkdir -p /tmp/tuya-dump
cat > /tmp/tuya-dump/DumpTuya.java <<'JAVA'
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.security.KeyStore;
import java.security.MessageDigest;
import java.util.*;
import java.util.regex.*;
import javax.crypto.Cipher;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;
import android.util.Base64;
import android.content.Context;
import android.app.Application;
import android.os.Looper;
import org.json.JSONObject;

public class DumpTuya {
    private static String readAll(String path) throws IOException {
        ByteArrayOutputStream baos = new ByteArrayOutputStream();
        try (InputStream in = new FileInputStream(path)) {
            byte[] buf = new byte[8192];
            int n;
            while ((n = in.read(buf)) > 0) {
                baos.write(buf, 0, n);
            }
        }
        return baos.toString("UTF-8");
    }

    private static Map<String, String> parseStrings(String xml) {
        Map<String, String> out = new HashMap<>();
        Pattern p = Pattern.compile("<string name=\\\"([^\\\"]+)\\\">(.*?)</string>");
        Matcher m = p.matcher(xml);
        while (m.find()) {
            out.put(m.group(1), m.group(2));
        }
        return out;
    }

    private static String sha256HexUpper(String s) throws Exception {
        MessageDigest md = MessageDigest.getInstance("SHA-256");
        byte[] digest = md.digest(s.getBytes("UTF-8"));
        StringBuilder sb = new StringBuilder();
        for (byte b : digest) {
            sb.append(String.format("%02X", b));
        }
        return sb.toString();
    }

    private static String decryptValue(String enc, SecretKey key) throws Exception {
        if (enc == null) return null;
        String[] parts = enc.split("]");
        if (parts.length < 2) return null;
        byte[] iv = Base64.decode(parts[0], Base64.NO_WRAP);
        byte[] cipherText = Base64.decode(parts[1], Base64.NO_WRAP);
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.DECRYPT_MODE, key, new GCMParameterSpec(128, iv));
        byte[] plain = cipher.doFinal(cipherText);
        return new String(plain, StandardCharsets.UTF_8);
    }

    private static void ensureAppContextAndKeystoreProvider() throws Exception {
        if (Looper.myLooper() == null) {
            Looper.prepareMainLooper();
        }
        Class<?> atClass = Class.forName("android.app.ActivityThread");
        Object at = atClass.getMethod("systemMain").invoke(null);
        Context systemContext = (Context) atClass.getMethod("getSystemContext").invoke(at);
        if (systemContext != null) {
            Context appContext = systemContext.createPackageContext(
                "com.tuya.smartlife",
                Context.CONTEXT_INCLUDE_CODE | Context.CONTEXT_IGNORE_SECURITY
            );
            Application app = new Application();
            java.lang.reflect.Method attach = Application.class.getDeclaredMethod("attach", Context.class);
            attach.setAccessible(true);
            attach.invoke(app, appContext);
            java.lang.reflect.Field mInitialApplication = atClass.getDeclaredField("mInitialApplication");
            mInitialApplication.setAccessible(true);
            mInitialApplication.set(at, app);
        }
        // Ensure keystore provider installed
        try {
            Class<?> p = Class.forName("android.security.keystore.AndroidKeyStoreProvider");
            p.getMethod("install").invoke(null);
        } catch (Throwable t) {
            // ignore
        }
        try {
            Class<?> p2 = Class.forName("android.security.keystore2.AndroidKeyStoreProvider");
            p2.getMethod("install").invoke(null);
        } catch (Throwable t) {
            // ignore
        }
    }

    public static void main(String[] args) throws Exception {
        ensureAppContextAndKeystoreProvider();

        String prefsPath = "/data/data/com.tuya.smartlife/shared_prefs/global_preference.xml";
        String xml = readAll(prefsPath);
        Map<String, String> enc = parseStrings(xml);

        KeyStore ks = KeyStore.getInstance("AndroidKeyStore");
        ks.load(null);
        String alias = null;
        Enumeration<String> aliases = ks.aliases();
        while (aliases.hasMoreElements()) {
            String a = aliases.nextElement();
            if (a.endsWith("_aes_key")) {
                alias = a;
                break;
            }
        }
        if (alias == null) {
            System.out.println("No _aes_key alias found in AndroidKeyStore");
            return;
        }
        SecretKey key = (SecretKey) ks.getKey(alias, null);
        if (key == null) {
            System.out.println("No key for alias: " + alias);
            return;
        }

        String securityKeyHash = sha256HexUpper("__SECURITY_KEY__");
        String securityEnc = enc.get(securityKeyHash);
        String securityJson = decryptValue(securityEnc, key);
        if (securityJson == null) {
            System.out.println("Failed to decrypt __SECURITY_KEY__");
            return;
        }
        System.out.println("__SECURITY_KEY__=" + securityJson);
        JSONObject sec = new JSONObject(securityJson);

        // Reflective MMKV usage to avoid compile-time dependency
        Class<?> mmkvClass = Class.forName("com.thingclips.smart.mmkv.MMKV");
        mmkvClass.getMethod("initialize", String.class)
            .invoke(null, "/data/data/com.tuya.smartlife/files/thingmmkv");
        java.lang.reflect.Method mmkvWithID = mmkvClass.getMethod("mmkvWithID", String.class, int.class, String.class);
        java.lang.reflect.Method allKeysMethod = mmkvClass.getMethod("allKeys");
        java.lang.reflect.Method getStringMethod = mmkvClass.getMethod("getString", String.class, String.class);

        File dir = new File("/data/data/com.tuya.smartlife/files/thingmmkv");
        File[] files = dir.listFiles();
        if (files == null) {
            System.out.println("No MMKV files found");
            return;
        }

        for (File f : files) {
            String name = f.getName();
            if (name.endsWith(".crc")) continue;
            if (f.isDirectory()) continue;
            String cryptKey = sec.optString(name, null);
            if (cryptKey == null || cryptKey.length() == 0) continue;
            Object kv = mmkvWithID.invoke(null, name, 2, cryptKey);
            if (kv == null) continue;
            String[] keys = (String[]) allKeysMethod.invoke(kv);
            if (keys == null) continue;
            for (String k : keys) {
                String v = (String) getStringMethod.invoke(kv, k, null);
                if (v == null) continue;
                if (v.contains("localKey") || v.contains("local_key") || v.contains("devId") || v.contains("deviceId")) {
                    System.out.println("MMKV[" + name + "] " + k + " => " + v);
                }
            }
        }
    }
}
JAVA

# Build -> dex -> push
export JAVA_HOME="$(brew --prefix openjdk@17)/libexec/openjdk.jdk/Contents/Home"
JAVAC="$(brew --prefix openjdk@17)/bin/javac"
JAR="$(brew --prefix openjdk@17)/bin/jar"

rm -rf /tmp/tuya-dump/classes /tmp/tuya-dump/dex
mkdir -p /tmp/tuya-dump/classes /tmp/tuya-dump/dex

$JAVAC --release 8 -classpath ~/android-sdk/platforms/android-30/android.jar \
  -d /tmp/tuya-dump/classes /tmp/tuya-dump/DumpTuya.java

$JAR cf /tmp/tuya-dump/dumptuya.jar -C /tmp/tuya-dump/classes .
~/android-sdk/build-tools/30.0.3/d8 --min-api 21 \
  --output /tmp/tuya-dump/dex /tmp/tuya-dump/dumptuya.jar

$JAR cf /tmp/tuya-dump/dumptuya.dex.jar -C /tmp/tuya-dump/dex classes.dex
~/android-sdk/platform-tools/adb -s emulator-5556 \
  push /tmp/tuya-dump/dumptuya.dex.jar /data/local/tmp/dumptuya.dex.jar
```

### 3.3 Run it inside the emulator

Replace `<APP_UID>` with the owner you got in step 3.1.

```sh
APP_PATH=$(~/android-sdk/platform-tools/adb -s emulator-5556 shell pm path com.tuya.smartlife | head -1 | sed 's/package://')
APP_DIR=$(dirname "$APP_PATH")
LIB_DIR="$APP_DIR/lib/arm64"

~/android-sdk/platform-tools/adb -s emulator-5556 shell \
  "su <APP_UID> sh -c 'CLASSPATH=/data/local/tmp/dumptuya.dex.jar:$APP_PATH \
  LD_LIBRARY_PATH=/system/lib64:/system_ext/lib64:/vendor/lib64:$LIB_DIR \
  /system/bin/app_process /data/local/tmp DumpTuya'"
```

You’ll see a big JSON blob. Find the ERV entry and grab its `localKey`.

Example (do **not** reuse this key):
```
name":"ERVQ-H-F-BM" ... "localKey":"<THE_KEY_YOU_NEED>"
```

### 3.4 Cleanup

```sh
~/android-sdk/platform-tools/adb -s emulator-5556 shell rm -f /data/local/tmp/dumptuya.dex.jar
```

## 4) Update Office Automate Config + Restart

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
