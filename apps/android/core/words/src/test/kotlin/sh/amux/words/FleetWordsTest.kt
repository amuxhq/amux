package sh.amux.words

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.push.Said
import sh.amux.push.Wants
import sh.amux.values.Activity
import sh.amux.values.ActivityKind
import sh.amux.values.ActivityLine
import sh.amux.values.Comparison
import sh.amux.values.ExitCause
import sh.amux.values.GroupState
import sh.amux.values.Kind
import sh.amux.values.SecondLine
import sh.amux.values.UsageLabel
import sh.amux.values.UsageState
import sh.amux.values.UsageWindowView
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.Locale

class FleetWordsTest {
    private val now: Instant = Instant.ofEpochSecond(1_790_000_000L)
    private val nowMs: Long = now.toEpochMilli()
    private val zone = ZoneOffset.UTC
    private val locale = Locale.US

    private fun said(line: SecondLine, provider: String = "Claude"): String? =
        FleetWords.secondLine(line, provider = provider, now = now, zone = zone, locale = locale)?.text

    private fun asks(wants: Wants, more: Int = 0): SecondLine =
        SecondLine.Said(Said.Ask(key = "k", wants = wants, why = "", more = more))

    @Test
    fun anAskSaysWhatItAsksAndHowManyMoreWait() {
        assertEquals("Wants to run cargo test", said(asks(Wants.Run("cargo test\n--all"))))
        assertEquals(
            "Wants to create src/a.rs · 2 more",
            said(asks(Wants.Change("src/a.rs", files = 1, created = true), more = 2)),
        )
        assertEquals(
            "Wants to write src/a.rs",
            said(asks(Wants.Change("src/a.rs", files = 1, created = null))),
        )
        assertEquals("Has a plan for you to decide", said(asks(Wants.Plan(""))))
        assertEquals("Which base?", said(asks(Wants.Question("Which base?"))))
        assertEquals("linear: Sign in", said(asks(Wants.Other("linear", "Sign in\nnow"))))
        assertEquals(
            FleetWords.Ink.Ask,
            FleetWords.secondLine(asks(Wants.Plan("")), provider = "Codex", now = now)?.ink,
        )
    }

    @Test
    fun backgroundJobsLeadTheLine() {
        fun withJobs(line: SecondLine, jobs: Int): String? =
            FleetWords.secondLine(line, provider = "Codex", now = now, backgroundJobs = jobs)?.text

        assertEquals("1 background job · Started the server.", withJobs(SecondLine.LastSaid("Started the server."), 1))
        assertEquals("3 background jobs running", withJobs(SecondLine.Blank, 3))
        val thinking = Activity(ActivityKind.Thinking, sinceMs = 0L, elapsedMs = 0L, turnElapsedMs = 0L)
        val step = SecondLine.Step(ActivityLine(thinking, step = null, subagents = null))
        assertEquals("2 background jobs · Thinking", withJobs(step, 2))
        assertEquals("Has a plan for you to decide", withJobs(asks(Wants.Plan("")), 2))
        assertEquals("Done.", withJobs(SecondLine.LastSaid("Done."), 0))
    }

    @Test
    fun aWorkingAgentNamesItsStep() {
        val running = Activity(ActivityKind.Running("s1"), sinceMs = 0L, elapsedMs = 0L, turnElapsedMs = 0L)
        assertEquals("npm test", said(SecondLine.Step(ActivityLine(running, step = "npm test\nmore", subagents = null))))
        val thinking = Activity(ActivityKind.Thinking, sinceMs = 0L, elapsedMs = 0L, turnElapsedMs = 0L)
        assertEquals("Thinking", said(SecondLine.Step(ActivityLine(thinking, step = null, subagents = null))))
    }

    @Test
    fun aParentSaysASubagentWaitsOnYouAsItsChatDoes() {
        val waits = Activity(ActivityKind.Subagents(3), sinceMs = 0L, elapsedMs = 0L, turnElapsedMs = 0L)
        fun live(running: Int, needsYou: Int) = GroupState(
            agents = running + needsYou,
            running = running,
            needsYou = needsYou,
            idle = 0,
            finished = 0,
            ended = 0,
            failed = 0,
            unknown = 0,
        )
        assertEquals(
            "3 subagents · 2 running · 1 waiting on you",
            said(SecondLine.Step(ActivityLine(waits, step = null, subagents = live(2, 1)))),
        )
        assertEquals(
            "3 subagents working",
            said(SecondLine.Step(ActivityLine(waits, step = null, subagents = live(3, 0)))),
        )
    }

    @Test
    fun aStuckAgentWarnsAndSaysWhenItCanGoOn() {
        val signedOut = FleetWords.secondLine(SecondLine.Said(Said.SignedOut), provider = "Codex", now = now)
        assertEquals("Signed out of Codex", signedOut?.text)
        assertEquals(FleetWords.Ink.Warning, signedOut?.ink)
        assertEquals("Signed out of Claude", said(SecondLine.Said(Said.SignedOut)))
        assertEquals("Signed out of OpenCode", said(SecondLine.Said(Said.SignedOut), provider = "OpenCode"))
        assertEquals("Usage limit reached", said(SecondLine.Said(Said.UsageLimit(null))))
        assertEquals("Usage limit reached", said(SecondLine.Said(Said.UsageLimit(nowMs - 1L))))
        assertTrue(said(SecondLine.Said(Said.UsageLimit(nowMs + 3_600_000L)))!!.startsWith("Usage limit reached · resets "))
        val failed = FleetWords.secondLine(SecondLine.Said(Said.Failed("Connection refused\nretry")), provider = "Codex", now = now)
        assertEquals("Connection refused", failed?.text)
        assertEquals(FleetWords.Ink.Warning, failed?.ink)
        val revoked = FleetWords.secondLine(SecondLine.HostRevoked, provider = "Claude", now = now)
        assertEquals("Unpaired by its device · pair again to reach it", revoked?.text)
        assertEquals(FleetWords.Ink.Warning, revoked?.ink)
    }

    @Test
    fun whatTheStateWordAlreadySaysIsNotSaidAgain() {
        assertNull(said(SecondLine.Exited(ExitCause.Finished)))
        assertNull(said(SecondLine.Exited(ExitCause.Ended)))
        assertNull(said(SecondLine.Blank))
        assertNull(said(SecondLine.LastSaid("")))
        val failed = FleetWords.secondLine(SecondLine.Exited(ExitCause.Failed("code 1")), provider = "Codex", now = now)
        assertEquals("Stopped · code 1", failed?.text)
        assertEquals(FleetWords.Ink.Error, failed?.ink)
        assertEquals("Done: the tests pass.", said(SecondLine.LastSaid("Done: the tests pass.")))
    }

    @Test
    fun theOverviewWordsUsageAndChanges() {
        val resetInstant = now.plusSeconds(4L * 86_400L)
        val near = UsageWindowView(
            label = UsageLabel.Weekly(model = "Fable"),
            state = UsageState.NearLimit,
            resetsAtMs = resetInstant.toEpochMilli(),
            usedPercent = 83.6,
        )
        assertEquals("Fable weekly limit", ChatWords.usageLabel(near.label))
        assertEquals("84% used", ChatWords.used(near))
        val weekday = DateTimeFormatter.ofPattern("EEE", locale).format(resetInstant.atZone(zone))
        assertEquals("near · resets $weekday", ChatWords.usageDetail(near, now = now, zone = zone, locale = locale))
        val unknown = UsageWindowView(UsageLabel.FiveHour, UsageState.Unspecified, resetsAtMs = null, usedPercent = 10.0)
        assertNull(ChatWords.usageDetail(unknown, now = now))
        assertEquals("Uncommitted", ChatWords.comparison(Comparison.Uncommitted, base = "main"))
        assertEquals("vs main", ChatWords.comparison(Comparison.OnBranch, base = "main"))
        assertEquals("On branch", ChatWords.comparison(Comparison.OnBranch, base = null))
        assertEquals("+25 \u22128", ChatWords.counts(added = 25, removed = 8))
        assertEquals("\u221212", ChatWords.counts(added = 0, removed = 12))
        assertEquals("14m", ChatWords.ran(sinceMs = nowMs - 14L * 60_000L, now = now))
    }
}
