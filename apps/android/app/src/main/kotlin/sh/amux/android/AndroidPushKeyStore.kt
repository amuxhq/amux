package sh.amux.android

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.io.IOException
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.util.Locale
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import sh.amux.push.Hpke
import sh.amux.push.PushKeyFailure
import sh.amux.push.PushKeyStore
import sh.amux.push.PushPrivateKey

/**
 * Android Keystore-backed [PushKeyStore].
 *
 * Each profile's 32-byte X25519 private key is wrapped with an Android Keystore
 * AES-256-GCM master key and stored under `filesDir/amux/installation/push-keys`,
 * which `backup_rules.xml` and `data_extraction_rules.xml` already exclude from
 * cloud backup and device-to-device transfer.
 *
 * Any Keystore or I/O error raises [PushKeyFailure] rather than returning null,
 * so transient unavailability (for example before first unlock) never tricks
 * [sh.amux.core.runtime.FcmPushRegistrar] into generating a replacement key.
 */
class AndroidPushKeyStore(
    private val directory: File,
    private val cipherProvider: KeyCipher = AndroidKeyStoreCipher,
) : PushKeyStore {
    constructor(context: Context) : this(
        directory = File(File(File(context.applicationContext.filesDir, "amux"), "installation"), "push-keys"),
    )

    interface KeyCipher {
        @Throws(GeneralSecurityException::class)
        fun encrypt(plaintext: ByteArray): ByteArray

        @Throws(GeneralSecurityException::class)
        fun decrypt(blob: ByteArray): ByteArray
    }

    override fun key(host: String): PushPrivateKey? {
        val file = fileFor(host)
        if (!file.exists()) return null
        return try {
            val encrypted = file.readBytes()
            val raw = cipherProvider.decrypt(encrypted)
            if (raw.size != Hpke.KEY_SIZE) {
                throw PushKeyFailure("stored push key for $host has unexpected size ${raw.size}")
            }
            PushPrivateKey(raw)
        } catch (e: PushKeyFailure) {
            throw e
        } catch (e: GeneralSecurityException) {
            throw PushKeyFailure("could not decrypt push key for $host", e)
        } catch (e: IOException) {
            throw PushKeyFailure("could not read push key for $host", e)
        }
    }

    override fun makeKey(host: String): PushPrivateKey {
        val created = PushPrivateKey.generate()
        val file = fileFor(host)
        try {
            if (!directory.exists() && !directory.mkdirs() && !directory.isDirectory) {
                throw PushKeyFailure("could not create push key directory ${directory.absolutePath}")
            }
            val encrypted = cipherProvider.encrypt(created.rawRepresentation)
            val tmp = File(directory, "${file.name}.tmp")
            tmp.writeBytes(encrypted)
            if (!tmp.renameTo(file)) {
                file.writeBytes(encrypted)
                tmp.delete()
            }
            return created
        } catch (e: PushKeyFailure) {
            throw e
        } catch (e: GeneralSecurityException) {
            throw PushKeyFailure("could not encrypt push key for $host", e)
        } catch (e: IOException) {
            throw PushKeyFailure("could not store push key for $host", e)
        }
    }

    override fun removeKey(host: String) {
        val file = fileFor(host)
        if (file.exists() && !file.delete()) {
            throw PushKeyFailure("could not delete push key for $host")
        }
    }

    private fun fileFor(host: String): File {
        val normalized = host.trim().lowercase(Locale.ROOT)
        require(normalized.isNotEmpty() && HOST_NAME_PATTERN.matches(normalized)) {
            "invalid push key host identifier: $host"
        }
        return File(directory, "$normalized.key")
    }

    companion object {
        private val HOST_NAME_PATTERN = Regex("^[0-9a-f-]+$")
    }
}

internal object AndroidKeyStoreCipher : AndroidPushKeyStore.KeyCipher {
    private const val ANDROID_KEYSTORE = "AndroidKeyStore"
    private const val KEY_ALIAS = "sh.amux.android.push.master"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val IV_SIZE = 12
    private const val TAG_BITS = 128

    @Synchronized
    private fun masterKey(): SecretKey {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        val existing = keyStore.getKey(KEY_ALIAS, null) as? SecretKey
        if (existing != null) return existing

        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        val spec = KeyGenParameterSpec.Builder(
            KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .setRandomizedEncryptionRequired(true)
            .build()
        generator.init(spec)
        return generator.generateKey()
    }

    override fun encrypt(plaintext: ByteArray): ByteArray {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, masterKey())
        val iv = cipher.iv
        if (iv.size != IV_SIZE) {
            throw GeneralSecurityException("unexpected GCM IV size ${iv.size}")
        }
        val ciphertext = cipher.doFinal(plaintext)
        return ByteArray(IV_SIZE + ciphertext.size).apply {
            System.arraycopy(iv, 0, this, 0, IV_SIZE)
            System.arraycopy(ciphertext, 0, this, IV_SIZE, ciphertext.size)
        }
    }

    override fun decrypt(blob: ByteArray): ByteArray {
        if (blob.size <= IV_SIZE) {
            throw GeneralSecurityException("encrypted push key blob too short")
        }
        val iv = blob.copyOfRange(0, IV_SIZE)
        val ciphertext = blob.copyOfRange(IV_SIZE, blob.size)
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, masterKey(), GCMParameterSpec(TAG_BITS, iv))
        return cipher.doFinal(ciphertext)
    }
}
