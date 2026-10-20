import AmuxDesign
import AmuxFeatures
import SwiftUI

// A chat in each state, and every row, ask and composer state the chat draws.

#Preview("Chat · conversation") { StoryScreen("chat.conversation") }
#Preview("Chat · working") { StoryScreen("chat.working") }
#Preview("Chat · run open") { StoryScreen("chat.run-open") }
#Preview("Chat · asking") { StoryScreen("chat.asking") }
#Preview("Chat · plan") { StoryScreen("chat.plan") }

#Preview("Asks") { StoryGallery(.asks) }

#Preview("Chat states") { StoryGallery(.chat) }

#Preview("Composer") { StoryGallery(.composer) }

#Preview("Rows") { StoryGallery(.rows) }
