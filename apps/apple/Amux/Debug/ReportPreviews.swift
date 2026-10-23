import SwiftUI

// Reporting a problem: the report over a frozen frame, from writing it to
// the receipt.

#Preview("Report") { StoryScreen("report.ready") }
#Preview("Report · boxes drawn") { StoryScreen("report.marked") }
#Preview("Report · signed out") { StoryScreen("report.signed-out") }
#Preview("Report · sending") { StoryScreen("report.sending") }
#Preview("Report · not sent") { StoryScreen("report.failed") }
#Preview("Report · sent") { StoryScreen("report.sent") }
