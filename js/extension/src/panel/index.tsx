// The side panel: the tab's pending comments, where they go, and Send, the
// shape of the diff review page. It follows the active tab of its window.

import { render } from "preact";
import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import {
  type AgentChoice,
  byPage,
  type Destination,
  type FromPanel,
  type PanelState,
  type PendingComment,
  type Reach,
  type ToPanel,
} from "../shared/messages.ts";
import { Banner, Button, type ChoiceGroup, Icons, Kbd, Picker, Textarea } from "./ui.tsx";

const port = chrome.runtime.connect({ name: "panel" });
const post = (message: FromPanel) => port.postMessage(message);
const isMac = navigator.platform.toLowerCase().includes("mac");
const shortcut = isMac ? "⌘⇧." : "Ctrl+Shift+.";

/**
 * The tab a panel opened as an ordinary page shows, named by `?tab=`: in its
 * own tab the active tab is the panel itself. The end-to-end test opens it so.
 */
const pinned = Number(new URLSearchParams(location.search).get("tab")) || null;

async function activeTab(): Promise<number | null> {
  if (pinned !== null) return pinned;
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  return tab?.id ?? null;
}

/** A folder's path as far as tells it apart: its last three parts. */
function shortPath(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts.length > 3 ? `…/${parts.slice(-3).join("/")}` : path;
}

function same(a: Destination | null, b: Destination | null): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function App() {
  const [state, setState] = useState<PanelState | null>(null);
  const [destination, setDestination] = useState<Destination | null>(null);
  const [message, setMessage] = useState("");
  const chosen = useRef(false);

  useEffect(() => {
    const listen = (incoming: ToPanel) => {
      if (incoming.type === "state") setState(incoming.state);
    };
    port.onMessage.addListener(listen);
    let shown: number | null = null;
    const follow = async () => {
      const tabId = await activeTab();
      if (tabId === null || tabId === shown) return;
      // Another tab's review: its own destination and words, not these.
      if (shown !== null) {
        chosen.current = false;
        setMessage("");
      }
      shown = tabId;
      post({ type: "watch", tabId });
    };
    void follow();
    const activated = () => void follow();
    chrome.tabs.onActivated.addListener(activated);
    chrome.windows.onFocusChanged.addListener(activated);
    return () => {
      port.onMessage.removeListener(listen);
      chrome.tabs.onActivated.removeListener(activated);
      chrome.windows.onFocusChanged.removeListener(activated);
    };
  }, []);

  useEffect(() => {
    if (!state || chosen.current) return;
    if (!same(destination, state.suggested)) setDestination(state.suggested);
  }, [state?.suggested]);

  if (!state) return <main class="panel" />;
  const tabId = state.tabId;
  const { review } = state;
  const items = review.comments.length + review.snippets.length;

  return (
    <main class="panel">
      <header>
        <img src="icons/icon-32.png" alt="" width={18} height={18} />
        <h1>Page review</h1>
        <Status reach={state.reach} />
      </header>
      <div class="scroll">
        <Problem state={state} />
        {state.outcome && "sent" in state.outcome && <Banner tone="success">Sent to {state.outcome.sent}</Banner>}
        {state.outcome && "failed" in state.outcome && <Banner tone="error">Not sent: {state.outcome.failed}</Banner>}
        {items === 0 ? <Empty state={state} /> : <Comments state={state} />}
        {items > 0 && state.blocked === null && !state.reviewing && review.comments.length > 0 && !state.needsGrant && (
          <Button
            variant="ghost"
            size="sm"
            class="add-more"
            onClick={() => tabId !== null && post({ type: "review", tabId })}
          >
            <Icons.plus /> Add more on the page
          </Button>
        )}
        {items > 0 && state.needsGrant && !state.reviewing && state.blocked === null && (
          <Banner tone="notice">
            To add more, click the amux button in the toolbar or press <Kbd>{shortcut}</Kbd> while you’re on the page.
          </Banner>
        )}
      </div>
      <Composer
        state={state}
        destination={destination}
        onDestination={(next) => {
          chosen.current = true;
          setDestination(next);
        }}
        message={message}
        onMessage={setMessage}
        onSend={() => {
          if (tabId === null || !destination) return;
          post({ type: "send", tabId, destination, message });
          setMessage("");
        }}
      />
    </main>
  );
}

function Comments({ state }: { state: PanelState }) {
  const { tabId, review } = state;
  const pages = byPage(review.comments);
  const row = (comment: PendingComment, index: number) => (
    <li key={comment.id} class="row">
      <button
        type="button"
        class="row-main"
        title="Show it on the page"
        onClick={() => tabId !== null && post({ type: "focus", tabId, id: comment.id })}
      >
        <span class="number">{index + 1}</span>
        <span class="row-body">
          <span class="row-target">
            {comment.quote
              ? `“${comment.quote}”`
              : comment.region
                ? "Box"
                : comment.element.text || comment.element.selector}
          </span>
          {comment.text ? <span class="row-text">{comment.text}</span> : <span class="row-text none">No comment</span>}
          {state.thumbnails[comment.id] && <img class="thumbnail" src={state.thumbnails[comment.id]} alt="" />}
        </span>
      </button>
      <Button
        variant="ghost"
        size="icon"
        class="row-remove"
        aria-label="Delete comment"
        onClick={() => tabId !== null && post({ type: "delete", tabId, id: comment.id })}
      >
        <Icons.close />
      </Button>
    </li>
  );
  return (
    <ol class="rows">
      {pages.length === 1
        ? pages[0]!.comments.map(row)
        : pages.map((page) => [
            <li key={page.url} class="section" title={page.url}>
              {page.title || page.url}
            </li>,
            ...page.comments.map(row),
          ])}
      {review.snippets.map((snippet) => (
        <li key={snippet.id} class="row">
          <div class="row-main">
            <span class="number snippet">¶</span>
            <span class="row-body">
              <span class="row-target">{snippet.name}</span>
              <span class="row-text">{snippet.text.split("\n")[0]}</span>
            </span>
          </div>
          <Button
            variant="ghost"
            size="icon"
            class="row-remove"
            aria-label="Remove"
            onClick={() => tabId !== null && post({ type: "deleteSnippet", tabId, id: snippet.id })}
          >
            <Icons.close />
          </Button>
        </li>
      ))}
    </ol>
  );
}

function Status({ reach }: { reach: Reach }) {
  const [words, kind] =
    reach.status === "ok"
      ? [`amux ${reach.version}`, "ok"]
      : reach.status === "connecting"
        ? ["Connecting…", "waiting"]
        : ["Not connected", "problem"];
  return (
    <span class={`status ${kind}`} title={words}>
      <span class="dot" />
      {words}
    </span>
  );
}

function Problem({ state }: { state: PanelState }) {
  const { reach, blocked } = state;
  if (reach.status === "not-installed") {
    return (
      <Banner tone="notice">
        amux isn’t set up for this browser yet. Install or update amux from{" "}
        <a href="https://amux.sh" target="_blank" rel="noopener">
          amux.sh
        </a>
        , then start it: it registers itself with your browsers when it starts.
      </Banner>
    );
  }
  if (reach.status === "not-running") {
    return (
      <Banner tone="notice">
        amux isn’t running. Open the amux app, or run <code>amux server start</code>.
      </Banner>
    );
  }
  if (reach.status === "stopped") {
    return <Banner tone="error">amux’s connection stopped{reach.detail ? `: ${reach.detail}` : "."}</Banner>;
  }
  if (blocked === "browser-page") {
    return <Banner tone="notice">The browser’s own pages can’t be reviewed. Open a website or a local file.</Banner>;
  }
  if (blocked === "file-access") {
    return (
      <Banner tone="notice">
        <p>To review local files, turn on “Allow access to file URLs” for amux.</p>
        <Button size="sm" onClick={() => post({ type: "fileAccess" })}>
          Open amux’s settings
        </Button>
      </Banner>
    );
  }
  return null;
}

/** The three gestures, as the overlay's toolbar names them. */
function Gestures() {
  return (
    <ul class="gestures">
      <li>
        <span class="gesture-icon">
          <Icons.block />
        </span>
        <span>
          <strong>Click a block</strong> to comment on all of it
        </span>
      </li>
      <li>
        <span class="gesture-icon">
          <Icons.text />
        </span>
        <span>
          <strong>Select text</strong> to comment on a phrase
        </span>
      </li>
      <li>
        <span class="gesture-icon">
          <Icons.box />
        </span>
        <span>
          <strong>Drag a box</strong> to send a picture of a region
        </span>
      </li>
    </ul>
  );
}

function Empty({ state }: { state: PanelState }) {
  if (state.blocked !== null) return null;
  if (state.reviewing) {
    return (
      <div class="empty">
        <h2>Reviewing this page</h2>
        <Gestures />
        <p class="muted">
          Hold <Kbd>Shift</Kbd> to use the page normally. Press <Kbd>Esc</Kbd> when you’re done.
        </p>
      </div>
    );
  }
  if (state.needsGrant) {
    return (
      <div class="empty">
        <h2>Comment on this page</h2>
        <Banner tone="notice">
          Click the amux button in the toolbar, or press <Kbd>{shortcut}</Kbd>, while you’re on this page. The panel
          can’t start a review by itself on a page you haven’t opened amux on.
        </Banner>
        <Gestures />
      </div>
    );
  }
  return (
    <div class="empty">
      <h2>Comment on this page</h2>
      <Gestures />
      <div class="empty-actions">
        <Button variant="primary" onClick={() => state.tabId !== null && post({ type: "review", tabId: state.tabId })}>
          Review this page
        </Button>
        <span class="muted">
          or <Kbd>{shortcut}</Kbd>
        </span>
      </div>
      <p class="muted">Your comments go to an agent together, like a review of its changes.</p>
    </div>
  );
}

interface ComposerProps {
  state: PanelState;
  destination: Destination | null;
  onDestination(destination: Destination | null): void;
  message: string;
  onMessage(message: string): void;
  onSend(): void;
}

function Composer({ state, destination, onDestination, message, onMessage, onSend }: ComposerProps) {
  const { review } = state;
  const count = review.comments.length + review.snippets.length;
  const groups = useMemo((): ChoiceGroup[] => {
    const many = state.profiles.length > 1;
    const label = (profile: string) => state.profiles.find((candidate) => candidate.id === profile)?.label ?? "";
    const agents: ChoiceGroup[] = state.profiles.map((profile) => ({
      label: many ? profile.label : "Agents",
      choices: state.agents
        .filter((agent) => agent.profile === profile.id && !agent.closed)
        .map((agent: AgentChoice) => ({
          value: JSON.stringify({ kind: "agent", profile: agent.profile, host: agent.host, agent: agent.agent }),
          label: agent.name,
          detail: [agent.repository || agent.cwd, agent.local ? "" : agent.hostName].filter(Boolean).join(" · "),
          badge: agent.needsYou ? "Needs you" : undefined,
        })),
    }));
    const folders: ChoiceGroup = {
      label: "New agent in",
      choices: state.folders.map((folder) => ({
        value: JSON.stringify({ kind: "new", profile: folder.profile, cwd: folder.path }),
        label: folder.name || folder.path,
        detail: [shortPath(folder.path), many ? label(folder.profile) : ""].filter(Boolean).join(" · "),
        icon: "plus" as const,
      })),
    };
    return [...agents, folders].filter((group) => group.choices.length > 0);
  }, [state.agents, state.profiles, state.folders]);
  if (count === 0 || state.reach.status !== "ok") return null;
  const noun = review.comments.length === 1 ? "comment" : "comments";
  const label =
    review.comments.length > 0
      ? `Send ${review.comments.length} ${noun}`
      : review.snippets.length === 1
        ? "Send"
        : `Send ${review.snippets.length}`;
  return (
    <form
      class="composer"
      onSubmit={(event) => {
        event.preventDefault();
        onSend();
      }}
    >
      <Picker
        groups={groups}
        value={destination ? JSON.stringify(destination) : null}
        placeholder="Choose an agent…"
        search="Find an agent or folder"
        onChange={(value) => onDestination(JSON.parse(value) as Destination)}
      />
      <Textarea
        rows={2}
        placeholder="What should the agent do with these? (optional)"
        value={message}
        onInput={(event) => onMessage((event.target as HTMLTextAreaElement).value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
            event.preventDefault();
            onSend();
          }
        }}
      />
      <div class="send-row">
        <span class="muted">
          <Kbd>{isMac ? "⌘↩" : "Ctrl+↩"}</Kbd> to send
        </span>
        <Button variant="primary" type="submit" disabled={!destination || state.sending}>
          {state.sending ? "Sending…" : label}
        </Button>
      </div>
    </form>
  );
}

render(<App />, document.getElementById("app")!);
