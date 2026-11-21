package sh.amux.push

import java.security.GeneralSecurityException
import java.security.KeyFactory
import java.security.SecureRandom
import java.security.spec.PKCS8EncodedKeySpec
import java.security.spec.X509EncodedKeySpec
import javax.crypto.Cipher
import javax.crypto.KeyAgreement
import javax.crypto.Mac
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * Pure JVM RFC 9180 HPKE Base-mode implementation for the suite used by amux pushes:
 * DHKEM(X25519, HKDF-SHA256) / HKDF-SHA256 / ChaCha20-Poly1305.
 */
internal object Hpke {
    const val KEY_SIZE: Int = 32
    const val NONCE_SIZE: Int = 12
    const val TAG_SIZE: Int = 16
    private const val HASH_SIZE: Int = 32
    private const val MODE_BASE: Byte = 0x00

    private val HPKE_V1 = "HPKE-v1".toByteArray(Charsets.US_ASCII)
    private val KEM_SUITE_ID = byteArrayOf(
        'K'.code.toByte(),
        'E'.code.toByte(),
        'M'.code.toByte(),
        0x00,
        0x20,
    )
    private val HPKE_SUITE_ID = byteArrayOf(
        'H'.code.toByte(),
        'P'.code.toByte(),
        'K'.code.toByte(),
        'E'.code.toByte(),
        0x00,
        0x20, // DHKEM(X25519, HKDF-SHA256)
        0x00,
        0x01, // HKDF-SHA256
        0x00,
        0x03, // ChaCha20Poly1305
    )

    private val EMPTY = ByteArray(0)
    private val ZERO_SALT = ByteArray(HASH_SIZE)

    // RFC 8410 DER headers for X25519 keys (OID 1.3.101.110).
    // PKCS8EncodedKeySpec / X509EncodedKeySpec work portably on both host
    // OpenJDK (SunEC) and Android Conscrypt (OpenSSLXDHKeyFactory), whereas
    // XECPrivateKeySpec / XECPublicKeySpec are rejected by Conscrypt.
    private val X25519_PKCS8_PREFIX = byteArrayOf(
        0x30, 0x2E, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06,
        0x03, 0x2B, 0x65, 0x6E, 0x04, 0x22, 0x04, 0x20,
    )
    private val X25519_X509_PREFIX = byteArrayOf(
        0x30, 0x2A, 0x30, 0x05, 0x06, 0x03, 0x2B, 0x65,
        0x6E, 0x03, 0x21, 0x00,
    )
    private val X25519_BASEPOINT = ByteArray(KEY_SIZE).apply { this[0] = 9 }
    private val LABEL_EAE_PRK = "eae_prk".toByteArray(Charsets.US_ASCII)
    private val LABEL_SHARED_SECRET = "shared_secret".toByteArray(Charsets.US_ASCII)
    private val LABEL_PSK_ID_HASH = "psk_id_hash".toByteArray(Charsets.US_ASCII)
    private val LABEL_INFO_HASH = "info_hash".toByteArray(Charsets.US_ASCII)
    private val LABEL_SECRET = "secret".toByteArray(Charsets.US_ASCII)
    private val LABEL_KEY = "key".toByteArray(Charsets.US_ASCII)
    private val LABEL_BASE_NONCE = "base_nonce".toByteArray(Charsets.US_ASCII)

    private val secureRandom = SecureRandom()

    fun generatePrivateKey(): ByteArray = ByteArray(KEY_SIZE).also(secureRandom::nextBytes)

    /**
     * Derives the 32-byte RFC 7748 little-endian X25519 public key for [privateKey].
     */
    fun deriveX25519PublicKey(privateKey: ByteArray): ByteArray {
        require(privateKey.size == KEY_SIZE) { "X25519 private key must be $KEY_SIZE bytes" }
        val keyFactory = KeyFactory.getInstance("X25519")
        val priv = keyFactory.generatePrivate(PKCS8EncodedKeySpec(concat(X25519_PKCS8_PREFIX, privateKey)))
        val base = keyFactory.generatePublic(X509EncodedKeySpec(concat(X25519_X509_PREFIX, X25519_BASEPOINT)))
        val agreement = KeyAgreement.getInstance("X25519")
        agreement.init(priv)
        agreement.doPhase(base, true)
        return agreement.generateSecret()
    }

    /**
     * Single-shot HPKE Base-mode decryption (`OpenBase`).
     */
    fun openBase(
        encapsulatedKey: ByteArray,
        recipientPrivateKey: ByteArray,
        info: ByteArray,
        ciphertext: ByteArray,
        aad: ByteArray = EMPTY,
    ): ByteArray {
        if (encapsulatedKey.size != KEY_SIZE) {
            throw GeneralSecurityException("encapsulated key must be $KEY_SIZE bytes")
        }
        if (recipientPrivateKey.size != KEY_SIZE) {
            throw GeneralSecurityException("private key must be $KEY_SIZE bytes")
        }
        if (ciphertext.size < TAG_SIZE) {
            throw GeneralSecurityException("ciphertext shorter than Poly1305 tag")
        }

        val dh = x25519(recipientPrivateKey, encapsulatedKey)
        val recipientPublicKey = deriveX25519PublicKey(recipientPrivateKey)
        val kemContext = concat(encapsulatedKey, recipientPublicKey)
        val sharedSecret = extractAndExpand(dh, kemContext)

        val (key, baseNonce) = keyScheduleBase(sharedSecret, info)
        val cipher = Cipher.getInstance("ChaCha20-Poly1305")
        cipher.init(Cipher.DECRYPT_MODE, SecretKeySpec(key, "ChaCha20"), IvParameterSpec(baseNonce))
        if (aad.isNotEmpty()) {
            cipher.updateAAD(aad)
        }
        return cipher.doFinal(ciphertext)
    }

    /**
     * Single-shot HPKE Base-mode encryption (`SealBase`), returning `enc || ciphertext`.
     */
    fun sealBase(
        recipientPublicKey: ByteArray,
        info: ByteArray,
        plaintext: ByteArray,
        aad: ByteArray = EMPTY,
        ephemeralPrivateKey: ByteArray = generatePrivateKey(),
    ): ByteArray {
        if (recipientPublicKey.size != KEY_SIZE) {
            throw GeneralSecurityException("recipient public key must be $KEY_SIZE bytes")
        }
        if (ephemeralPrivateKey.size != KEY_SIZE) {
            throw GeneralSecurityException("ephemeral private key must be $KEY_SIZE bytes")
        }

        val encapsulatedKey = deriveX25519PublicKey(ephemeralPrivateKey)
        val dh = x25519(ephemeralPrivateKey, recipientPublicKey)
        val kemContext = concat(encapsulatedKey, recipientPublicKey)
        val sharedSecret = extractAndExpand(dh, kemContext)

        val (key, baseNonce) = keyScheduleBase(sharedSecret, info)
        val cipher = Cipher.getInstance("ChaCha20-Poly1305")
        cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "ChaCha20"), IvParameterSpec(baseNonce))
        if (aad.isNotEmpty()) {
            cipher.updateAAD(aad)
        }
        val ciphertext = cipher.doFinal(plaintext)
        return concat(encapsulatedKey, ciphertext)
    }

    private fun x25519(privateKey: ByteArray, peerPublicKey: ByteArray): ByteArray {
        val keyFactory = KeyFactory.getInstance("X25519")
        val priv = keyFactory.generatePrivate(PKCS8EncodedKeySpec(concat(X25519_PKCS8_PREFIX, privateKey)))
        val pub = keyFactory.generatePublic(X509EncodedKeySpec(concat(X25519_X509_PREFIX, peerPublicKey)))
        val agreement = KeyAgreement.getInstance("X25519")
        agreement.init(priv)
        agreement.doPhase(pub, true)
        val secret = agreement.generateSecret()
        // RFC 7748 §6.1 / RFC 9180 §7.1.4: reject all-zero shared secrets.
        var nonZero = 0
        for (b in secret) {
            nonZero = nonZero or (b.toInt() and 0xFF)
        }
        if (nonZero == 0) {
            throw GeneralSecurityException("X25519 produced an all-zero shared secret")
        }
        return secret
    }

    private fun extractAndExpand(dh: ByteArray, kemContext: ByteArray): ByteArray {
        val eaePrk = labeledExtract(KEM_SUITE_ID, EMPTY, LABEL_EAE_PRK, dh)
        return labeledExpand(KEM_SUITE_ID, eaePrk, LABEL_SHARED_SECRET, kemContext, KEY_SIZE)
    }

    private fun keyScheduleBase(sharedSecret: ByteArray, info: ByteArray): Pair<ByteArray, ByteArray> {
        val pskIdHash = labeledExtract(HPKE_SUITE_ID, EMPTY, LABEL_PSK_ID_HASH, EMPTY)
        val infoHash = labeledExtract(HPKE_SUITE_ID, EMPTY, LABEL_INFO_HASH, info)
        val keyScheduleContext = concat(byteArrayOf(MODE_BASE), pskIdHash, infoHash)

        val secret = labeledExtract(HPKE_SUITE_ID, sharedSecret, LABEL_SECRET, EMPTY)
        val key = labeledExpand(HPKE_SUITE_ID, secret, LABEL_KEY, keyScheduleContext, KEY_SIZE)
        val baseNonce = labeledExpand(HPKE_SUITE_ID, secret, LABEL_BASE_NONCE, keyScheduleContext, NONCE_SIZE)
        return key to baseNonce
    }

    private fun labeledExtract(
        suiteId: ByteArray,
        salt: ByteArray,
        label: ByteArray,
        ikm: ByteArray,
    ): ByteArray {
        val labeledIkm = concat(HPKE_V1, suiteId, label, ikm)
        return hkdfExtract(salt, labeledIkm)
    }

    private fun labeledExpand(
        suiteId: ByteArray,
        prk: ByteArray,
        label: ByteArray,
        info: ByteArray,
        length: Int,
    ): ByteArray {
        val lenBytes = byteArrayOf(
            ((length ushr 8) and 0xFF).toByte(),
            (length and 0xFF).toByte(),
        )
        val labeledInfo = concat(lenBytes, HPKE_V1, suiteId, label, info)
        return hkdfExpand(prk, labeledInfo, length)
    }

    private fun hkdfExtract(salt: ByteArray, ikm: ByteArray): ByteArray {
        val effectiveSalt = if (salt.isEmpty()) ZERO_SALT else salt
        return hmacSha256(effectiveSalt, ikm)
    }

    private fun hkdfExpand(prk: ByteArray, info: ByteArray, length: Int): ByteArray {
        require(length in 0..(255 * HASH_SIZE)) { "HKDF length out of range: $length" }
        val result = ByteArray(length)
        var offset = 0
        var counter = 1
        var previous = EMPTY
        while (offset < length) {
            val mac = Mac.getInstance("HmacSHA256")
            mac.init(SecretKeySpec(prk, "HmacSHA256"))
            mac.update(previous)
            mac.update(info)
            mac.update(counter.toByte())
            previous = mac.doFinal()
            val toCopy = minOf(HASH_SIZE, length - offset)
            System.arraycopy(previous, 0, result, offset, toCopy)
            offset += toCopy
            counter++
        }
        return result
    }

    private fun hmacSha256(key: ByteArray, data: ByteArray): ByteArray {
        val mac = Mac.getInstance("HmacSHA256")
        mac.init(SecretKeySpec(key, "HmacSHA256"))
        return mac.doFinal(data)
    }

    private fun concat(vararg parts: ByteArray): ByteArray {
        var total = 0
        for (part in parts) total += part.size
        val out = ByteArray(total)
        var offset = 0
        for (part in parts) {
            System.arraycopy(part, 0, out, offset, part.size)
            offset += part.size
        }
        return out
    }
}
