package sh.amux.words

/** How an account row describes itself under its name. */
object AccountWords {
    const val SIGNED_OUT: String = "Signed out"

    /**
     * The second line of an account row: signed out, else how many devices
     * it has paired, else its address while nothing is paired yet.
     */
    fun line(signedIn: Boolean, computers: Int?, email: String): String {
        if (!signedIn) return SIGNED_OUT
        if (computers == null || computers <= 0) return email
        return if (computers == 1) "1 device" else "$computers devices"
    }
}
