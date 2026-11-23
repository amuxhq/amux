package sh.amux.words

/**
 * Shared copy formatting primitives for pure JVM wording helpers.
 */
object Words {
    const val MIDDLE_DOT: String = " · "

    fun joinFacts(vararg facts: String): String =
        facts.filter { it.isNotBlank() }.joinToString(MIDDLE_DOT)
}
