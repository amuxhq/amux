# Releasing amux

*For maintainers cutting a release of the iPhone app or the `amux` binary, and developers working on how installed daemons find and verify new builds.*

Three things ship from this repository, each on its own schedule:

| What | How it is cut | Tags |
| --- | --- | --- |
| The iPhone app | `just ios release`, from a Mac with the signing setup below | `ios-v<version>-b<build>` |
| The `amux` binary | `just release <version>`, then the Release workflow on the pushed tag | `v<version>` |
| The daemon's release feed | `just deploy <version>` signs the release's binaries with the release Mac's key and publishes the channel manifest to amux.sh | none |

## The iPhone app

The app ships from this repository, built by Xcode from the generated
project — there is no Expo, no EAS and no hosted build service in the path.
One command, `just ios release`, takes a clean checkout to a signed `.ipa` that
Apple has validated, and delivers it to App Store Connect, where TestFlight
shows it once Apple has processed it; the commit and its tag are pushed after
that. It stops there: **nothing here submits anything for review.** A
rehearsal, `--no-upload` and `--no-push` each stop short; see
[Where it stops](#where-it-stops).

The app's privacy manifest, `apps/apple/Amux/Resources/PrivacyInfo.xcprivacy`,
declares what the app collects and why it calls the APIs Apple asks reasons
for (UserDefaults, file timestamps, disk space). An upload that uses one of
those APIs without a declared reason is refused. The manifest and the App
Privacy answers in App Store Connect describe the same collection, so a change
to what the app or [analytics](ANALYTICS.md) sends updates both, and the App
Store Connect answers before the version that starts sending is submitted.

The app is the next version of the listing already on the App Store, not a
new one. Its bundle identifier, `sh.amux.app`, is what signing and the App
Store record agree on, and it is committed in `apps/apple/project.yml`, from
which XcodeGen writes `apps/apple/Amux/Info.plist`. The listing's numeric
Apple ID is not recorded here: nothing this recipe runs asks for it, the
upload included — altool finds the listing from the bundle identifier inside
the package. Read it back from the App Store Connect record when it is wanted:
`xcrun altool --list-apps --api-key <key id> --api-issuer <issuer id>`.

The recipe is `release` in [`apps/apple/justfile`](../apps/apple/justfile);
the work is done by [`scripts/release.py`](../scripts/release.py).

### The two numbers

A build carries two numbers, and they are not interchangeable.

**The marketing version** (`CFBundleShortVersionString`) is what a person
sees in the App Store: `1.0.32`. It lives in one place — the app target's
`MARKETING_VERSION` build setting in `apps/apple/project.yml` — and the
committed `apps/apple/Amux/Info.plist` reads it from there as
`$(MARKETING_VERSION)`. Writing it as a build setting rather than a literal is
what lets a rehearsal archive the version a release *would* cut without
editing a tracked file: the number can be passed to `xcodebuild` instead.

`just ios release` derives the next version by bumping the patch component of
the highest version it knows — the one in the project, or a higher one in the
tags; `--version X.Y.Z` names a different one instead. The only version it
refuses is one *below* the highest it knows, because a version can never move
backwards.

`--version` may name the version already here, and that is not a mistake: the
App Store rejects a version string that is not above the version it last
**released**, and it rejects a build number that has been used before. It does
not object to several builds carrying one marketing version while that version
is unreleased — which is exactly what happens when the first attempt at a
version is rejected in review, or when a validation finds something to fix. So
`--version 1.0.32 --build 37` is a legitimate second attempt at 1.0.32; what
is spent by each attempt is the build number, never the version.

The project carries `1.0.32`, build `36`, and the tag `ios-v1.0.32-b36`
records them, so the next release is `1.0.33` with build `37` unless flags say
otherwise.

**The build number** (`CFBundleVersion`) is a single integer that identifies
one binary forever. It lives beside the marketing version as
`CURRENT_PROJECT_VERSION`, read by the bundle as
`$(CURRENT_PROJECT_VERSION)`, and comes from one place only: one above the
highest number the `ios-v*` tags record. `--build N` raises it deliberately.
The recipe refuses to reuse a number and refuses to go backwards, and — when
no tag records a number at all — refuses to invent the first one.

That rule is not tidiness. A build number, once an upload has used it, is
consumed permanently for this app: App Store Connect binds it to that binary
and will not accept it again even if the build is rejected, expired or
deleted, and an upload whose build number is not greater than the highest
already in its version train is rejected outright. Numbers only ever go up,
and a mistake costs a number that can never be recovered. Locally, the same
rule keeps diagnosis honest — a debug report names its build as
`amux-ios/<marketing version>`, and two different binaries claiming one
identity make every report that mentions it ambiguous.

App Store Connect also holds build numbers this repository never issued,
from the app's earlier Expo builds, and the tags know nothing about them. So
in a checkout with no `ios-v*` tag — a clone that did not fetch tags, say —
`just ios release` refuses rather than counting from the project:

```
no ios-v* tag records a build number, so there is nothing to count from and a
first number will not be guessed. Read the highest build App Store Connect
holds for this app (xcrun altool --list-builds, or the TestFlight tab) and
pass --build N above it; every later release counts from the tags
```

Fetch the tags first (`git fetch --tags`). Only if the tags really are gone,
read the highest build number App Store Connect holds for `sh.amux.app` and
pass `--build N` above it. A rehearsal is the one exception — it issues no
number, spends none and records none, so with no tag it archives under the
build number in the project and says so, rather than stopping a signing check
that proves nothing about numbering.

### The tag

`ios-v<marketing version>-b<build number>` — for example
`ios-v1.0.32-b36`. Its own namespace, because plain `vX.Y.Z` tags in this
repository belong to the [`amux` binary](#the-amux-binary), which versions
separately and moves on its own schedule.

Putting the build number in the tag name is what makes the tags the record of issued numbers:
the highest number ever issued can be read from `git tag` alone, with no
network and no state file to lose.

The tag is annotated and cut on the release commit — the commit that writes
the two numbers, titled `Version <version> (build <build>)` — and both come
*last*, after Apple has validated and received the exported build. The numbers
are written into the working tree before the archive, so the binary carries
them and the commit records exactly the tree that was archived; nothing is
recorded until Apple has answered. See
[When a release stops partway](#when-a-release-stops-partway).

A full release then pushes both: `git push origin HEAD:main`, then the tag.
Cut releases from a checkout whose `HEAD` fast-forwards `main`, or the push
fails and leaves the commit and tag at home.

### Release notes

The notes are the annotated tag's message. The recipe drafts them from the
commit subjects since the previous `ios-v*` tag (the last twenty commits when
there is none), and `--notes-file <path>` replaces the draft with prose
written by hand. The same text is written beside the exported `.ipa` as
`ReleaseNotes.txt`, so the export directory carries what the build claims to
contain.

The upload does not send them to Apple. TestFlight's "What to Test" for a
build is filled in by hand in App Store Connect, from `ReleaseNotes.txt` if
that is what testers should read.

### Signing

Two things sign this app, and they belong in different places.

**Committed, in `apps/apple/project.yml`:** device signing is off by default
(`CODE_SIGNING_ALLOWED: NO`), and the simulator SDK turns it back on with an
ad-hoc identity (`CODE_SIGN_IDENTITY[sdk=iphonesimulator*]: "-"`). Every
routine build in this repository is a simulator build, so the default is the
one that needs no identity at all.

**Untracked, `apps/apple/Signing.local.xcconfig`:** the Team ID, and nothing
else.

```
// The Apple Developer team this Mac signs as.
DEVELOPMENT_TEAM = ABCDE12345
```

`.gitignore` covers it. A Team ID is not a secret, but it is not this
repository's either: a committed one would build somebody else's app under
their team, and every fork would inherit it. The bundle identifier is *not*
in this file — it is committed, because the app's identity is the same
everywhere and a local override is a way to sign the wrong app.

The recipe *reads* this file rather than sourcing it or handing it to
`xcodebuild -xcconfig`: an untracked file that a recipe executed, or that
could set any build setting it liked, is a way to run anything. It takes the
one assignment it expects and passes it on the command line. When the file is
absent or the assignment is missing, the recipe says exactly which piece is
missing and exits non-zero.

**The Team ID is the certificate's `OU`, not the name in its parentheses.**
An identity reads `Apple Distribution: <person> (<ten characters>)`, and the
string in the parentheses is *usually* the Team ID but is the individual's
identifier on a personal Development certificate — a different value, ten
characters long, that looks exactly as plausible. Writing that one into
`apps/apple/Signing.local.xcconfig` costs an afternoon: every credential is
valid, every flag is right, and `xcodebuild` stops with *No Account for Team
"…". Add a new account in Accounts settings*, which reads like a missing Apple
Account rather than a wrong ten characters. Take the Team ID from
developer.apple.com under **Membership details**, or read the `OU` field of
any certificate in the account:

```
security find-certificate -c "Apple Distribution" -p | \
  openssl x509 -noout -subject
```

**The export signs by hand.** `apps/apple/ExportOptions.plist` sets
`signingStyle: manual` and names both the certificate type
(`Apple Distribution`) and the profile (`amux App Store`, for
`sh.amux.app`). Automatic signing is what Xcode does in the IDE, and it does
not work here: the export answers *Cloud signing permission error* and then
*No profiles for `sh.amux.app` were found*, with an active matching profile
sitting installed. Signing by hand means **the certificate and the profile
must already exist on the Mac before an export** — nothing creates them
mid-run. It also means two names are pinned: rename the profile in Apple's
portal and the export stops. The certificate type is named rather than one
certificate's full name, because a full name ends in the Team ID and that
file is committed.

**The certificate expires.** The one on the release Mac runs to **10
September 2027**. When it does, the export fails to find an identity: make a
new Apple Distribution certificate, install it, and make a new `amux App
Store` profile tied to it — the profile is bound to the certificate and does
not survive it. Keeping the same two names means nothing in this repository
changes.

So: **a simulator build needs nothing** — no team, no certificate, no
profile. **A distribution archive needs** the Team ID above, an Apple
Distribution certificate in the login keychain, an App Store provisioning
profile named `amux App Store` for `sh.amux.app`, and the App Store Connect
API key. The last three are produced once and then reused; the
[checklist](#one-time-operator-setup) at the end is how they come to exist.

### The commands

`just ios release` depends on two other recipes, so they run first whether or
not anybody ran them: `just ios package` builds every shipping slice of the
Rust bridge under the `release` profile into
`target/ios/AmuxApp.xcframework`, which the Release configuration links, and
`just ios scope-audit` inspects the release bundle, so a bundle carrying a
debug surface or an excluded platform stops the release before an archive
exists rather than after Apple has one. Then the project is generated from
`apps/apple/project.yml`, as every other iOS recipe does.

Archive:

```
xcodebuild archive \
  -project apps/apple/Amux.xcodeproj -scheme Amux -configuration Release \
  -destination 'generic/platform=iOS' \
  -archivePath target/ios/release/Amux.xcarchive \
  -derivedDataPath target/ios/ReleaseDerivedData \
  -allowProvisioningUpdates \
  -authenticationKeyPath "$HOME/.appstoreconnect/private_keys/AuthKey_<KEY ID>.p8" \
  -authenticationKeyID <key id> -authenticationKeyIssuerID <issuer id> \
  -quiet \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_STYLE=Automatic \
  DEVELOPMENT_TEAM=<from apps/apple/Signing.local.xcconfig> \
  MARKETING_VERSION=<version> CURRENT_PROJECT_VERSION=<build>
```

The two numbers are passed rather than assumed: a full release has already
written them into `apps/apple/project.yml`, and a rehearsal has not written
them anywhere, so passing them is what makes both runs archive the same way.

Export:

```
xcodebuild -exportArchive \
  -archivePath target/ios/release/Amux.xcarchive \
  -exportPath target/ios/release/<tag> \
  -exportOptionsPlist target/ios/release/ExportOptions.plist \
  -allowProvisioningUpdates \
  -authenticationKeyPath "$HOME/.appstoreconnect/private_keys/AuthKey_<KEY ID>.p8" \
  -authenticationKeyID <key id> -authenticationKeyIssuerID <issuer id>
```

`apps/apple/ExportOptions.plist` is committed and holds no Team ID; the recipe
copies it to `target/ios/release/ExportOptions.plist` and inserts `teamID`
from the local signing file, so the committed file stays free of anything
that identifies a team. Its keys:

| key | value | why |
| --- | --- | --- |
| `method` | `app-store-connect` | the App Store distribution shape; the export is still local |
| `destination` | `export` | write the `.ipa` here; the upload is a separate step the recipe runs with altool |
| `signingStyle` | `manual` | the export names what it signs with. Automatic signing fails here — see [Signing](#signing) |
| `signingCertificate` | `Apple Distribution` | the certificate *type*; `teamID` picks which identity in the keychain. A full name would carry the Team ID into a committed file |
| `provisioningProfiles` | `sh.amux.app` → `amux App Store` | the profile by name. It has to be installed already |
| `manageAppVersionAndBuildNumber` | `false` | Xcode may otherwise rewrite the build number at export. The number this repository chose and tagged is the number that ships |
| `uploadSymbols` | `true` | symbols travel with the archive so crash reports are readable |

The API key, passed by the three `-authenticationKey*` flags, is what takes a
person out of the loop: `xcodebuild` authenticates to the Apple Developer
website with it instead of an Apple Account, so no password and no 2FA prompt
appears in a release. `-allowProvisioningUpdates` lets it register and
refresh what it can — the app ID, and the development profile the archive
signs with — rather than stopping to ask. It does *not* extend to the
distribution profile: Xcode's cloud signing refuses that from a script here,
which is why the export signs by hand against a profile made once. The
distribution certificate and the `amux App Store` profile are therefore
standing inputs, and `just ios release --preflight` reports them as such.

Validate:

```
xcrun altool --validate-app -f target/ios/release/<tag>/Amux.ipa -t ios \
  --api-key <key id> --api-issuer <issuer id>
```

`altool` finds the private key itself: `~/.appstoreconnect/private_keys` is
one of the directories it searches for `AuthKey_<key id>.p8`, so only the two
identifiers are passed.

Validation asks Apple to check the binary exactly as an upload would — the
signature, the entitlements, the icons, the Info.plist, the deployment
target — and answers. It consumes no build number, creates no TestFlight
build, and is visible to nobody.

Upload, the last step that reaches Apple:

```
xcrun altool --upload-app -f target/ios/release/<tag>/Amux.ipa -t ios \
  --api-key <key id> --api-issuer <issuer id>
```

### One command

```
just ios release              # bump, archive, export, validate, upload,
                               # commit, tag, push
just ios release --no-upload   # all of that except the upload
just ios release --no-push     # all of that except the push
just ios release --preflight   # check every input, do nothing
just ios release --rehearse    # archive, export and validate with the
                                # would-be numbers; write nothing, tag nothing
```

The recipe's one-line description in `just --list ios` still reads "upload
nothing"; the script it runs uploads and pushes as described here.

Validation is where a rehearsal ends and where a release goes on to deliver,
so `just ios release` is the whole release and there is no `altool` command to
remember afterwards. A rehearsal reaching that point is what makes it a real
proof rather than a dry run: Apple answers on the actual signed binary, and
answers the same way it will when the build is delivered.

`--preflight` checks what the run needs — the signing file, the key and both
identifiers in the keychain, the private key on disk, the export options, the
Apple Distribution certificate in the login keychain, an unexpired profile
under each name the export options ask for, and a derivable version and build
number — names anything missing and exits non-zero if anything is. It reports
the tree's state too, but does not fail on it: an uncommitted file is not
something a person produces once, and a rehearsal does not care. A real
release does, and refuses.

`--rehearse` goes all the way to a validated `.ipa` using the numbers the next
release *would* use, but writes nothing to the tree and cuts no tag, so it can
be run as often as you like — validation spends no build number, so running it
a hundred times costs nothing but the wait. It proves the "writes nothing"
half rather than promising it: `git status --porcelain` is read before the run
and again after it, and a rehearsal that left any path changed names those
paths and fails instead of printing the claim. A rehearsal never uploads and
never pushes.

### When a release stops partway

A release does the reversible work first and the permanent work last, so
each place it can stop has one recovery.

**The numbers are written but nothing is committed.** Anything that fails in
the archive, the export, the validation or the upload leaves this. `just ios
release` says so and stops; `git status` shows the modified project files and
no new commit, and `git tag --list 'ios-v*'` is unchanged. Undo the numbers:

```
git checkout apps/apple/project.yml apps/apple/Amux/Info.plist apps/apple/Amux.xcodeproj/project.pbxproj
```

Then fix what failed and run the release again. If the failure came before
the upload, nothing was spent: the build number was never tagged and Apple
never accepted a binary carrying it, so the same number is still free. If the
upload itself failed, look in TestFlight before running again: a build that
reached Apple has spent its number, and the next run needs `--build` above it.

**The commit was made but the tag is missing.** Only a failing `git tag`
leaves this — the name already exists, most often, because a previous attempt
tagged it. The commit is on the branch and is not pushed. Either tag that
commit by hand, taking the message from the notes the run wrote beside the
`.ipa`:

```
git tag -a ios-v<version>-b<build> -F target/ios/release/ios-v<version>-b<build>/ReleaseNotes.txt
```

or undo the commit and run the release again with a build number above the
one already tagged:

```
git reset --hard HEAD~1        # nothing was pushed
just ios release --build <N>
```

**The commit and tag exist but the push failed.** Push them by hand:
`git push origin HEAD:main`, then `git push origin ios-v<version>-b<build>`.

Never delete an existing `ios-v*` tag to make room. A tag is the record of a
number that may already have reached Apple, and removing it is how a number
gets issued twice.

### Where it stops

The recipe ends at the push. It does not submit for review.

Uploading is the one step here that cannot be taken back. The build number is
spent permanently whether or not anything is ever submitted, and the build is
visible to everyone on the team the moment Apple finishes processing it.
Everything before it is local and undone with one `git checkout`. That is why
it runs after the validation, on the same package Apple has just accepted, and
before the commit and the tag: a build Apple would refuse never reaches the
upload, and the tag records a build that actually arrived.

The commit and the tag are pushed after it, because the tag is the record. A
build number is spent permanently the moment the upload lands, and the next
release counts from the tags — so a tag left in the tree that cut it is a
record of a spent number that disappears with that tree, and the release after
it would refuse to guess and send somebody back to App Store Connect to look
up what this run already knew.

Three ways to stop short. A rehearsal never delivers — that is what makes it
free to run as often as you like, since validation spends no build number.
`--no-upload` runs a full release, tag and push and all, stopping before the
delivery. `--no-push` keeps the commit and tag at home, which leaves the
record unreadable to every other checkout.

Submitting for review stays a person's act in App Store Connect, with the
screenshots, the notes and the reviewers already decided.

### Screenshots

The listing's iPhone screenshots belong to an App Store version, and a
version's listing is fixed once it is live: new screenshots go on the next
version, before it is submitted.

```sh
just store                          # compose the seven frames (marketing/README.md)
just ios screenshots --dry-run      # say what would change
just ios screenshots                # or --version X.Y.Z for a version not made yet
```

`just ios screenshots` signs in with the same App Store Connect key as a
release, finds the version that can still change (not yet submitted, or sent
back by review), and replaces its en-US 6.9-inch screenshots with the seven
frames, in order. Apple shrinks that set for every smaller iPhone, so it is
the only one uploaded, and the listing's other iPhone sets are removed once
it is whole: an older 6.5-inch set left in place would go on showing on
6.5-inch phones. iPad sets are left alone. The API calls the 6.9-inch set
`APP_IPHONE_67`, the largest iPhone type its reference lists;
`--display-type` names another if Apple adds one. It attaches no build and
submits nothing: in App Store Connect, the version still needs its build and
a person to submit it.

### One-time operator setup

Everything here is done once, in a browser, by a person. Afterwards releases
need no Apple Account, no password and no 2FA prompt. Follow it to set up a
Mac that cuts releases.

**1. Create the App Store Connect API key.**

- Sign in to App Store Connect (appstoreconnect.apple.com) as an Account
  Holder or Admin — a Developer cannot create keys.
- Go to **Users and Access**, then the **Integrations** tab, then
  **App Store Connect API**, and stay on the **Team Keys** list.
- Do not use the **In-App Purchase** key offered in the same area. That is a
  different credential with a different purpose; it cannot build, upload or
  read builds, and using it produces an authentication failure that does not
  explain itself.
- Generate a key with the **App Manager** role. App Manager is what builds,
  TestFlight and app metadata require. **Developer is not enough** — it can
  read, and the release will fail partway through with an authorization
  error.
- Enable **Access to Certificates, Identifiers & Profiles** on the key. This
  is a separate grant from the role, and it is what lets the key create the
  distribution certificate and the provisioning profile in step 5 instead of
  a person clicking through the portal. A key that already exists without the
  access cannot be changed; make another one. Confirm a key has it by asking
  for the list it gates: `GET /v1/certificates` answers 200 with the access
  and 403 without.
- **Download the `.p8` file now.** It can be downloaded exactly once. Apple
  never shows it again, and there is no recovery: a lost key is replaced by
  revoking it and creating another. This is the only irreversible step in the
  list.
- Copy the two identifiers shown beside the key in the same list: the
  **Key ID** (also the `<key id>` in the downloaded file's
  `AuthKey_<key id>.p8` name, by Apple's convention) and the **Issuer ID**
  (one per team, above the list).

**2. Put the key where the tools already look.**

```
mkdir -p ~/.appstoreconnect/private_keys
mv ~/Downloads/AuthKey_<key id>.p8 ~/.appstoreconnect/private_keys/
chmod 600 ~/.appstoreconnect/private_keys/AuthKey_<key id>.p8
```

That directory is one of the paths `xcodebuild`, `altool` and `notarytool`
search by convention, so nothing has to be configured to find it. Never copy
it into this repository.

**3. Put the three identifiers in the login keychain.** The key id, the
issuer id and the Team ID, under the service `amux-appstoreconnect` — the
same place the QA account passwords are kept, and never committed:

```
security add-generic-password -s amux-appstoreconnect -a key-id    -w <key id>
security add-generic-password -s amux-appstoreconnect -a issuer-id -w <issuer id>
security add-generic-password -s amux-appstoreconnect -a team-id   -w <Team ID>
```

Read one back with
`security find-generic-password -s amux-appstoreconnect -a key-id -w`. The
Team ID is on developer.apple.com under **Membership details** — and see the
warning in [Signing](#signing) before copying one out of a certificate name,
because the string in an identity's parentheses is not always it.

**4. Write the local signing file.**

```
printf 'DEVELOPMENT_TEAM = %s\n' \
  "$(security find-generic-password -s amux-appstoreconnect -a team-id -w)" \
  > apps/apple/Signing.local.xcconfig
```

`.gitignore` already covers it. Confirm with
`git check-ignore -q apps/apple/Signing.local.xcconfig`.

**5. Make the distribution certificate and the App Store profile.** Both,
once. The export signs by hand, so neither appears on its own during a run —
a Mac that has only an Apple Development identity is the normal starting
point and this is the step that fills the gap.

On developer.apple.com: **Certificates, Identifiers & Profiles →
Certificates → +**, choose **Apple Distribution**, and upload a certificate
signing request produced by **Keychain Access → Certificate Assistant →
Request a Certificate From a Certificate Authority** (saved to disk).
Download the resulting `.cer` and double-click it to install it, with its
private key, into the login keychain. Then **Profiles → +**, choose **App
Store Connect** distribution, select `sh.amux.app` and that certificate, and
name the profile exactly **`amux App Store`** — `apps/apple/ExportOptions.plist`
asks for that name. Download it and double-click it to install it into
`~/Library/MobileDevice/Provisioning Profiles`.

The same two things can be created through the App Store Connect API with the
key from step 1; the private key still has to be generated locally and the
signed certificate imported by hand, so the browser is not much slower.

A team may hold a limited number of distribution certificates at a time.
Revoking an old one whose private key you can no longer find is normal and
breaks nothing already on the App Store — Apple re-signs submitted builds
during processing, so a shipped app does not depend on the certificate that
signed it staying valid.

Confirm both landed:

```
security find-identity -v -p codesigning        # names an Apple Distribution identity
ls ~/Library/MobileDevice/Provisioning\ Profiles/
```

**6. Check it.** `just ios release --preflight` reports every input —
including the certificate and the profile from step 5, by name — and says
which one is missing. Nothing in this step reaches Apple beyond
authenticating. Then `just ios release --rehearse` archives and exports for
real, writing nothing to the tree and cutting no tag, which is the proof that
the arrangement works end to end.

None of the values above — the key, its id, the issuer id or the Team ID —
appear in any committed file, and none of them should be pasted into one.

## The amux binary

The desktop binary is versioned by the `version` of `crates/amux` and
`crates/node`, which move together: the daemon reports its own crate's
version, and a release that moved only the CLI would have `amux --version`
and the fleet disagree. A release is two acts, at two times, each one
recipe (both implemented by `xtask release` in
[`crates/xtask/src/release.rs`](../crates/xtask/src/release.rs)):

```
just release 0.8.0                 # cut: the version exists and its binaries are built
just deploy 0.8.0 --channel preview   # machines on preview take it
just deploy 0.8.0 --rollout 10        # a tenth of the machines on stable take it
just deploy 0.8.0                     # all of stable
```

**Cutting** makes the version exist. `just release <version>` refuses a
tree with other changes in it, a version not above the current one, and a
Mac without the release signing key (a cut nobody could deploy is a tag
for nothing); then it writes the version into both `Cargo.toml` files,
updates `Cargo.lock` offline, runs `just release-check`, commits
`v<version>`, tags `v<version>` and pushes the branch and the tag. The tag
starts the Release workflow, and the cut is done. It does not wait for the
workflow.

**Deploying** puts a cut release in front of machines, and is the act that
chooses who: a channel, and how much of it. `just deploy <version>` waits
for the tag's workflow to have published the binaries if it has not yet,
then signs and publishes the channel manifest (below,
[Deploying a release](#deploying-a-release)). It repeats against the same
release: a wider rollout, or preview promoted to stable, is another deploy
of the same version. Versions are plain; there is no preview version,
only a preview deployment, and a bad preview is fixed by cutting the next
version.

`just release-check` builds the shipping binary under the `release` profile —
`cargo build --release -p amux --bins --no-default-features --features bundled`,
whole-program optimised (fat LTO, one codegen unit) with line tables — and
runs [`scripts/release-policy-check.sh`](../scripts/release-policy-check.sh)
on it, which fails if the binary's help mentions the debug command, if it
accepts `amux debug`, or if its SQLite linkage breaks policy
(`scripts/sqlite_linkage.py`).

The pushed tag starts the Release workflow
([`.github/workflows/release.yml`](../.github/workflows/release.yml)). It
runs `just release-check -- --target <triple>` for three targets, splits
each binary's symbols from it with
[`scripts/release-symbols.sh`](../scripts/release-symbols.sh), and
publishes a GitHub Release with the three stripped binaries, their symbols,
a `checksums.txt` of the binaries' SHA-256 sums, and notes GitHub generates
from the commits:

| Runner | Target | File |
| --- | --- | --- |
| `ubuntu-latest` | `x86_64-unknown-linux-gnu` | `amux-linux-x86_64` |
| `macos-latest` | `aarch64-apple-darwin` | `amux-macos-arm64` |
| `windows-latest` | `x86_64-pc-windows-msvc` | `amux-windows-x86_64.exe` |

Each binary's symbols are published beside it: `amux-linux-x86_64.debug`
(found through the binary's GNU debuglink), `amux-macos-arm64.dSYM.zip`
(matched by Mach-O UUID) and `amux-windows-x86_64.pdb`. They hold function
names and line tables, enough to symbolicate a crash address from a shipped
binary; the binaries themselves carry no symbols.

## The daemon's release feed

A machine running [`amux supervise`](SUPERVISOR.md) finds new builds in a
channel manifest and installs them itself. The format and the checks live in
[`crates/node/src/release.rs`](../crates/node/src/release.rs); which build
to install, when, and when to roll back in
[`crates/supervisor-core`](../crates/supervisor-core/src/lib.rs); the fetch,
staging and file moves in
[`crates/node/src/supervisor/mod.rs`](../crates/node/src/supervisor/mod.rs).

### Channels

There are two channels, `stable` and `preview`. An install follows `stable`
unless someone chose otherwise (`amux config set channel preview`): preview is for
people who asked for builds ahead of stable, and it is never picked by
chance. Each channel is one manifest at a fixed address:

```
<releases_url>/<channel>.json      # releases_url defaults to https://amux.sh/releases
https://amux.sh/releases/stable.json
https://amux.sh/releases/preview.json
```

`releases_url` in the installation config points an install, or a test, at
another server.

### The manifest

A manifest is JSON: the channel it is for, an optional rollout percentage,
one entry per target triple, and one signature over all of it.

```json
{
  "channel": "stable",
  "rollout": 10,
  "targets": {
    "aarch64-apple-darwin": {
      "version": "0.8.0",
      "url": "https://example.com/amux-macos-arm64",
      "sha256": "<hex SHA-256 of the file at url>",
      "size": 31457280
    }
  },
  "signature": "<base64 Ed25519 signature>"
}
```

- A supervisor on the stable channel refuses a manifest whose `channel` is
  `preview`, however well it is signed: a build deployed to preview cannot
  be served to stable by whoever holds the manifest's URL.
- A binary looks itself up by the triple it was built for, which
  `crates/node/build.rs` compiles in as `AMUX_TARGET`.
- `url` is fetched as-is; the file there is the whole `amux` binary, and
  `size` is its length. The supervisor writes no more than `size` bytes
  of it, and installs it only when its length and hash are the entry's.
- `rollout`, when present and below 100, admits a host when SHA-256 of its
  host id, mod 100, is under the number. A host's place is fixed across every
  rollout and nothing is stored. Publishing at 10, watching, then raising to
  100 is a staged rollout; lowering the number stops further adoption.
- The supervisor installs an entry only if its version is newer than the
  running one, so a manifest cannot move a machine backwards. A bad release
  that has already activated is fixed by publishing a higher version — the
  previous code under a new number when the bad release changed no data
  shape, a forward fix otherwise.
- Promoting a preview build to stable is deploying the same version to
  stable: the same binary, the same entry, in the other manifest.

### Signing and verification

The signature is Ed25519 over the whole manifest, as exactly these bytes:
the channel, the rollout (`100` when absent), then for each target in name
order its version, url, hash in lowercase hex, and size:

```
amux manifest
<channel>
<rollout>
<target>
<version>
<url>
<sha256>
<size>
...
```

One signature covers everything the server could otherwise change: which
channel a build is on, how many hosts take it, and what each build is. A
tampered manifest cannot relabel an old signed binary as a newer release,
move a preview build onto stable, or widen a staged rollout. The
supervisor verifies the manifest before reading anything from it, then
downloads to `amux.staged` beside the installed binary, writing no more
than the entry's size and computing the SHA-256 as it goes, and installs
only if the length and the hash are the entry's. `release::sign` is the
publishing half of that check. A manifest is at most 1 MiB and an
artifact's signed size at most 256 MiB
([Parameters](PARAMETERS.md)); more is refused unread.

The public keys are compiled into the binary from `AMUX_RELEASE_PUBLIC_KEYS`,
comma-separated keys of 64 hex digits each, set in the environment at build
time. A manifest is accepted when its signature verifies against any of
them:

| Build | Keys it trusts |
| --- | --- |
| `AMUX_RELEASE_PUBLIC_KEYS` set | Those keys |
| Debug build without it | `TEST_RELEASE_KEY`, whose private half lives in the test suites |
| Release build without it | None. It restarts its daemon but installs nothing, and `amux update` says so. |

The Release workflow sets it to the release key and its backup, so every
published binary trusts both; a release build made by hand trusts nothing and
is replaced the way it was installed.

The supervisor tests sign releases with the test key and serve manifests
from a local server
([`crates/node/tests/supervisor.rs`](../crates/node/tests/supervisor.rs),
[`crates/amux/tests/supervise_cli.rs`](../crates/amux/tests/supervise_cli.rs)).

### The release key

The private key is a 32-byte seed that exists in one place: a keychain of
its own on the Mac that cuts releases, `amux-release.keychain`, as the
generic password item with service `amux-release-key`. It is never in this
repository, never on GitHub and never on amux.sh. That is the point of
signing at all: the machines that install a release verify it against a key
compiled into the binary they already run, so the server that hands out
manifests cannot make them install something else. A compromised amux.sh can
serve a stale manifest or none, and nothing worse. A GitHub token stolen
after a deploy can tag and build what it likes and re-upload any asset, and
none of it reaches a machine either: a manifest needs the key, and a swapped
asset fails the hash the key signed.

The keychain has a password of its own, not the login password, and locks
again a minute after use and when the Mac sleeps. Its item names no
application allowed to read it, so every read, the release tool's own
included, asks for that password: a process running as the operator, such
as a dependency's build script during `just deploy`, cannot take the seed
without a prompt. A deploy asks twice: once to unlock the keychain, once to
let `security` read the item. Answer the second with **Allow**, never
**Always Allow**, which would add `security` to the item's readers for good.

What the key does not vouch for is the build itself. The binaries are
built by the Release workflow on GitHub's runners, and `just deploy`
signs the digests that workflow published, so a runner or release
storage compromised between the build and the deploy gets its binary
signed. That is the trust kept deliberately: the window is the one
release being cut, the deploy is an act someone runs by hand, and the
alternative is building every target on the release Mac. Should that
window ever need closing, GitHub's artifact attestations would let the
deploy prove each asset came from the workflow at the tagged commit
before signing it, leaving only a compromised runner.

`just release-key generate` makes a seed from the system's randomness,
makes the keychain when there is none (asking for its new password), stores
the seed and prints the public half; it refuses when an item is already
there, so a key is rotated deliberately by deleting the old item first
(`security delete-generic-password -s amux-release-key amux-release.keychain`).
A deploy names the keychain key's public half when the workflow does not list
it.

**The backup key.** A second seed, made once by `just release-key generate
--file PATH`, which writes it to a new file only its owner can read and prints its
public half. It does not stay on the Mac: it is moved somewhere offline and
the file deleted. Its public half is listed in
`AMUX_RELEASE_PUBLIC_KEYS` beside the release key's, so every binary trusts
it already. If the release key leaks or the Mac is lost, the backup signs the
release that drops the old key and adds a new one
(`just deploy VERSION --key-file PATH`), and installed machines take it
without anyone reinstalling.

Both public halves are committed as `AMUX_RELEASE_PUBLIC_KEYS` in
[`.github/workflows/release.yml`](../.github/workflows/release.yml). The
release tool refuses to cut or deploy when the workflow lists fewer than two
keys, and refuses to sign with a seed whose public half it does not list,
because the binaries would refuse the result.

Rotating the key without the backup means every installed binary stops
trusting new manifests until it has been replaced by hand with one built
under the new key: publish the last release under the old key with the new
public key compiled in, then switch.

### Deploying a release

```
just deploy 0.8.0                       # stable, every machine
just deploy 0.8.0 --channel preview     # the preview channel
just deploy 0.8.0 --rollout 10          # stable, a tenth of the machines; later --rollout 100
```

`xtask release deploy` waits until the tagged GitHub Release holds
`checksums.txt` (the workflow may still be building after a cut; it gives
up after 45 minutes), reads the checksums, signs each target's hash with
the keychain's seed (or the backup's, with `--key-file PATH`), verifies the
signature against the keys the workflow compiles in, writes the channel's manifest with each entry's URL pointing
at the release's own asset (a copy stays in `target/release-manifests/`),
and uploads `<channel>.json` to that same release, replacing one already
there. So:

- A staged rollout is the same command with a higher `--rollout`, which
  replaces the manifest on the release.
- Promoting a preview build to stable is `--channel stable` for the version
  the preview manifest names.
- Every manifest ever deployed stays on the release that carried it, and a
  channel's current manifest is the newest release that carries a manifest
  for that channel.

Because the key is local, deploying is done on the release Mac. Nothing in
the workflow can sign.

### What amux.sh serves

A machine reads `https://amux.sh/releases/<channel>.json`. That is a static
file: the deploy's last step runs the operator's publish script,
`~/scripts/amux-publish-release <channel> <version> <manifest> [<file>...]`,
which puts the signed manifest, and any files named after it, where amux.sh
serves them and reads each back over the public address to confirm. The
script is not in this repository; which host holds the files, where, and how
the Mac reaches it belong with the rest of the host configuration, and this
repository only says that the step happens and refuses to finish a deploy
without it, since a manifest uploaded to the release alone reaches no
machine. amux.sh never holds the key and cannot alter a manifest without the
signature failing; the worst it can do is serve a stale one or none, in
which case a supervised machine's hourly check finds nothing and
`amux update` reports it.

### Installing

`https://amux.sh/install` is the same arrangement. The scripts are
[`scripts/install/install.sh`](../scripts/install/install.sh) and
[`install.ps1`](../scripts/install/install.ps1); every stable deploy hands
them to the publish script beside the manifest, from the checkout the deploy
runs in, and amux.sh answers `/install` with the one for the caller's shell:

```sh
curl -fsSL https://amux.sh/install | sh      # macOS, Linux
irm https://amux.sh/install | iex            # Windows PowerShell
```

A script reads the stable manifest, takes the entry for the machine's target,
downloads the binary the entry names, and installs it to `~/.amux/bin` only
when the download's size and sha256 are the entry's. So the manifest is the
one statement of what a new machine runs, as it is for a running one: a
release that was cut but never deployed to stable is not installed, and a
release pulled from stable stops being installed at once. The rollout
percentage is ignored: it protects machines that already run something, and
a new machine has nothing to protect.

A script does not check the manifest's signature. It and the manifest come
from the same server, so a check there would trust the server it was
checking; the first install trusts amux.sh over TLS, and every update after
it is verified by the installed binary with the key it was built with.

The scripts parse the manifest by hand, so
[`crates/xtask/tests/install_scripts.rs`](../crates/xtask/tests/install_scripts.rs)
runs them against a manifest the release crate serialized, served from a
loopback stand-in for amux.sh: a change to the manifest's shape that would
break an installer breaks that test first.
