import AmuxApp
import AmuxValues
import Foundation

/// One open chat on the shared runtime.
///
/// It opens whole, and then moves by the deltas the profile's steps hand
/// its owner. Rows are handed out by item key, which never moves, so the
/// sequence of keys changes only at its two edges until a delta reloads
/// it. The sequence is the chat's window: while the reader follows the
/// newest row it keeps at most its cap and drops the oldest rows as new
/// ones arrive; while the reader is in history it stays put, and what
/// arrives is held apart until the reader returns. The owner says which
/// with `follow`. Close it before the runtime stops.
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

    public func opening() -> ChatDelta {
        let closed = ChatDelta(
            chat: id, rows: [], ask: nil, frame: nil, keys: .reload([]), overview: nil,
            settings: nil)
        return call(closed) { live in
            Bridge.read(ChatDelta.self, amux_session_opening(live)) ?? closed
        }
    }

    public func follow(_ following: Bool) {
        call(()) { live in amux_session_follow(live, following) }
    }

    public func keptDraft() -> Draft? {
        call(nil) { live in Bridge.read(Draft?.self, amux_session_kept_draft(live)) ?? nil }
    }

    /// Keeps the draft with this phone's daemon for the profile; saves go in
    /// order and an empty draft is forgotten.
    public func keep(_ draft: Draft) {
        call(()) { live in Bridge.json(draft).withCString { amux_session_keep_draft(live, $0) } }
    }

    public func rows(for keys: [String]) -> [Row] {
        call([]) { live in
            Bridge.json(keys).withCString {
                Bridge.read([Row].self, amux_session_rows_for(live, $0))
            } ?? []
        }
    }

    public func toggleRun(_ member: String) {
        call(()) { live in member.withCString { amux_session_toggle_run(live, $0) } }
    }

    // MARK: - Acts

    public func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> {
        let act = Bridge.json(ChatAct.send(draft))
        return await self.act(SendOutcome.self) { live in act.withCString { amux_session_act(live, $0) } }
    }

    /// Answers the head ask by the position of a choice on its card.
    public func answer(
        _ ask: String, choice: Int, note: String? = nil
    ) async -> ActOutcome? {
        await value(ActOutcome.self, .answer(ask: ask, choice: UInt32(choice), note: note ?? ""))
    }

    /// Answers a question card with one response per question, in order.
    public func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome? {
        await value(ActOutcome.self, .answerQuestions(ask: ask, responses: responses))
    }

    /// Replies to a question card in the person's own words instead of
    /// answering, with what they had answered so far.
    public func replyInstead(
        _ ask: String, text: String, soFar: [QuestionResponse]
    ) async -> ActOutcome? {
        await value(ActOutcome.self, .replyInstead(ask: ask, reply: text, soFar: soFar))
    }

    /// Submits a form ask with the person's values, one per field in the
    /// form's order; the shared view checks and encodes them.
    public func answerForm(_ ask: String, values: [FormValue]) async -> ActOutcome? {
        await value(ActOutcome.self, .answerForm(ask: ask, values: values))
    }

    public func withdraw(_ input: [UInt8]) async -> ActOutcome? {
        await value(ActOutcome.self, .withdraw(input))
    }

    public func sendNow(_ input: [UInt8]) async -> ActOutcome? {
        await value(ActOutcome.self, .sendNow(input))
    }

    public func resend(_ input: [UInt8]) async -> SendOutcome? {
        await value(SendOutcome?.self, .resend(input)) ?? nil
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
        await value(ActOutcome.self, .interrupt)
    }

    public func stopJob(_ key: String) async -> ActOutcome? {
        await value(ActOutcome.self, .stopJob(key))
    }

    public func switchBack() async -> ActOutcome? {
        await value(ActOutcome.self, .switchBack)
    }

    /// Sends a pick from the settings view in the agent's kind.
    public func command(_ draft: Draft) async -> CommandOutcome? {
        await value(CommandOutcome.self, .command(draft))
    }

    public func change(_ setting: SettingChange) async -> ActOutcome? {
        await value(ActOutcome.self, .changeSetting(setting))
    }

    /// Starts an exited agent again with the composer's draft.
    public func resume(with draft: Draft) async -> ActOutcome? {
        await value(ActOutcome.self, .resume(draft))
    }

    public func pageOlder(_ rows: UInt32) async -> PageOutcome? {
        await value(PageOutcome.self, .pageOlder(rows))
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

    /// Starts a chat act on the core queue and waits for its JSON that is
    /// the value itself.
    private func value<T: Decodable & Sendable>(_ type: T.Type, _ act: ChatAct) async -> T? {
        let act = Bridge.json(act)
        return Bridge.value(T.self, await loop.answer { [self] _ in
            call(nil) { live in act.withCString { amux_session_act(live, $0) } }
        })
    }
}
