package sh.amux.features

/**
 * Semantic `Modifier.testTag(...)` identifiers shared across screens, Robolectric
 * UI tests, and the debug driving door.
 */
object TestTags {
    // Shell & bottom navigation
    const val SHELL: String = "shell"
    const val TAB_AGENTS: String = "tab.agents"
    const val TAB_HOSTS: String = "tab.hosts"
    const val NAV_BACK: String = "nav.back"

    // Agents home
    const val HOME_SCREEN: String = "home"
    const val HOME_LIST: String = "home.list"
    const val HOME_SUBTITLE: String = "home.subtitle"
    const val HOME_FAB_NEW_AGENT: String = "home.fab.new-agent"
    const val HOME_SEARCH_TOGGLE: String = "home.search.toggle"
    const val HOME_SEARCH: String = "home.search"
    const val HOME_SEARCH_CLEAR: String = "home.search.clear"
    const val HOME_ACCOUNT_MENU: String = "home.account-menu"
    const val HOME_SETTLED_ROW: String = "home.settled-row"
    const val HOME_NO_MATCHES: String = "home.no-matches"
    const val HOME_NO_MATCHES_CLEAR: String = "home.no-matches.clear"
    const val HOME_EMPTY_FIRST_RUN: String = "home.empty.first-run"
    const val HOME_EMPTY_LOOKING: String = "home.empty.looking"
    const val HOME_EMPTY_SEND_LINK: String = "home.empty.send-link"
    const val HOME_EMPTY_SIGN_IN: String = "home.empty.sign-in"
    const val HOME_EMPTY_NO_AGENTS: String = "home.empty.no-agents"
    const val HOME_EMPTY_NEW_AGENT: String = "home.empty.new-agent"
    const val HOME_SETTLED_EMPTY: String = "settled.empty"
    const val HOME_CALLOUT_SUBSCRIBE_CARD: String = "home.callout.subscribe.card"
    const val HOME_CALLOUT_SUBSCRIBE: String = "home.subscribe"
    const val HOME_CALLOUT_SIGN_IN_CARD: String = "home.callout.sign-in.card"
    const val HOME_CALLOUT_SIGN_IN: String = "home.sign-in"
    const val HOME_CALLOUT_NOTIFICATIONS_CARD: String = "home.callout.notifications.card"
    const val HOME_CALLOUT_NOTIFICATIONS: String = "home.notifications"

    // Row actions sheet & dialogs
    const val HOME_ROW_ACTIONS_SHEET: String = "home.row-actions"
    const val HOME_ACTION_PIN: String = "home.action.pin"
    const val HOME_ACTION_UNPIN: String = "home.action.unpin"
    const val HOME_ACTION_RENAME: String = "home.action.rename"
    const val HOME_ACTION_SETTLE: String = "home.action.settle"
    const val HOME_ACTION_UNSETTLE: String = "home.action.unsettle"
    const val HOME_ACTION_CANCEL_SETTLE: String = "home.action.cancel-settle"
    const val HOME_ACTION_DELETE: String = "home.action.delete"
    const val HOME_RENAME_DIALOG: String = "home.rename.dialog"
    const val HOME_RENAME_INPUT: String = "home.rename.input"
    const val HOME_RENAME_CONFIRM: String = "home.rename.confirm"
    const val HOME_SETTLE_CONFIRM: String = "home.settle.confirm"
    const val HOME_DELETE_CONFIRM: String = "home.delete.confirm"

    // Account switcher sheet
    const val ACCOUNTS_SHEET: String = "accounts.sheet"
    const val ACCOUNTS_ADD: String = "accounts.add"
    const val ACCOUNTS_SIGN_IN: String = "accounts.sign-in"
    const val ACCOUNTS_SETTINGS: String = "accounts.settings"

    // Hosts / Computers tab
    const val HOSTS_SCREEN: String = "hosts"
    const val HOSTS_LIST: String = "hosts.list"
    const val HOSTS_SUBTITLE: String = "hosts.subtitle"
    const val HOSTS_EMPTY: String = "hosts.empty"
    const val HOSTS_SUBSCRIBE_CARD: String = "hosts.subscribe.card"
    const val HOSTS_SUBSCRIBE: String = "hosts.subscribe"
    const val HOST_REVOKE_BUTTON: String = "host.revoke"
    const val HOST_REVOKE_CONFIRM: String = "host.revoke.confirm"

    // Pairing sheet
    const val PAIRING_SHEET: String = "pairing.sheet"
    const val PAIRING_TITLE: String = "pairing.title"
    const val PAIRING_DIGITS: String = "pairing.digits"
    const val PAIRING_CHECKING: String = "pairing.checking"
    const val PAIRING_REFUSED: String = "pairing.refused"
    const val PAIRING_FINGERPRINT: String = "pairing.fingerprint"
    const val PAIRING_CONFIRM: String = "pairing.confirm"
    const val PAIRING_CANCEL: String = "pairing.cancel"
    const val PAIRING_CLOSE: String = "pairing.close"
    const val PAIRING_TRUSTED: String = "pairing.trusted"
    const val PAIRING_SIGN_IN: String = "pairing.sign-in"
    const val PAIRING_SUBSCRIBE: String = "pairing.subscribe"

    // Store failure
    const val STORE_FAILURE_SCREEN: String = "store-failure"
    const val STORE_FAILURE_TITLE: String = "store-failure.title"
    const val STORE_FAILURE_CAUSE: String = "store-failure.cause"
    const val STORE_FAILURE_RELAUNCH: String = "store-failure.relaunch"

    // Detail routes
    const val SCREEN_CONVERSATION: String = "screen.conversation"
    const val SCREEN_CHANGES: String = "screen.changes"
    const val SCREEN_HOST: String = "screen.host"
    const val SCREEN_NEW_AGENT: String = "screen.new-agent"
    const val SCREEN_SETTINGS: String = "screen.settings"
    const val SCREEN_SETTLED: String = "screen.settled"
    const val SCREEN_PAYWALL: String = "screen.paywall"

    fun homeRow(name: String): String = "home.row.$name"
    fun homeRowMenu(name: String): String = "home.row.$name.menu"
    fun homeFamily(name: String): String = "home.family.$name"
    fun homeFilter(kind: String): String = "home.filter.$kind"
    fun homeFound(name: String): String = "home.found.$name"
    fun homePair(name: String): String = "home.pair.$name"
    fun homeSubscribe(name: String): String = "home.subscribe.$name"

    fun accountRow(email: String): String = "accounts.row.$email"

    fun hostsRow(name: String): String = "hosts.row.$name"
    fun hostsFound(name: String): String = "hosts.found.$name"
    fun hostsPair(name: String): String = "hosts.pair.$name"
    fun hostsSubscribe(name: String): String = "hosts.subscribe.$name"
}
