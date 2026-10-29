import AmuxDesign
import AmuxFeatures
import SwiftUI

// A chat in each state, and every row, ask and composer state the chat draws.

#Preview("Chat · loading") { StoryScreen("chat.loading") }
#Preview("Chat · not opened") { StoryScreen("chat.not-opened") }
#Preview("Chat · working") { StoryScreen("chat.working") }
#Preview("Chat · conversation") { StoryScreen("chat.conversation") }
#Preview("Chat · asking") { StoryScreen("chat.asking") }
#Preview("Chat · plan") { StoryScreen("chat.plan") }
#Preview("Chat · queued") { StoryScreen("chat.queued") }
#Preview("Chat · host away") { StoryScreen("chat.away") }
#Preview("Chat · exited") { StoryScreen("chat.exited") }
#Preview("Chat · signed out") { StoryScreen("chat.signed-out") }
#Preview("Chat · overview") { StoryScreen("chat.overview") }
#Preview("Chat · stop a job") { StoryScreen("chat.overview-stop") }
#Preview("Chat · settings") { StoryScreen("chat.settings") }
#Preview("Chat · stop") { StoryScreen("chat.stop") }
#Preview("Chat · delete") { StoryScreen("chat.delete") }
#Preview("Chat · rename") { StoryScreen("chat.rename") }
#Preview("Chat · large text") { StoryScreen("chat.large-text") }
#Preview("Ask · question, large text") { StoryScreen("ask.question-large-text") }

#Preview("Asks") { StoryGallery(.asks) }

#Preview("Chat states") { StoryGallery(.chat) }

#Preview("Composer") { StoryGallery(.composer) }

#Preview("Rows") { StoryGallery(.rows) }
