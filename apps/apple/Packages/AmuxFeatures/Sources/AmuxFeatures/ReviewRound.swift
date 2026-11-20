import AmuxCore
import AmuxDesign
import AmuxWords
import SwiftUI

/// The round a review sent into the chat closed, raised over the chat from
/// its chip: each comment it sent on its file, with the code it was written
/// on as stored with it, read from the agent's computer. When the computer
/// no longer holds the round, as after starting over, it shows the text the
/// agent read. The review itself, as it stands now, is one tap away.
public struct ReviewRoundSheet: View {
    @Environment(\.design) private var design
    let round: SentRound
    /// The round's page from the agent's computer; nil once it holds none
    /// of the round.
    let read: () async -> ReviewPage?
    let openReview: () -> Void
    let close: () -> Void
    @State private var asked = false
    @State private var page: ReviewPage?

    public init(
        round: SentRound, page: ReviewPage? = nil, read: @escaping () async -> ReviewPage?,
        openReview: @escaping () -> Void, close: @escaping () -> Void
    ) {
        self.round = round
        self.read = read
        self.openReview = openReview
        self.close = close
        _page = State(initialValue: page)
        _asked = State(initialValue: page != nil)
    }

    public var body: some View {
        NavigationStack {
            ZStack(alignment: .bottom) {
                Ground()
                content
                if asked { foot }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(String(localized: "Done"), action: close)
                        .identified("review.round.done", label: String(localized: "Done"))
                }
            }
        }
        .task {
            guard !asked else { return }
            page = await read()
            asked = true
        }
        .accessibilityElement(children: .contain)
        .reported("review.round", value: !asked ? "reading" : (page == nil ? "text" : "round"))
    }

    /// "Round 2", as the agent's computer numbered it when the message
    /// arrived; the chip's own guess until the round is read.
    private var title: String {
        let number = page?.rows.lazy.compactMap { row -> UInt32? in
            if case .comment(let comment) = row.of, case .sent(let round) = comment.state { return round }
            return nil
        }.first
        return ChatWords.reviewRound(number ?? round.sent.round)
    }

    @ViewBuilder
    private var content: some View {
        if !asked {
            PagePlaceholder(.waiting(ChatWords.reviewOpening), identifier: "review.round")
        } else if let page {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                    VStack(alignment: .leading, spacing: Space.s) {
                        ForEach(Array(page.banners.enumerated()), id: \.offset) { _, banner in
                            ReviewBannerCard(banner: banner)
                        }
                    }
                    .padding(.horizontal, design.metrics.gutter)
                    .padding(.vertical, page.banners.isEmpty ? 0 : Space.s)
                    ForEach(ReviewSections(page.rows).all) { section in
                        Section {
                            ForEach(section.rows, id: \.self) { index in row(page.rows[index]) }
                        } header: {
                            if let head = section.head, case .file(let file) = page.rows[head].of {
                                ReviewFileHeadRow(head: file, tally: false)
                                    .identified("review.round.file", label: file.path)
                            }
                        }
                    }
                }
                .padding(.bottom, 96)
            }
        } else {
            ScrollView {
                VStack(alignment: .leading, spacing: Space.m) {
                    Explain(ChatWords.reviewRoundGone)
                        .identified("review.round.gone", label: ChatWords.reviewRoundGone)
                    Text(verbatim: round.text)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.ink.color)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .fixedSize(horizontal: false, vertical: true)
                        .identified("review.round.text", label: round.text)
                }
                .padding(.horizontal, design.metrics.gutter)
                .padding(.top, Space.m)
                .padding(.bottom, 96)
            }
        }
    }

    /// A round's rows are its comments under their files' heads, and the
    /// code stored with each; drawn as the page draws them, to read only.
    @ViewBuilder
    private func row(_ row: ReviewRow) -> some View {
        switch row.of {
        case .comment(let comment):
            ReviewCommentCard(comment: comment)
                .identified("review.round.comment", label: comment.text, value: ReviewWords.commentValue(comment))
        case .line(let line):
            ReviewCodeLineRow(line: line)
        case .hunk(let hunk):
            ReviewHunkBand(hunk: hunk)
        case .noLines(_, let why):
            ReviewNote(text: ChatWords.reviewNoLines(why))
        case .file, .gap, .loading:
            EmptyView()
        }
    }

    private var foot: some View {
        Button {
            close()
            openReview()
        } label: {
            Text(ChatWords.reviewOpenReview)
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
                .frame(maxWidth: .infinity)
                .frame(height: 50)
                .frosted(Capsule(), as: .glass)
        }
        .buttonStyle(.amuxControl)
        .accessibilityHint("Shows the agent’s changes as they are now")
        .identified("review.round.open", label: ChatWords.reviewOpenReview)
        .padding(.horizontal, design.metrics.gutter)
        .padding(.bottom, 8)
    }
}
