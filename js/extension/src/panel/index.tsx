// The side panel: the tab's pending comments, where they go, and Send, the
// shape of the diff review page. It follows the active tab of its window.

import { render } from "preact";
import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import {
  byPage,
  type AgentChoice,
  type Destination,
  type FromPanel,
  type PanelState,
  type PendingComment,
  type Reach,
  type ToPanel,
} from "../shared/messages.ts";

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
    const follow = async () => {
      const tabId = await activeTab();
      if (tabId !== null) post({ type: "watch", tabId });
    };
    void follow();
    const activated = () => {
      chosen.current = false;
      setMessage("");
      void follow();
    };
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
  const pages = byPage(review.comments);
  // Numbered as each page's pins are; a page heading only once there are several.
  const comment = (comment: PendingComment, index: number) => (
    <li key={comment.id}>
      <button
        type="button"
        class="comment"
        title="Show it on the page"
        onClick={() => tabId !== null && post({ type: "focus", tabId, id: comment.id })}
      >
        <span class="number">{index + 1}</span>
        <span class="body">
          <span class="target">
            {comment.quote ? `“${comment.quote}”` : comment.region ? "Box" : comment.element.text || comment.element.selector}
          </span>
          {comment.text ? <span class="text">{comment.text}</span> : <span class="text none">No comment</span>}
          {state.thumbnails[comment.id] && <img class="thumbnail" src={state.thumbnails[comment.id]} alt="" />}
        </span>
      </button>
      <button
        type="button"
        class="remove"
        aria-label="Delete comment"
        onClick={() => tabId !== null && post({ type: "delete", tabId, id: comment.id })}
      >
        ×
      </button>
    </li>
  );

  return (
    <main class="panel">
      <header>
        <img src="icon.png" alt="" width={18} height={18} />
        <h1>Page review</h1>
        <Status reach={state.reach} />
      </header>
      <div class="scroll">
      <Problem state={state} />
      {state.outcome && "sent" in state.outcome && (
        <p class="outcome sent" role="status">
          <span class="glyph">✓</span> Sent to {state.outcome.sent}
        </p>
      )}
      {state.outcome && "failed" in state.outcome && (
        <p class="outcome failed" role="alert">
          <span class="glyph">!</span> Not sent: {state.outcome.failed}
        </p>
      )}
      {review.comments.length === 0 && review.snippets.length === 0 ? (
        <Empty state={state} />
      ) : (
        <ol class="comments">
          {pages.length === 1
            ? pages[0]!.comments.map(comment)
            : pages.map((page) => [
                <li key={page.url} class="page" title={page.url}>
                  {page.title || page.url}
                </li>,
                ...page.comments.map(comment),
              ])}
          {review.snippets.map((snippet) => (
            <li key={snippet.id}>
              <div class="comment">
                <span class="number snippet">¶</span>
                <span class="body">
                  <span class="target">{snippet.name}</span>
                  <span class="text">{snippet.text.split("\n")[0]}</span>
                </span>
              </div>
              <button
                type="button"
                class="remove"
                aria-label="Remove"
                onClick={() => tabId !== null && post({ type: "deleteSnippet", tabId, id: snippet.id })}
              >
                ×
              </button>
            </li>
          ))}
        </ol>
      )}
      {items > 0 && !state.reviewing && state.blocked === null && review.comments.length > 0 && !state.needsGrant && (
        <button type="button" class="link" onClick={() => tabId !== null && post({ type: "review", tabId })}>
          Add more on the page
        </button>
      )}
      {state.needsGrant && !state.reviewing && state.blocked === null && items > 0 && (
        <p class="notice">
          To add more, click the amux button in the toolbar or press <kbd>{shortcut}</kbd> while you’re on the page.
        </p>
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
      <p class="notice">
        amux isn’t set up for this browser yet. Install or update amux from <a href="https://amux.sh" target="_blank">amux.sh</a>, then start it: it
        registers itself with your browsers when it starts.
      </p>
    );
  }
  if (reach.status === "not-running") {
    return (
      <p class="notice">
        amux isn’t running. Open the amux app, or run <code>amux server start</code>.
      </p>
    );
  }
  if (reach.status === "stopped") {
    return <p class="notice">amux’s connection stopped{reach.detail ? `: ${reach.detail}` : "."}</p>;
  }
  if (blocked === "browser-page") {
    return <p class="notice">The browser’s own pages can’t be reviewed. Open a website or a local file.</p>;
  }
  if (blocked === "file-access") {
    return (
      <div class="notice">
        <p>To review local files, turn on “Allow access to file URLs” for amux.</p>
        <button type="button" onClick={() => post({ type: "fileAccess" })}>
          Open amux’s settings
        </button>
      </div>
    );
  }
  return null;
}

const ICON = { width: 16, height: 16, viewBox: "0 0 16 16", fill: "none", stroke: "currentColor", "stroke-width": 1.5 };

/** A block with a pointer on it. */
const BlockIcon = () => (
  <svg {...ICON} aria-hidden="true">
    <rect x="1.75" y="3.25" width="12.5" height="7.5" rx="1.5" />
    <path d="M9 8.5l4 5.5-1.6.2-.9 1.5z" fill="currentColor" stroke="none" />
  </svg>
);

/** Lines of text with one picked out. */
const TextIcon = () => (
  <svg {...ICON} aria-hidden="true" stroke-linecap="round">
    <path d="M2 4h12M2 12h7" />
    <rect x="1.5" y="6.25" width="9" height="3.5" rx="1" fill="currentColor" stroke="none" opacity="0.35" />
    <path d="M3 8h6" />
  </svg>
);

/** A dashed region. */
const BoxIcon = () => (
  <svg {...ICON} aria-hidden="true" stroke-dasharray="2.5 2">
    <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
  </svg>
);

/** The three gestures, as the overlay's toolbar names them. */
function Gestures() {
  return (
    <ul class="gestures">
      <li>
        <span class="gesture-glyph">
          <BlockIcon />
        </span>
        <span>
          <strong>Click a block</strong> to comment on all of it
        </span>
      </li>
      <li>
        <span class="gesture-glyph">
          <TextIcon />
        </span>
        <span>
          <strong>Select text</strong> to comment on a phrase
        </span>
      </li>
      <li>
        <span class="gesture-glyph">
          <BoxIcon />
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
          Hold <kbd>Shift</kbd> to use the page normally. Press <kbd>Esc</kbd> when you’re done.
        </p>
      </div>
    );
  }
  if (state.needsGrant) {
    return (
      <div class="empty">
        <h2>Comment on this page</h2>
        <p class="notice">
          Click the amux button in the toolbar, or press <kbd>{shortcut}</kbd>, while you’re on this page. The panel
          can’t start a review by itself on a page you haven’t opened amux on.
        </p>
        <Gestures />
      </div>
    );
  }
  return (
    <div class="empty">
      <h2>Comment on this page</h2>
      <Gestures />
      <button
        type="button"
        class="primary"
        onClick={() => state.tabId !== null && post({ type: "review", tabId: state.tabId })}
      >
        Review this page
      </button>
      <p class="muted">
        Or press <kbd>{shortcut}</kbd>. Your comments go to an agent together, like a review of its changes.
      </p>
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
  const groups = useMemo(() => {
    const byProfile = new Map<string, { label: string; agents: AgentChoice[] }>();
    for (const profile of state.profiles) byProfile.set(profile.id, { label: profile.label, agents: [] });
    for (const agent of state.agents) {
      if (agent.closed) continue;
      byProfile.get(agent.profile)?.agents.push(agent);
    }
    return [...byProfile.entries()];
  }, [state.agents, state.profiles]);
  if (count === 0 || state.reach.status !== "ok") return null;
  const many = state.profiles.length > 1;
  const value = destination ? JSON.stringify(destination) : "";
  const noun = review.comments.length > 0 ? (review.comments.length === 1 ? "comment" : "comments") : "";
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
      <label>
        <span>To</span>
        <select
          value={value}
          onChange={(event) => {
            const picked = (event.target as HTMLSelectElement).value;
            onDestination(picked ? (JSON.parse(picked) as Destination) : null);
          }}
        >
          <option value="">Choose an agent…</option>
          {groups.map(([id, group]) => (
            <optgroup key={id} label={many ? group.label : "Agents"}>
              {group.agents.map((agent) => (
                <option
                  key={agent.agent}
                  value={JSON.stringify({ kind: "agent", profile: agent.profile, host: agent.host, agent: agent.agent })}
                >
                  {agent.name}
                  {agent.needsYou ? " · needs you" : ""}
                  {agent.repository ? ` · ${agent.repository}` : ""}
                  {agent.local ? "" : ` · ${agent.hostName}`}
                </option>
              ))}
            </optgroup>
          ))}
          {state.folders.length > 0 && (
            <optgroup label="New agent in">
              {state.folders.map((folder) => (
                <option
                  key={`${folder.profile}:${folder.path}`}
                  value={JSON.stringify({ kind: "new", profile: folder.profile, cwd: folder.path })}
                >
                  {folder.name || folder.path}
                  {many ? ` · ${state.profiles.find((profile) => profile.id === folder.profile)?.label ?? ""}` : ""}
                </option>
              ))}
            </optgroup>
          )}
        </select>
      </label>
      <textarea
        rows={3}
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
          <kbd>{isMac ? "⌘↩" : "Ctrl+↩"}</kbd>
        </span>
        <button type="submit" class="primary" disabled={!destination || state.sending}>
          {state.sending ? "Sending…" : label}
        </button>
      </div>
    </form>
  );
}

render(<App />, document.getElementById("app")!);
