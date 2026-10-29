import AmuxDesign
import AmuxFeatures
import SwiftUI

// The whole app, the agent list in each state, and the visual foundation it is drawn on.

#Preview("App") { StoryApp() }
#Preview("App · chat open") { StoryApp(chatOpen: true) }
#Preview("App · settings open") { StoryApp(settingsOpen: true) }
#Preview("App · review open") { StoryApp(reviewOpen: true) }
#Preview("App · new agent") { StoryApp(newAgentOpen: true) }

#Preview("Fleet · first run") { StoryScreen("fleet.first-run") }
#Preview("Fleet · first run, looking") { StoryScreen("fleet.first-run-looking") }
#Preview("Fleet · first run, network refused") { StoryScreen("fleet.first-run-refused") }
#Preview("Fleet · first run, signed out") { StoryScreen("fleet.first-run-signed-out") }
#Preview("Fleet · first run, on the account, free") { StoryScreen("fleet.first-run-on-the-account-free") }
#Preview("Fleet · no agents") { StoryScreen("fleet.no-agents") }
#Preview("Fleet · busy") { StoryScreen("fleet.busy") }
#Preview("Fleet · filter") { StoryScreen("fleet.filter") }
#Preview("Fleet · no matches") { StoryScreen("fleet.no-matches") }
#Preview("Fleet · signed out") { StoryScreen("fleet.signed-out") }
#Preview("Fleet · large text") { StoryScreen("fleet.large-text") }

#Preview("Fleet · every example") { StoryGallery(.fleet) }

#Preview("Foundation") { FoundationSheet() }

#Preview("Fleet · guides") { StoryScreen("fleet.busy").overlay { AlignmentGuides() } }

#Preview("Working marks") { WorkingMarks() }

#Preview("Fleet · breathing") { StoryScreen("fleet.busy").environment(\.workingMark, .breathing) }
#Preview("Fleet · typing") { StoryScreen("fleet.busy").environment(\.workingMark, .typing) }
#Preview("Fleet · orbit") { StoryScreen("fleet.busy").environment(\.workingMark, .orbit) }
#Preview("Fleet · filling") { StoryScreen("fleet.busy").environment(\.workingMark, .filling) }
