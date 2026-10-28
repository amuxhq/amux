import AmuxCore
import AmuxFeatures
import SwiftUI
import UIKit

/// Reporting a problem, over the whole app, in every build.
///
/// Two ways in and no others. The system says a screenshot was taken, and the
/// app freezes its own frame and offers to report it; or somebody goes to Help
/// and asks. Nothing is shaken: a shake is already undo, and a phone in a
/// pocket shakes all day.
struct ReportTools: ViewModifier {
    let composition: Composition
    @State private var layers: ReportLayers

    init(composition: Composition) {
        self.composition = composition
        _layers = State(initialValue: ReportLayers { ReportLayer(composition: composition) })
    }

    func body(content: Content) -> some View {
        content
            .environment(\.reportLayers, layers)
            .onReceive(NotificationCenter.default.publisher(
                for: UIApplication.userDidTakeScreenshotNotification)
            ) { _ in
                composition.reports.offer(composition.freezer)
            }
    }
}

/// The offer and the report, as the top surface on screen draws them.
private struct ReportLayer: View {
    let composition: Composition

    var body: some View {
        let reports = composition.reports
        Color.clear
            .reportOffer(
                reports.offering,
                take: { reports.accept() },
                dismiss: { reports.dismiss() })
            .overlay {
                if reports.open {
                    ReportScreen(
                        model: reports,
                        signedIn: composition.accounts.selectedAccount?.signedIn == true
                    ) { asked in
                        switch asked {
                        case .cancel: reports.dismiss()
                        case .send: composition.sendReport()
                        }
                    }
                }
            }
    }
}

extension Composition {
    func beginReport() {
        reports.begin(freezer)
    }

    func sendReport() {
        // Filed under the account on screen, and only one that is signed in:
        // an account listed with Sign In beside it has no session to send
        // with, and amux.sh would refuse it after the person had pressed Send.
        let account = accounts.selectedAccount.flatMap { $0.signedIn ? $0.id : nil }
        // The profile on screen is the account's, and its runtime writes the
        // report beside the dump it took and sends it.
        let profile = runtime.profile
        Task {
            await reports.send(
                with: profile, as: account, build: AppFiles.build,
                gitSHA: AppFiles.gitSHA)
        }
    }
}
