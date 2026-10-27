import AmuxDesign
import AmuxFeatures
import SwiftUI

// The review page, and New Agent.

#Preview("Review · page") { StoryScreen("review.page") }
#Preview("Review · no changes") { StoryScreen("review.no-changes") }

#Preview("Review · every example") { StoryGallery(.review) }

#Preview("New agent · Claude") { StoryScreen("new-agent.claude") }
#Preview("New agent · choosing a directory") { StoryScreen("new-agent.browsing") }
#Preview("New agent · large text") { StoryScreen("new-agent.large-text") }
