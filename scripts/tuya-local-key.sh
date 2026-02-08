#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${APP_UID:-}" ]]; then
  echo "APP_UID is required. Example: APP_UID=u0_a167 scripts/tuya-local-key.sh" >&2
  exit 1
fi

if ! command -v adb >/dev/null 2>&1; then
  echo "adb not found in PATH" >&2
  exit 1
fi

if ! command -v javac >/dev/null 2>&1; then
  echo "javac not found in PATH" >&2
  exit 1
fi

if ! command -v jar >/dev/null 2>&1; then
  echo "jar not found in PATH" >&2
  exit 1
fi

if [[ ! -d "${HOME}/android-sdk" ]]; then
  echo "~/android-sdk not found" >&2
  exit 1
fi

SDK_ROOT="${HOME}/android-sdk"
BUILD_TOOLS="${SDK_ROOT}/build-tools/30.0.3"
PLATFORM_JAR="${SDK_ROOT}/platforms/android-30/android.jar"
D8_BIN="${BUILD_TOOLS}/d8"

if [[ ! -x "${D8_BIN}" ]]; then
  echo "D8 not found at ${D8_BIN}. Install build-tools;30.0.3" >&2
  exit 1
fi

WORKDIR="/tmp/tuya-dump"
mkdir -p "${WORKDIR}"

cat > "${WORKDIR}/DumpTuya.java" <<'JAVA'
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

rm -rf "${WORKDIR}/classes" "${WORKDIR}/dex" "${WORKDIR}/dumptuya.jar" "${WORKDIR}/dumptuya.dex.jar"
mkdir -p "${WORKDIR}/classes" "${WORKDIR}/dex"

javac --release 8 -classpath "${PLATFORM_JAR}" -d "${WORKDIR}/classes" "${WORKDIR}/DumpTuya.java"
jar cf "${WORKDIR}/dumptuya.jar" -C "${WORKDIR}/classes" .
"${D8_BIN}" --min-api 21 --output "${WORKDIR}/dex" "${WORKDIR}/dumptuya.jar"
jar cf "${WORKDIR}/dumptuya.dex.jar" -C "${WORKDIR}/dex" classes.dex

adb -s emulator-5556 root >/dev/null
adb -s emulator-5556 push "${WORKDIR}/dumptuya.dex.jar" /data/local/tmp/dumptuya.dex.jar >/dev/null

APP_PATH=$(adb -s emulator-5556 shell pm path com.tuya.smartlife | head -1 | sed 's/package://')
APP_DIR=$(dirname "$APP_PATH")
LIB_DIR="$APP_DIR/lib/arm64"

adb -s emulator-5556 shell \
  "su ${APP_UID} sh -c 'CLASSPATH=/data/local/tmp/dumptuya.dex.jar:$APP_PATH \
  LD_LIBRARY_PATH=/system/lib64:/system_ext/lib64:/vendor/lib64:$LIB_DIR \
  /system/bin/app_process /data/local/tmp DumpTuya'"
