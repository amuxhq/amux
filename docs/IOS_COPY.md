# iPhone copy

*For anyone writing or reviewing the words the iPhone app shows.*

Write the words a person needs to use the app. Controls name actions; status
lines state facts; failure descriptions explain what the person can do next.
Provider output, names, paths, commands and the person's own text stay verbatim.

## Case and length

Use Title Case for navigation titles, tabs, buttons, menu items and settings
rows: **New Agent**, **Sign In**, **Add to Review**, **Remove from This Phone**. Small
words within a title stay lowercase, and names keep their spelling, as in
**Start on Studio** and **Continue as ada@example.com**.

Use sentence case for headlines, section heads, descriptions, captions,
placeholders and status lines: **No devices yet**, **Needs you**, **Message
refactor-auth**, **Reconnecting · your draft is kept**. Identifiers, commands,
model names and paths keep their original case and use the mono face.

A button is a verb or a verb and its object, normally at most 3 words unless
it includes a name. A description is one sentence and earns its place only
when the control does not explain itself. State a destructive action's actual
consequences. Do not add reassurance or describe implementation choices.

## Terms and punctuation

| Thing | Use |
| --- | --- |
| A computer running amux | device |
| An agent awaiting a response or review | needs you |
| The account service | account |
| What a subscription provides | relay |
| Adding a device | pair |
| Ending a paired device's access | unpair |
| A device found by discovery on this network | Local |
| A device reached with nothing in between, not through the relay | Direct |
| A device reached through amux's relay | Relayed |
| A plan that blocks relayed reach | Relayed connections need Pro |
| An unpaired device discovered on the local network | found |
| A device the relay refused to connect for the account's free plan | away |
| A device no channel reaches, for any other reason | offline |
| Leaving an account | sign out |
| The debug bundle | report |
| A plan you do not accept | send back |
| Ending the turn under way; the agent stays live | interrupt |
| Ending a live agent; it goes to exited and can be resumed | stop |
| Removing an exited agent for good | delete |

Use **away** only when the daemon says the relay refused the device because the
selected account has no subscription: upgrading reaches it. The daemon says so
only once nothing else is being tried, and keeps saying so until that changes,
so the subscription offer shows as soon as a device reads away and stays put. Use **offline** for
every other device out of reach, including one that unpaired this phone
and one only a signed-out phone cannot reach. Never describe an offline device as
away, and never offer a subscription merely because a device is offline.

The protocol, the code and the commands people type (`amux hosts`) say *host*;
nothing the app shows does. A device's own name stays as it is, as in
**laptop is nearby**. How a device is reached reads **Local** (found on this
network), **Direct** (no relay in between) or **Relayed** (through amux's
relay), as in "macOS · Relayed" or "Connected · Direct". Where the plan blocks
relayed reach, say **Relayed connections need Pro**; in a longer sentence, say
what Pro does: "Pro relays connections when your phone and device aren't on
the same network." Never "over the internet", "from anywhere" as a route, or
"cloud".

Putting an agent away is **Settle**, never "stop" or "archive": swiped left
on its row, or from its menu. Only a settled agent offers **Delete Agent**,
beside **Un-settle**. Settling asks first only when it cuts work short, and
names what it cuts: "desk-1 is working and has 2 background jobs and 1
subagent." with "Settling stops it now."; otherwise it settles at once. A row
swiped right offers **Pin** or **Unpin**. The square button under a working
turn and the ask card's way out of the turn read **Interrupt**.

A message is never sent while the chat is not current: Send is off and the
draft stays, and the empty field says why, short enough for one line:
"Catching up · your draft is kept", "Reconnecting · your draft is kept",
"desk is out of reach · your draft is kept". One refused because its device
could not be reached comes back with "Not sent: its device is away". A settle
whose device cannot be reached is **held** and says when it goes: "Settle held ·
sends when desk is back". Unpairing a device asks first: "Unpair desk?" with
"This phone stops reaching it and its agents leave this phone’s list. Only
pairing again brings it back."

Use second person and present tense. An agent or device is *it*. Avoid hedging,
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

Update the source and the catalogue together: a literal the catalogue lacks fails the
inventory, and so does an entry no literal uses any more. Each catalogue entry has its
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
accessibility layouts, update only the relevant snapshot baselines and
journey geometry, say why in the commit message, and run an ordinary
comparison. Report descriptions
must match what a report carries: the contents caption names the screenshot
and the phone's diagnostic dump, and promises nothing a bundle may declare
absent.
