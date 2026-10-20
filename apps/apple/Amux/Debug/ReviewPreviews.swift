import AmuxDesign
import AmuxFeatures
import SwiftUI

// The review page, and New Agent.

#Preview("Review · page") { StoryScreen("review.page") }
#Preview("Review · selecting") { StoryScreen("review.selection") }
#Preview("Review · writing") { StoryScreen("review.comment") }
#Preview("Review · editing") { StoryScreen("review.editing") }
#Preview("Review · folded") { StoryScreen("review.folded") }
#Preview("Review · reading") { StoryScreen("review.reading") }
#Preview("Review · no changes") { StoryScreen("review.no-changes") }
#Preview("Review · not read") { StoryScreen("review.not-read") }

#Preview("New agent · Claude") { StoryScreen("new-agent.claude") }
#Preview("New agent · Codex") { StoryScreen("new-agent.codex") }
#Preview("New agent · choosing a directory") { StoryScreen("new-agent.browsing") }
#Preview("New agent · signed out") { StoryScreen("new-agent.signed-out") }
#Preview("New agent · refused") { StoryScreen("new-agent.refused") }
#Preview("New agent · no repositories") { StoryScreen("new-agent.no-repositories") }
#Preview("New agent · directories not listed") { StoryScreen("new-agent.unlisted") }
