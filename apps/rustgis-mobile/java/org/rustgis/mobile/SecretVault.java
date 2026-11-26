package org.rustgis.mobile;

/**
 * Secrets kept for the Rust side's secret store (src/secrets.rs): each one encrypted with
 * AES-GCM under a key in the Android Keystore, made once and never leaving it, and kept in the
 * file of its own as "1.<nonce>.<ciphertext>" (Base64), the account name bound to it. The files
 * are in the app's no-backup folder, which Android's backups leave out (the key couldn't open
 * them elsewhere), named by a SHA-256 of the account. A new entry is written whole beside the old
 * and renamed over it, so a failed save or removal leaves the entry as it was, and is said as a
 * failure. Nothing is kept unencrypted: when the keystore can't be used, the Rust side says so
 * and the secret lasts for the session. An entry a newer RustGIS wrote (a version above 1) is
 * neither read, written over nor removed.
 *
 * Each call answers with a line: "ok" (with the secret after a tab, for a load that found one),
 * "none" (a load that found none), "unavailable" with the system's reason after a tab, or
 * "failed" with a code after a tab (newer, unwritten, damaged, no-key, undecryptable, error) and
 * the system's reason, if any, after another: the Rust side words them (src/vaultanswer.rs).
 * Called on any thread, one call at a time.
 */
final class SecretVault {
    /** The keystore's name for the one key. */
    private static final String KEY = "rustgis-secrets";
    /** The folder, in the app's no-backup folder, the encrypted secrets are kept in. */
    private static final String FOLDER = "rustgis-secrets";
    /** The version of the entries this one writes. */
    private static final int VERSION = 1;
    private static final String CIPHER = "AES/GCM/NoPadding";
    private static final int TAG_BITS = 128;
    private static final String NEWER = "failed\tnewer";
    private static final String UNWRITTEN = "failed\tunwritten";

    private SecretVault() {}

    static synchronized String load(android.content.Context context, String account) {
        try {
            String kept = kept(file(context, account));
            if (kept == null) {
                return "none";
            }
            int version = version(kept);
            if (version > VERSION) {
                return NEWER;
            }
            String[] parts = kept.split("\\.", -1);
            if (version != VERSION || parts.length != 3) {
                return "failed\tdamaged";
            }
            javax.crypto.SecretKey key = key(false);
            if (key == null) {
                // Restored to another device, say: the key never leaves the one it was made on.
                return "failed\tno-key";
            }
            byte[] iv = android.util.Base64.decode(parts[1], android.util.Base64.NO_WRAP);
            byte[] sealed = android.util.Base64.decode(parts[2], android.util.Base64.NO_WRAP);
            javax.crypto.Cipher cipher = javax.crypto.Cipher.getInstance(CIPHER);
            cipher.init(javax.crypto.Cipher.DECRYPT_MODE, key, new javax.crypto.spec.GCMParameterSpec(TAG_BITS, iv));
            cipher.updateAAD(account.getBytes(java.nio.charset.StandardCharsets.UTF_8));
            byte[] plain = cipher.doFinal(sealed);
            return "ok\t" + new String(plain, java.nio.charset.StandardCharsets.UTF_8);
        } catch (java.security.KeyStoreException | java.security.NoSuchProviderException | java.io.IOException
                | java.security.ProviderException e) {
            return "unavailable\t" + reason(e);
        } catch (java.security.GeneralSecurityException | IllegalArgumentException e) {
            // A damaged entry, or one sealed with a key the keystore no longer has.
            return "failed\tundecryptable\t" + reason(e);
        } catch (RuntimeException e) {
            return "failed\terror\t" + reason(e);
        }
    }

    static synchronized String save(android.content.Context context, String account, String value) {
        try {
            java.io.File file = file(context, account);
            if (version(kept(file)) > VERSION) {
                return NEWER;
            }
            javax.crypto.Cipher cipher = javax.crypto.Cipher.getInstance(CIPHER);
            // The keystore chooses the nonce, a new one for each secret.
            cipher.init(javax.crypto.Cipher.ENCRYPT_MODE, key(true));
            // Bound to its account: an entry copied under another account doesn't decrypt.
            cipher.updateAAD(account.getBytes(java.nio.charset.StandardCharsets.UTF_8));
            byte[] sealed = cipher.doFinal(value.getBytes(java.nio.charset.StandardCharsets.UTF_8));
            String kept = VERSION + "." + android.util.Base64.encodeToString(cipher.getIV(), android.util.Base64.NO_WRAP) + "."
                    + android.util.Base64.encodeToString(sealed, android.util.Base64.NO_WRAP);
            return write(file, kept);
        } catch (java.security.KeyStoreException | java.security.NoSuchProviderException | java.io.IOException
                | java.security.ProviderException e) {
            return "unavailable\t" + reason(e);
        } catch (java.security.GeneralSecurityException | RuntimeException e) {
            return "failed\terror\t" + reason(e);
        }
    }

    static synchronized String delete(android.content.Context context, String account) {
        try {
            java.io.File file = file(context, account);
            String before = kept(file);
            if (version(before) > VERSION) {
                return NEWER;
            }
            return before == null || file.delete() || !file.exists() ? "ok" : UNWRITTEN;
        } catch (java.io.IOException | RuntimeException e) {
            return "failed\terror\t" + reason(e);
        }
    }

    /**
     * Writes the account's entry whole to a file beside its own and renames it over it, so the
     * entry is either the new one or, after a failure, still the one before.
     */
    private static String write(java.io.File file, String kept) {
        java.io.File folder = file.getParentFile();
        java.io.File part = new java.io.File(folder, file.getName() + ".part");
        try {
            if (folder == null || !(folder.isDirectory() || folder.mkdirs())) {
                return UNWRITTEN;
            }
            try (java.io.FileOutputStream out = new java.io.FileOutputStream(part)) {
                out.write(kept.getBytes(java.nio.charset.StandardCharsets.US_ASCII));
                // On the disk before the rename makes it the entry.
                out.getFD().sync();
            }
            java.nio.file.Files.move(part.toPath(), file.toPath(), java.nio.file.StandardCopyOption.ATOMIC_MOVE);
            // The rename on the disk too. It is made whatever this comes to: only a power cut
            // before Android writes the folder could still undo it.
            try (java.nio.channels.FileChannel dir =
                    java.nio.channels.FileChannel.open(folder.toPath(), java.nio.file.StandardOpenOption.READ)) {
                dir.force(true);
            } catch (java.io.IOException | RuntimeException e) {
                android.util.Log.w("rustgis", "the secrets' folder wasn't synced: " + reason(e));
            }
            return "ok";
        } catch (java.io.IOException | RuntimeException e) {
            part.delete();
            return UNWRITTEN + "\t" + reason(e);
        }
    }

    /** The account's entry, or null when there is none. */
    private static String kept(java.io.File file) throws java.io.IOException {
        if (!file.exists()) {
            return null;
        }
        byte[] bytes = java.nio.file.Files.readAllBytes(file.toPath());
        return new String(bytes, java.nio.charset.StandardCharsets.US_ASCII);
    }

    /** The account's file: its name a SHA-256 of the account, as an account may hold any character. */
    private static java.io.File file(android.content.Context context, String account) {
        byte[] digest;
        try {
            digest = java.security.MessageDigest.getInstance("SHA-256").digest(account.getBytes(java.nio.charset.StandardCharsets.UTF_8));
        } catch (java.security.NoSuchAlgorithmException e) {
            // Every Android has SHA-256.
            throw new IllegalStateException(e);
        }
        StringBuilder name = new StringBuilder();
        for (byte b : digest) {
            name.append(String.format(java.util.Locale.ROOT, "%02x", b));
        }
        return new java.io.File(new java.io.File(context.getApplicationContext().getNoBackupFilesDir(), FOLDER), name.toString());
    }

    /** The version an entry was written in: the number before its first point, 0 if none. */
    private static int version(String kept) {
        if (kept == null) {
            return 0;
        }
        int dot = kept.indexOf('.');
        try {
            return dot <= 0 ? 0 : Integer.parseInt(kept.substring(0, dot));
        } catch (NumberFormatException e) {
            return 0;
        }
    }

    /** The keystore's key, made the first time it is asked for when {@code make}; else null when there is none. */
    private static synchronized javax.crypto.SecretKey key(boolean make) throws java.security.GeneralSecurityException, java.io.IOException {
        java.security.KeyStore store = java.security.KeyStore.getInstance("AndroidKeyStore");
        store.load(null);
        java.security.Key key = store.getKey(KEY, null);
        if (key instanceof javax.crypto.SecretKey) {
            return (javax.crypto.SecretKey) key;
        }
        if (!make) {
            return null;
        }
        javax.crypto.KeyGenerator maker = javax.crypto.KeyGenerator.getInstance(
                android.security.keystore.KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
        maker.init(new android.security.keystore.KeyGenParameterSpec.Builder(KEY,
                android.security.keystore.KeyProperties.PURPOSE_ENCRYPT | android.security.keystore.KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(android.security.keystore.KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(android.security.keystore.KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build());
        return maker.generateKey();
    }

    /** What went wrong, on one line. */
    private static String reason(Throwable e) {
        String message = e.getMessage() == null ? e.getClass().getSimpleName() : e.getMessage();
        return message.replaceAll("[\\t\\r\\n]", " ");
    }
}
