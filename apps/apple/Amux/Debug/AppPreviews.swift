import AmuxDesign
import AmuxFeatures
import SwiftUI

// The whole app, the agent list in each state, and the visual foundation it is drawn on.

#Preview("App") { StoryApp() }
#Preview("App · chat open") { StoryApp(chatOpen: true) }
#Preview("App · settings open") { StoryApp(settingsOpen: true) }
#Preview("App · review open") { StoryApp(reviewOpen: true) }
#Preview("App · new agent") { StoryApp(newAgentOpen: true) }

#Preview("Fleet · busy") { StoryScreen("fleet.busy") }
#Preview("Fleet · stopped open") { StoryScreen("fleet.stopped-open") }
#Preview("Fleet · host away") { StoryScreen("fleet.away") }
#Preview("Fleet · no agents") { StoryScreen("fleet.no-agents") }
#Preview("Fleet · first run") { StoryScreen("fleet.first-run") }
#Preview("Fleet · two accounts") { StoryScreen("fleet.two-accounts") }
#Preview("Fleet · signed out") { StoryScreen("fleet.signed-out") }
#Preview("Fleet · profile picture") { StoryScreen("fleet.picture") }
#Preview("Fleet · no matches") { StoryScreen("fleet.no-matches") }

#Preview("Foundation") { FoundationSheet() }

#Preview("Fleet · guides") { StoryScreen("fleet.busy").overlay { AlignmentGuides() } }

#Preview("Working marks") { WorkingMarks() }

#Preview("Fleet · breathing") { StoryScreen("fleet.busy").environment(\.workingMark, .breathing) }
#Preview("Fleet · typing") { StoryScreen("fleet.busy").environment(\.workingMark, .typing) }
#Preview("Fleet · orbit") { StoryScreen("fleet.busy").environment(\.workingMark, .orbit) }
#Preview("Fleet · filling") { StoryScreen("fleet.busy").environment(\.workingMark, .filling) }
