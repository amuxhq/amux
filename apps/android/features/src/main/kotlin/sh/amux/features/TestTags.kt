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
    const val HOME_NEW_AGENT: String = "home.newAgent"
    const val HOME_SEARCH_TOGGLE: String = "home.search.toggle"
    const val HOME_SEARCH: String = "home.search"
    const val HOME_SEARCH_CLEAR: String = "home.search.clear"
    const val HOME_SEARCH_CLOSE: String = "home.search.close"
    const val HOME_ACCOUNTS: String = "home.accounts"
    const val HOME_SETTLED: String = "home.settled"
    const val HOME_NO_MATCHES: String = "home.no-matches"
    const val HOME_NO_MATCHES_CLEAR: String = "home.no-matches.clear"
    const val HOME_EMPTY_FIRST_RUN: String = "home.empty.first-run"
    const val HOME_EMPTY_LOOKING: String = "home.empty.looking"
    const val HOME_NETWORK_UNSEEN: String = "home.localNetwork.refused"
    const val HOME_NETWORK_UNSEEN_ACTION: String = "home.localNetwork.settings"
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
    const val HOME_RENAME_PROBLEM: String = "home.rename.problem"
    const val HOME_RENAME_CONFIRM: String = "home.rename.confirm"
    const val HOME_SETTLE_DIALOG: String = "home.settle.dialog"
    const val HOME_SETTLE_CONFIRM: String = "home.settle.confirm"
    const val HOME_DELETE_DIALOG: String = "home.delete.dialog"
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

    // Pairing sheet. It is one sheet here, named by what it shows as the
    // other clients name their pages: the code being entered and checked
    // (`pin`), the device that answered to be trusted or an invitation being
    // checked (`pair-confirm`), and an invitation that needs an account.
    const val PAIRING_CLOSE: String = "pair.close"
    const val PIN_SHEET: String = "pin"
    const val PIN_TITLE: String = "pin.title"
    const val PIN_CODE: String = "pin.code"
    const val PIN_CHECKING: String = "pin.checking"
    const val PIN_REFUSED: String = "pin.refused"
    const val PIN_EXPIRY: String = "pin.expiry"
    const val PIN_SUBSCRIBE: String = "pin.subscribe"
    const val PIN_NOT_NOW: String = "pin.notNow"
    const val PAIR_CONFIRM_SHEET: String = "pair-confirm"
    const val PAIR_CONFIRM_TITLE: String = "pair-confirm.title"
    const val PAIR_CONFIRM_NAME: String = "pair-confirm.name"
    const val PAIR_CONFIRM_EXPIRY: String = "pair-confirm.expiry"
    const val PAIR_CONFIRM_FINGERPRINT: String = "pair-confirm.fingerprint"
    const val PAIR_CONFIRM_TRUST: String = "pair-confirm.trust"
    const val PAIR_CONFIRM_ABANDON: String = "pair-confirm.abandon"
    const val PAIR_CONFIRM_TRUSTED: String = "pair-confirm.trusted"
    const val PAIR_CONFIRM_CHECKING: String = "pair-confirm.checking"
    const val PAIR_CONFIRM_REFUSED: String = "pair-confirm.refused"
    const val PAIR_CONFIRM_BACK: String = "pair-confirm.back"
    const val PAIR_NEEDS_ACCOUNT_SHEET: String = "pair-needs-account"
    const val PAIR_NEEDS_ACCOUNT_SIGN_IN: String = "pair-needs-account.signIn"
    const val PAIR_NEEDS_ACCOUNT_CANCEL: String = "pair-needs-account.cancel"

    // Store failure
    const val STORE_FAILURE_SCREEN: String = "store-failure"
    const val STORE_FAILURE_TITLE: String = "store-failure.title"
    const val STORE_FAILURE_CAUSE: String = "store-failure.cause"
    const val STORE_FAILURE_RELAUNCH: String = "store-failure.relaunch"

    // Detail routes
    const val SCREEN_CHAT: String = "chat"
    const val SCREEN_CHANGES: String = "screen.changes"
    const val SCREEN_HOST: String = "screen.host"
    const val SCREEN_NEW_AGENT: String = "screen.new-agent"
    const val SCREEN_SETTINGS: String = "you"
    const val SCREEN_SETTLED: String = "settled"
    const val SETTLED_BACK: String = "settled.back"
    const val SCREEN_PAYWALL: String = "screen.paywall"

    // Rows and offers are named by the id of the agent or device they stand
    // for, never its name: a name changes on rename and two devices may share
    // one, and the phone journeys address the same element on every client by
    // the id the topology handed out.

    /** An agent's row, on the Agents list and on the Settled page alike. */
    fun homeRow(agent: List<Int>): String = "home.row.${uuidText(agent)}"
    fun homeFamily(name: String): String = "home.family.$name"
    fun homeFilter(kind: String): String = "home.filter.$kind"
    fun homeFound(host: List<Int>): String = "home.found.${uuidText(host)}"
    fun homePair(host: List<Int>): String = "home.pair.${uuidText(host)}"
    fun homeSubscribe(host: List<Int>): String = "home.subscribe.${uuidText(host)}"

    /** An account in the switcher sheet, by its email as in Settings. */
    fun accountRow(email: String): String = "account.$email"

    fun hostsRow(host: List<Int>): String = "hosts.row.${uuidText(host)}"
    fun hostsFound(host: List<Int>): String = "hosts.found.${uuidText(host)}"
    fun hostsPair(host: List<Int>): String = "hosts.pair.${uuidText(host)}"
    fun hostsSubscribe(host: List<Int>): String = "hosts.subscribe.${uuidText(host)}"
}
