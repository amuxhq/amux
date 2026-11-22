package sh.amux.core.runtime

/** Thrown across JNI when the Rust bridge reports an error or catches a panic. */
class BridgeException(message: String) : RuntimeException(message)
