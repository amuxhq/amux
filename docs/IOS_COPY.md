# iPhone copy

*For anyone writing or reviewing the words the iPhone app shows.*

Write the words a person needs to use the app. Controls name actions; status
lines state facts; failure descriptions explain what the person can do next.
Provider output, names, paths, commands and the person's own text stay verbatim.

## Case and length

Use Title Case for navigation titles, tabs, buttons, menu items and settings
rows: **New Agent**, **Sign In**, **Add to Review**, **Remove from This Phone**. Small
words within a title stay lowercase, and names keep their spelling, as in
**Start on Studio** and **Continue on amux.sh**.

Use sentence case for headlines, section heads, descriptions, captions,
placeholders and status lines: **No hosts yet**, **Needs you**, **Message
refactor-auth**, **Reconnecting · your draft is kept**. Identifiers, commands,
model names and paths keep their original case and use the mono face.

A button is a verb or a verb and its object, normally at most 3 words unless
it includes a name. A description is one sentence and earns its place only
when the control does not explain itself. State a destructive action's actual
consequences. Do not add reassurance or describe implementation choices.

## Terms and punctuation

| Thing | Use |
| --- | --- |
| A computer running amux | host |
| An agent awaiting a response or review | needs you |
| The account service | account |
| What a subscription provides | relay |
| Adding a host | pair |
| A host reached on the local network | on this network |
| The route to a host on the local network | direct |
| An unpaired host discovered on the local network | found |
| A host the relay refused to connect for the account's free plan | away |
| A host no channel reaches, for any other reason | offline |
| Leaving an account or ending a device's access | sign out / revoke |
| The debug bundle | report |
| A plan you do not accept | send back |
| Ending the turn under way; the agent stays live | interrupt |
| Ending a live agent; it goes to exited and can be resumed | stop |
| Removing an exited agent for good | delete |

Use **away** only when the daemon says the relay refused the host because the
selected account has no subscription: upgrading reaches it. Use **offline** for
every other host out of reach, including one that no longer trusts this phone
and one only a signed-out phone cannot reach. Never describe an offline host as
away, and never offer a subscription merely because a host is offline.

A live agent's menu offers **Stop Agent** and never **Delete Agent**; only an
exited agent is deleted. The square button under a working turn and the ask
card's way out of the turn read **Interrupt**. A stop confirm names what the
stop cuts, "desk-1 is working and has 2 background jobs and 1 sub-agent. Stop
anyway?", and asks plainly when it cuts nothing under way.

Something sent while the host cannot be reached is **held**, never refused,
and says when it goes: "held · sends when desk is back", "Stop held · sends
when desk is back". The empty field says the same before anything is typed,
short enough for one line: "Catching up · sends once caught up",
"Reconnecting · sends when desk is back", "desk is out of reach · sends when
back". Revoking a host asks first: "Revoke desk? This phone stops reaching it,
and only pairing again brings it back."

Use second person and present tense. An agent or host is *it*. Avoid hedging,
filler and exclamation marks. Use digits for numbers, a middle dot between
facts on one line, curly quotes and a proper ellipsis (…). Full sentences end
with full stops; status fragments do not. Do not use em dashes, semicolons,
pipes or slashes as prose separators. Preserve punctuation inside verbatim
commands and provider/user content.

## Reviewing a copy change

`just ios lint` runs the copy inventory and its failure probes alongside the
feature architecture checks. It runs without Xcode or a
simulator. The English catalogue is
`apps/apple/Amux/Resources/Localizable.xcstrings`, and report copy is in it
like any other shipping screen's. Copy only the driving tools use is in
`AmuxTestSupport/Sources/AmuxTestSupport/Resources/DebugCopy.xcstrings`, whose
resource directory is excluded from Release.

Update the source and the catalogue together. Each catalogue entry has its
English value and a comment locating its uses. Swift interpolation is shown
as `%@` slots for review; the expression and its fallback literals are checked
separately. These catalogues inventory the app's English copy. They do not
declare support for translated locales or infer localized number/plural
formatting from Swift expressions. Copy returned by models and helpers is
reviewed just like text written directly in a view.

The checker scans all Swift sources in the app and every package, including
report views. Raw strings, multiline strings and strings inside
interpolation are included. It refuses any new nonempty literal until it
appears in the appropriate catalogue or receives an exact non-copy exemption
in `apps/apple/Tools/noncopy.json`. An exemption names its file, literal, surrounding
source, occurrence count and reason. It is for protocol fields, asset names,
formatting, harness diagnostics and verbatim fixture content. It is never a
way to approve interface copy. Reusing an exempt protocol key as a new label,
moving its use or leaving a stale exemption fails the check.

There is no automatic inventory refresh. Review the literal's use before
adding or changing an exemption. The lint enforces coverage and the mechanical
punctuation rules; a reviewer still checks case, clarity, consequences and
whether externally supplied text is being preserved accurately. Review
catalogue entries as complete rendered phrases where views combine them.

After a visible change, inspect both appearances and any affected small or
accessibility layouts, update only the relevant goldens, say why in the
commit message, and run an ordinary comparison. Report descriptions
must match what a report carries: the contents caption names the screenshot
and the phone's diagnostic dump, and promises nothing a bundle may declare
absent.
