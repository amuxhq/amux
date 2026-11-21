package sh.amux.features

import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag

/**
 * Semantic `Modifier.testTag(...)` constants for Ask, Overview, Chat Settings,
 * and Review composables.
 */
object AskReviewTags {
    // Ask card
    const val ASK_CARD: String = "ask"
    const val ASK_SENDING: String = "ask.sending"
    const val ASK_REJECTED: String = "ask.rejected"
    const val ASK_NOT_CONFIRMED: String = "ask.not-confirmed"
    const val ASK_DISMISSED: String = "ask.dismissed"
    const val ASK_POSITION: String = "ask.position"
    const val ASK_MORE: String = "ask.more"
    const val ASK_INTERRUPT_MENU: String = "ask.interrupt.menu"
    const val ASK_STOP: String = "ask.stop"
    const val ASK_RESEND: String = "ask.resend"
    const val ASK_DISCARD: String = "ask.discard"
    const val ASK_SUBJECT: String = "ask.subject"
    const val ASK_DIFF_TOGGLE: String = "ask.diff"
    const val ASK_DENY: String = "ask.deny"
    const val ASK_NOTE: String = "ask.note"
    const val ASK_NOTE_BACK: String = "ask.note.back"
    const val ASK_PLAN_SWITCH: String = "ask.plan.auto"
    const val ASK_PLAN_SWITCHES: String = "ask.plan.switches"
    const val ASK_PLAN_APPROVE: String = "ask.approve"
    const val ASK_SUBMIT: String = "ask.submit"
    const val ASK_LINK_OPEN: String = "ask.open"
    const val ASK_GRANT: String = "ask.grant"
    const val ASK_GRANT_TURN: String = "ask.grant.turn"
    const val ASK_GRANT_SESSION: String = "ask.grant.session"
    const val ASK_QUESTION: String = "ask.question"
    const val ASK_PREVIEW: String = "ask.preview"
    const val ASK_OPTION_OTHER: String = "ask.option.other"
    const val ASK_OTHER_INPUT: String = "ask.other"
    const val ASK_SKIP: String = "ask.skip"
    const val ASK_NEXT: String = "ask.next"
    const val ASK_SEND: String = "ask.send"
    const val ASK_STEP_REVIEW: String = "ask.step.review"
    const val ASK_REPLY_LINK: String = "ask.reply"
    const val ASK_REPLY_BACK: String = "ask.reply.back"
    const val ASK_REPLY_INPUT: String = "ask.reply.text"
    const val ASK_REPLY_SEND: String = "ask.reply.send"

    fun askChoice(index: Int): String = "ask.choice.$index"
    fun askStep(index: Int): String = "ask.step.$index"
    fun askOption(position: Int): String = "ask.option.$position"
    fun askAddNote(index: Int): String = "ask.addNote.$index"
    fun askQuestionNote(index: Int): String = "ask.note.$index"
    fun askReview(index: Int): String = "ask.review.$index"
    fun askField(name: String): String = "ask.field.$name"
    fun askFieldProblem(name: String): String = "ask.field.$name.problem"
    fun askFieldChoice(name: String, optionIndex: Int): String = "ask.field.$name.option.$optionIndex"

    // Chat overview
    const val OVERVIEW_SHEET: String = "chat.overview"
    const val OVERVIEW_CLOSE: String = "chat.overview.close"
    const val OVERVIEW_PLACE: String = "chat.place"
    const val OVERVIEW_STOP_DIALOG: String = "chat.overview.stop.dialog"
    const val OVERVIEW_STOP_CONFIRM: String = "chat.overview.stop"
    const val OVERVIEW_STOP_CANCEL: String = "chat.overview.stop-cancel"
    const val OVERVIEW_USAGE: String = "chat.overview.usage"
    const val OVERVIEW_CONTEXT: String = "chat.overview.context"
    const val OVERVIEW_REVIEW: String = "chat.overview.review"
    const val OVERVIEW_CHANGES_LOADING: String = "chat.overview.changes.loading"
    const val OVERVIEW_CHANGES_ERROR: String = "chat.overview.changes.error"
    const val OVERVIEW_CHANGES_RETRY: String = "chat.overview.changes.retry"
    const val OVERVIEW_CHANGES_NONE: String = "chat.overview.changes.none"

    fun overviewJob(index: Int): String = "chat.overview.job.$index"
    fun overviewJobStop(index: Int): String = "chat.overview.job.$index.stop"
    fun overviewServer(name: String): String = "chat.overview.server.$name"
    fun overviewUsageWindow(index: Int): String = "chat.overview.usage.$index"
    fun overviewComparison(name: String): String = "chat.overview.comparison.$name"
    fun overviewFile(path: String): String = "chat.overview.file.$path"

    // Chat settings
    const val CHAT_SETTINGS_SHEET: String = "chat.settings"
    const val CHAT_SETTINGS_CLOSE: String = "chat.settings.close"
    const val CHAT_SETTINGS_TYPING_HINT: String = "chat.settings.typing"
    const val CHAT_SETTINGS_CYCLE: String = "chat.settings.cycle"
    const val CHAT_SETTINGS_CYCLES_HINT: String = "chat.settings.mode.cycles"
    const val CHAT_SETTINGS_CONTEXT: String = "chat.settings.context"

    fun chatSettingsModel(value: String): String = "chat.settings.model.$value"
    fun chatSettingsEffort(value: String): String = "chat.settings.effort.$value"
    fun chatSettingsPermission(value: String): String = "chat.settings.mode.$value"
    fun chatSettingsMode(value: String): String = "chat.settings.workmode.$value"
    fun chatSettingsCommand(name: String): String = "chat.settings.command.$name"

    // Review page
    const val REVIEW_SCREEN: String = "review"
    const val REVIEW_BACK: String = "review.back"
    const val REVIEW_SEND: String = "review.send"
    const val REVIEW_LOADING: String = "review.loading"
    const val REVIEW_ERROR: String = "review.error"
    const val REVIEW_RETRY: String = "review.retry"
    const val REVIEW_EMPTY: String = "review.empty"
    const val REVIEW_FILE_COMMENT: String = "review.file.comment"
    const val REVIEW_COMMENT_SHEET: String = "review.comment.sheet"
    const val REVIEW_COMMENT_INPUT: String = "review.note"
    const val REVIEW_COMMENT_SAVE: String = "review.save"
    const val REVIEW_COMMENT_ADD: String = "review.add"
    const val REVIEW_COMMENT_DELETE: String = "review.delete"
    const val REVIEW_COMMENT_CANCEL: String = "review.cancel"

    fun reviewFileChip(index: Int): String = "review.file.$index"
    fun reviewLine(fileIndex: Int, hunkIndex: Int, lineIndex: Int): String =
        "review.line.$fileIndex.$hunkIndex.$lineIndex"
    fun reviewCommentThread(index: Int): String = "review.comment.thread.$index"
}
