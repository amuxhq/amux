import AmuxValues
import AmuxWords
import SwiftUI

/// Above the composer while the agent's account needs signing in: who must
/// sign in, the account and what went wrong, how to sign in on the agent's
/// host, and that sending tries again. Nothing in amux can sign the
/// provider in, so the composer under it still sends, and the provider's
/// next good answer takes the notice away.
struct SignInNotice: View {
    let signIn: SignInView
    let host: String

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Icon("circle-alert", size: 15).foregroundStyle(Palette.caution)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 3) {
                Text(ChatWords.needsSignIn(signIn.providerName)).font(.ui(.body, weight: .medium))
                Text(ChatWords.signInDetail(signIn, host: host))
                    .font(.ui(.meta)).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 11)
        .background(RoundedRectangle(cornerRadius: Radius.composer, style: .continuous).fill(Palette.raised))
        .overlay(RoundedRectangle(cornerRadius: Radius.composer, style: .continuous).strokeBorder(Palette.line))
    }
}
