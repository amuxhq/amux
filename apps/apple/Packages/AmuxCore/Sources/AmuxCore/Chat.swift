import AmuxApp
import AmuxValues
import Foundation

/// One open chat on the shared runtime.
///
/// Rows are handed out by item key, which never moves: the owner holds a
/// sequence of keys that changes only at its two edges, fetches the rows an
/// update changed, and reads the keys again only when a change batch says
/// the sequence was reloaded. The sequence is the chat's window: while the
/// reader follows the newest row it keeps at most its cap and drops the
/// oldest rows as new ones arrive; while the reader is in history it stays
/// put, and what arrives is held apart until the reader returns. The owner
/// says which with `follow`. Close it before the runtime stops.
public final class Chat: ChatSource, @unchecked Sendable {
    private let handle: OpaquePointer
    private let loop: CoreLoop
    /// Held across every call into the library, so stopping waits for the
    /// calls in flight and none starts after it. Recursive, because a call
    /// may be made from inside another on the same thread.
    private let lock = NSRecursiveLock()
    private var open = true
    /// What the owner's wake names this chat by.
    public let id: UInt64

    init(handle: OpaquePointer, loop: CoreLoop) {
        self.handle = handle
        self.loop = loop
        self.id = amux_session_id(handle)
    }

    public func close() {
        let wasOpen = lock.withLock {
            defer { open = false }
            return open
        }
        guard wasOpen else { return }
        amux_session_close(handle)
    }

    deinit { close() }

    /// Runs a call on the open chat, or answers `closed` once it is not.
    private func call<T>(_ closed: T, _ body: (OpaquePointer) -> T) -> T {
        lock.withLock { open ? body(handle) : closed }
    }

    public func keys() -> [String] {
        call([]) { live in
            Bridge.read([String].self, amux_session_keys(live)) ?? []
        }
    }

    /// Keys newer than `newest`, or nil when it is no longer held.
    public func keys(above newest: String) -> [String]? {
        call(nil) { live in
            newest.withCString {
                Bridge.read([String]?.self, amux_session_new_keys_above(live, $0))
            } ?? nil
        }
    }

    /// Keys older than `oldest`, or nil when it is no longer held.
    public func keys(below oldest: String) -> [String]? {
        call(nil) { live in
            oldest.withCString {
                Bridge.read([String]?.self, amux_session_new_keys_below(live, $0))
            } ?? nil
        }
    }

    public func oldestKey() -> String? {
        call(nil) { live in
            Bridge.read(String?.self, amux_session_oldest_key(live)) ?? nil
        }
    }

    public func follow(_ following: Bool) {
        call(()) { live in amux_session_follow(live, following) }
    }

    public func rows(for keys: [String], options: RowOptions? = nil) -> [Row] {
        call([]) { live in
            Bridge.json(keys).withCString { keys in
                if let options {
                    return Bridge.json(options).withCString {
                        Bridge.read([Row].self, amux_session_rows_for(live, keys, $0))
                    }
                }
                return Bridge.read([Row].self, amux_session_rows_for(live, keys, nil))
            } ?? []
        }
    }

    public func toggleRun(_ member: String, open: [String]) -> [String] {
        call(open) { live in
            member.withCString { member in
                Bridge.json(open).withCString {
                    Bridge.read([String].self, amux_session_toggle_run(live, member, $0))
                }
            } ?? open
        }
    }

    public func keepOpenRuns(_ open: [String]) -> [String] {
        call(open) { live in
            Bridge.json(open).withCString {
                Bridge.read([String].self, amux_session_keep_open_runs(live, $0))
            } ?? open
        }
    }

    public func askCard() -> AskCard? {
        call(nil) { live in
            Bridge.read(AskCard?.self, amux_session_ask_card(live)) ?? nil
        }
    }

    public func overview() -> Overview? {
        call(nil) { live in
            Bridge.read(Overview.self, amux_session_overview(live))
        }
    }

    public func settings() -> SettingsView? {
        call(nil) { live in
            Bridge.read(SettingsView.self, amux_session_settings(live))
        }
    }

    public func frame() -> ChatFrame? {
        call(nil) { live in
            Bridge.read(ChatFrame.self, amux_session_frame(live))
        }
    }

    /// What changed since the last take; the next change wakes the owner.
    public func takeChanges() -> ChatChanges {
        let none = ChatChanges(keys: [], reloaded: false, session: false)
        return call(none) { live in
            Bridge.read(ChatChanges.self, amux_session_take_changes(live)) ?? none
        }
    }

    // MARK: - Acts

    public func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> {
        let draft = Bridge.json(draft)
        return await act(SendOutcome.self) { live in
            draft.withCString { amux_session_send(live, $0) }
        }
    }

    /// Answers the head ask by the position of a choice on its card.
    public func answer(
        _ ask: String, choice: Int, note: String? = nil
    ) async -> ActOutcome? {
        await value(ActOutcome.self) { live in
            ask.withCString { ask in
                Bridge.optional(note) { amux_session_answer(live, ask, UInt32(choice), $0) }
            }
        }
    }

    /// Answers a question card with one response per question, in order.
    public func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome? {
        let responses = Bridge.json(responses)
        return await value(ActOutcome.self) { live in
            ask.withCString { ask in
                responses.withCString { amux_session_answer_questions(live, ask, $0) }
            }
        }
    }

    /// Replies to a question card in the person's own words instead of
    /// answering, with what they had answered so far.
    public func replyInstead(
        _ ask: String, text: String, soFar: [QuestionResponse]
    ) async -> ActOutcome? {
        let soFar = Bridge.json(soFar)
        return await value(ActOutcome.self) { live in
            ask.withCString { ask in
                text.withCString { text in
                    soFar.withCString { amux_session_reply_instead(live, ask, text, $0) }
                }
            }
        }
    }

    /// Submits a form ask with the person's values, one per field in the
    /// form's order; the shared view checks and encodes them.
    public func answerForm(_ ask: String, values: [FormValue]) async -> ActOutcome? {
        let values = Bridge.json(values)
        return await value(ActOutcome.self) { live in
            ask.withCString { ask in
                values.withCString { amux_session_answer_form(live, ask, $0) }
            }
        }
    }

    public func withdraw(_ input: [UInt8]) async -> ActOutcome? {
        let input = Bridge.json(input)
        return await value(ActOutcome.self) { live in
            input.withCString { amux_session_withdraw(live, $0) }
        }
    }

    public func sendNow(_ input: [UInt8]) async -> ActOutcome? {
        let input = Bridge.json(input)
        return await value(ActOutcome.self) { live in
            input.withCString { amux_session_send_now(live, $0) }
        }
    }

    public func resend(_ input: [UInt8]) async -> SendOutcome? {
        let input = Bridge.json(input)
        return await value(SendOutcome?.self) { live in
            input.withCString { amux_session_resend(live, $0) }
        } ?? nil
    }

    public func draft(of input: [UInt8]) -> Draft? {
        call(nil) { live in
            Bridge.json(input).withCString {
                Bridge.read(Draft?.self, amux_session_draft_of(live, $0)) ?? nil
            }
        }
    }

    public func discard(_ input: [UInt8]) {
        call(()) { live in Bridge.json(input).withCString { amux_session_discard(live, $0) } }
    }

    public func interrupt() async -> ActOutcome? {
        await value(ActOutcome.self) { amux_session_interrupt($0) }
    }

    public func switchBack() async -> ActOutcome? {
        await value(ActOutcome.self) { amux_session_switch_back($0) }
    }

    /// Sends a pick from the settings view in the agent's kind.
    public func command(_ draft: Draft) async -> CommandOutcome? {
        let draft = Bridge.json(draft)
        return await value(CommandOutcome.self) { live in
            draft.withCString { amux_session_command(live, $0) }
        }
    }

    public func change(_ setting: SettingChange) async -> ActOutcome? {
        let setting = Bridge.json(setting)
        return await value(ActOutcome.self) { live in
            setting.withCString { amux_session_change_setting(live, $0) }
        }
    }

    /// Starts an exited agent again with the composer's draft.
    public func resume(with draft: Draft) async -> ActOutcome? {
        let draft = Bridge.json(draft)
        return await value(ActOutcome.self) { live in
            draft.withCString { amux_session_resume(live, $0) }
        }
    }

    public func pageOlder(_ rows: UInt32) async -> PageOutcome? {
        await value(PageOutcome.self) { amux_session_page_older($0, rows) }
    }

    /// The agent's diff for `comparison` as its host froze it, with its
    /// patch.
    public func review(
        _ comparison: Comparison
    ) async -> Result<FrozenReview, RuntimeFailure> {
        let comparison = Bridge.json(comparison)
        return await act(FrozenReview.self) { live in
            comparison.withCString { amux_session_review(live, $0) }
        }
    }

    /// Fetches the files changed for `comparison`, without a patch; the
    /// overview lists them from then on.
    public func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure> {
        let comparison = Bridge.json(comparison)
        return await act(Overview.self) { live in
            comparison.withCString { amux_session_open_overview(live, $0) }
        }
    }

    /// Stores an attachment's bytes, answering the reference a draft carries.
    public func putBlob(
        _ data: Data, name: String, mime: String
    ) async -> Result<BlobRef, RuntimeFailure> {
        await act(BlobRef.self) { live in
            data.withUnsafeBytes { bytes in
                name.withCString { name in
                    mime.withCString { mime in
                        amux_session_put_blob(
                            live, bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count,
                            name, mime)
                    }
                }
            }
        }
    }

    /// An attachment's bytes, where this phone holds them.
    public func blob(_ hash: [UInt8]) -> Data? {
        let bytes = call(AmuxBytes(data: nil, len: 0)) { live in
            Bridge.json(hash).withCString { amux_session_blob(live, $0) }
        }
        guard let data = bytes.data else { return nil }
        defer { amux_bytes_free(bytes) }
        return Data(bytes: data, count: bytes.len)
    }

    /// Starts an act on the core queue and waits for its `{"Ok": T}` or
    /// `{"Err": reason}`.
    private func act<T: Decodable & Sendable>(
        _ type: T.Type, _ start: @escaping @Sendable (OpaquePointer) -> UInt64
    ) async -> Result<T, RuntimeFailure> {
        Bridge.answer(T.self, await loop.answer { [self] _ in call(nil) { start($0) } })
    }

    /// Starts an act on the core queue and waits for its JSON that is the
    /// value itself.
    private func value<T: Decodable & Sendable>(
        _ type: T.Type, _ start: @escaping @Sendable (OpaquePointer) -> UInt64
    ) async -> T? {
        Bridge.value(T.self, await loop.answer { [self] _ in call(nil) { start($0) } })
    }
}
