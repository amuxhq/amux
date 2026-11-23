package sh.amux.design

/**
 * The two resolved appearances every screen is drawn and captured in.
 */
enum class Appearance(val wireName: String) {
    Light("light"),
    Dark("dark"),
    ;

    val isDark: Boolean
        get() = this == Dark
}

/**
 * User-selectable appearance preference, including following the system setting.
 */
enum class AppearanceMode(val wireName: String) {
    System("system"),
    Light("light"),
    Dark("dark"),
    ;

    fun resolve(systemDark: Boolean): Appearance = when (this) {
        System -> if (systemDark) Appearance.Dark else Appearance.Light
        Light -> Appearance.Light
        Dark -> Appearance.Dark
    }
}
